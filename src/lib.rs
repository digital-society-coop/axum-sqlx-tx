use std::{
    future::Future,
    ops::{Deref, DerefMut},
    sync::RwLock,
};

use axum::{
    extract::{FromRequest, RequestParts},
    http::{Request, StatusCode},
    response::IntoResponse,
    routing::Route,
};
use futures::{channel::oneshot, future::BoxFuture};

#[cfg(feature = "postgres")]
mod db;

/// A [`tower::Layer`] that enables the [`Connection`] and [`Transaction`] extractors.
pub struct Layer<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
}

impl<DB: sqlx::Database> Layer<DB> {
    /// Construct a new layer with the given `pool`.
    ///
    /// `pool` will be used to acquire a connection for each request.
    pub fn new(pool: sqlx::Pool<DB>) -> Self {
        Self { pool }
    }
}

impl<DB: sqlx::Database, B> tower::Layer<Route<B>> for Layer<DB> {
    type Service = Service<DB, B>;

    fn layer(&self, inner: Route<B>) -> Self::Service {
        Service {
            pool: self.pool.clone(),
            inner,
        }
    }
}

/// A [`tower::Service`] that enables the [`Connection`] and [`Transaction`] extractors.
#[derive(Debug)]
pub struct Service<DB: sqlx::Database, B> {
    pool: sqlx::Pool<DB>,
    inner: Route<B>,
}

impl<DB: sqlx::Database, B> Clone for Service<DB, B> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<DB: sqlx::Database, B> tower::Service<Request<B>> for Service<DB, B>
where
    B: Send + 'static,
{
    type Response = <Route<B> as tower::Service<Request<B>>>::Response;
    type Error = <Route<B> as tower::Service<Request<B>>>::Error;
    type Future =
        BoxFuture<'static, <<Route<B> as tower::Service<Request<B>>>::Future as Future>::Output>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut req: Request<B>) -> Self::Future {
        let (tx, mut rx) = oneshot::channel();
        let ext = Ext::Idle {
            pool: self.pool.clone(),
            back: Some(tx),
        };
        let mut inner = self.inner.clone();
        Box::pin(async move {
            req.extensions_mut().insert(ext);
            let res = RwLock::new(inner.call(req).await);

            // See if the transaction extractor was used
            if let Some(transaction) = rx.try_recv().ok().flatten() {
                let success = {
                    let res = res.read().unwrap();
                    res.as_ref()
                        .map(|res| res.status().is_success())
                        .unwrap_or(false)
                };
                if success {
                    println!("committing!");
                    if let Err(error) = transaction.commit().await {
                        return Ok(Error::Database(error).into_response());
                    }
                }
            }

            res.into_inner().unwrap()
        })
    }
}

enum Ext<DB: sqlx::Database> {
    Idle {
        pool: sqlx::Pool<DB>,
        back: Option<oneshot::Sender<sqlx::Transaction<'static, DB>>>,
    },
    Active {
        state: ExtState<DB>,
        back: Option<oneshot::Sender<sqlx::Transaction<'static, DB>>>,
    },
}

impl<DB: sqlx::Database> Drop for Ext<DB> {
    fn drop(&mut self) {
        if let Self::Active { state, back } = self {
            if let Some(transaction) = state.take_transaction() {
                let _ = back.take().unwrap().send(transaction);
            }
        }
    }
}

impl<DB: sqlx::Database> Ext<DB> {
    // Get an established transaction, or acquire one from the pool.
    async fn take_or_begin(
        &mut self,
    ) -> Result<
        (
            sqlx::Transaction<'static, DB>,
            oneshot::Sender<sqlx::Transaction<'static, DB>>,
        ),
        Error,
    > {
        if let Self::Idle { pool, back } = self {
            let state = pool
                .begin()
                .await
                .map_or_else(ExtState::error, ExtState::connected);
            *self = Self::Active {
                state,
                back: Some(back.take().unwrap()),
            };
        }

        match self {
            Self::Active { state, .. } => state.take_handle(),
            Self::Idle { .. } => unreachable!(),
        }
    }
}

enum ExtState<DB: sqlx::Database> {
    Connected {
        connection_slot: oneshot::Receiver<sqlx::Transaction<'static, DB>>,
    },
    Error {
        error_slot: Option<sqlx::Error>,
    },
}

impl<DB: sqlx::Database> ExtState<DB> {
    fn connected(handle: sqlx::Transaction<'static, DB>) -> Self {
        let (tx, rx) = oneshot::channel();
        tx.send(handle).unwrap();

        Self::Connected {
            connection_slot: rx,
        }
    }

    fn error(error: sqlx::Error) -> Self {
        Self::Error {
            error_slot: Some(error),
        }
    }

    fn take_handle(
        &mut self,
    ) -> Result<
        (
            sqlx::Transaction<'static, DB>,
            oneshot::Sender<sqlx::Transaction<'static, DB>>,
        ),
        Error,
    > {
        match self {
            Self::Error { error_slot } => Err(error_slot
                .take()
                .map(Error::Database)
                .unwrap_or(Error::Overlapping)),
            Self::Connected { connection_slot } => {
                // Take the value out of the receiver. There will be no value if we have multiple
                // overlapping extractors for the same request.
                let connection = connection_slot
                    .try_recv()
                    .ok()
                    .flatten()
                    .ok_or(Error::Overlapping);

                connection.map(|connection| {
                    let (tx, rx) = oneshot::channel();
                    *connection_slot = rx;
                    (connection, tx)
                })
            }
        }
    }

    fn take_transaction(&mut self) -> Option<sqlx::Transaction<'static, DB>> {
        if let Self::Connected { connection_slot } = self {
            if let Ok(Some(tx)) = connection_slot.try_recv() {
                return Some(tx);
            }
        }
        None
    }
}

/// An `axum` extractor for a database transaction.
#[derive(Debug)]
pub struct Transaction<DB: sqlx::Database> {
    // These MUST be populated until drop
    transaction: Option<sqlx::Transaction<'static, DB>>,
    connection_slot: Option<oneshot::Sender<sqlx::Transaction<'static, DB>>>,
}

impl<DB: sqlx::Database> Transaction<DB> {
    async fn extract<B>(req: &mut RequestParts<B>) -> Result<Self, Error> {
        let ext: &mut Ext<DB> = req
            .extensions_mut()
            .and_then(|ext| ext.get_mut())
            .ok_or(Error::MissingExtension)?;

        let (transaction, connection_slot) = ext.take_or_begin().await?;

        Ok(Transaction {
            transaction: Some(transaction),
            connection_slot: Some(connection_slot),
        })
    }
}

impl<DB: sqlx::Database> Drop for Transaction<DB> {
    fn drop(&mut self) {
        let transaction = self
            .transaction
            .take()
            .expect("BUG: connection empty during drop");
        let connection_slot = self
            .connection_slot
            .take()
            .expect("BUG: connection_slot empty during drop");

        // oneshot::Sender::send would fail if the receive is dropped, which in this case would mean
        // the `Ext` was dropped, in which case we have nowhere to send it anyway.
        let _ = connection_slot.send(transaction);
    }
}

impl<DB: sqlx::Database> Deref for Transaction<DB> {
    type Target = sqlx::Transaction<'static, DB>;

    fn deref(&self) -> &Self::Target {
        self.transaction.as_ref().unwrap()
    }
}

impl<DB: sqlx::Database> DerefMut for Transaction<DB> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.transaction.as_mut().unwrap()
    }
}

impl<DB: sqlx::Database, B: Send> FromRequest<B> for Transaction<DB> {
    type Rejection = Error;

    fn from_request<'r, 'f>(
        req: &'r mut RequestParts<B>,
    ) -> BoxFuture<'f, Result<Self, Self::Rejection>>
    where
        'r: 'f,
        Self: 'f,
    {
        Box::pin(async move { Self::extract(req).await })
    }
}

/// An error returned from an extractor.
#[derive(Debug)]
pub enum Error {
    MissingExtension,
    Overlapping,
    Mixed,
    Database(sqlx::Error),
}

impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let message = match self {
            Self::MissingExtension => {
                "missing extension; did you register axum_sqlx::Layer?".to_owned()
            }
            Self::Overlapping => {
                "cannot extract Connection more than once at the same time".to_owned()
            }
            Self::Mixed => {
                "cannot mix Connection and Transaction extractors for the same request".to_owned()
            }
            Self::Database(error) => error.to_string(),
        };
        (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        env,
        marker::PhantomData,
        ops::{Deref, DerefMut},
    };

    use axum::extract::FromRequest;
    use sqlx::{PgPool, Postgres};

    use crate::{Error, Layer, Transaction};

    struct Auth<E>(PhantomData<E>);

    impl<B, E, H, C> FromRequest<B> for Auth<E>
    where
        B: Send + 'static,
        E: FromRequest<B> + Deref<Target = H> + DerefMut + Send,
        E::Rejection: std::fmt::Debug + Send,
        H: Deref<Target = C> + DerefMut + Send,
        C: sqlx::Connection,
    {
        type Rejection = Error;

        fn from_request<'life0, 'async_trait>(
            req: &'life0 mut axum::extract::RequestParts<B>,
        ) -> core::pin::Pin<
            Box<
                dyn core::future::Future<Output = Result<Self, Self::Rejection>>
                    + core::marker::Send
                    + 'async_trait,
            >,
        >
        where
            'life0: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let mut c = E::from_request(req).await.unwrap();
                c.ping().await.unwrap();

                Ok(Auth(PhantomData))
            })
        }
    }

    #[tokio::test]
    #[ignore]
    async fn transaction() {
        async fn handler(
            _auth: Auth<Transaction<Postgres>>,
            mut tx: Transaction<Postgres>,
        ) -> String {
            let (message,): (String,) = sqlx::query_as("SELECT 'hello world'")
                .fetch_one(&mut tx)
                .await
                .unwrap();
            message
        }

        let pool = PgPool::connect(&env::var("DATABASE_URL").unwrap())
            .await
            .unwrap();

        let app = axum::Router::new()
            .route("/", axum::routing::get(handler))
            .route_layer(Layer::new(pool));

        let server = axum::Server::bind(&([0, 0, 0, 0], 0).into()).serve(app.into_make_service());
        println!("serving {}", server.local_addr());

        server.await.unwrap();
    }
}
