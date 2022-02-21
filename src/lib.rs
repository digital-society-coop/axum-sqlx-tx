use std::{
    future::Future,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex},
};

use axum::{
    extract::FromRequest,
    http::{Request, StatusCode},
    routing::Route,
};
use futures::future::BoxFuture;
use sqlx::{pool::PoolConnection, Pool};

#[cfg(feature = "mssql")]
mod mssql;

#[cfg(feature = "mysql")]
mod mysql;

#[cfg(feature = "postgres")]
mod postgres;

#[cfg(feature = "sqlite")]
mod sqlite;

/// A [`tower::Layer`] that enables the [`Connection`] and [`Transaction`] extractors.
pub struct Layer<DB: sqlx::Database> {
    pool: Pool<DB>,
}

impl<DB: sqlx::Database> Layer<DB> {
    /// Construct a new layer with the given `pool`.
    ///
    /// `pool` will be used to acquire a connection for each request.
    pub fn new(pool: Pool<DB>) -> Self {
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
    pool: Pool<DB>,
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
        let mut inner = self.inner.clone();
        let pool = self.pool.clone();
        Box::pin(async move {
            let connection = pool.acquire().await;
            req.extensions_mut().insert(Ext {
                connection_slot: Arc::new(Mutex::new(Some(connection))),
            });
            inner.call(req).await
        })
    }
}

type ConnectionSlot<DB> = Option<Result<PoolConnection<DB>, sqlx::Error>>;

struct Ext<DB: sqlx::Database> {
    connection_slot: Arc<Mutex<ConnectionSlot<DB>>>,
}

/// An `axum` extractor for a database connection.
///
/// The connection is taken from the request extensions, and put back on drop. This allows the
/// connection to be reused across different extractors and middleware.
///
/// To use the extractor, you must register [`Layer`] in your `axum` app.
///
/// ```
/// #[tokio::main]
/// fn main() {
///     let pool = sqlx::PgPool::connect("postgres://mydb").await.unwrap();
///
///     let app = axum::Router::new()
///         .route("/", axum::routing::get(handler))
///         .route_layer(Layer::new(pool));
///
///     axum::Server::bind(&([0, 0, 0, 0], 0).into())
///         .serve(app.into_make_service())
///         .await
///         .unwrap();
/// }
///
/// async fn handler(connection: Connection<sqlx::Postgres>) -> String {
///     sqlx::query_as("SELECT 'hello world'").await.unwrap()
/// }
/// ```
///
/// # Errors
///
/// You will get an [`Error::Concurrent`] error if you try to extract twice from the same request.
#[derive(Debug)]
pub struct Connection<DB: sqlx::Database> {
    connection_slot: Arc<Mutex<ConnectionSlot<DB>>>,

    // This must be populated, until dropped
    connection: Option<PoolConnection<DB>>,
}

impl<DB: sqlx::Database> Drop for Connection<DB> {
    fn drop(&mut self) {
        if let Ok(mut guard) = self.connection_slot.try_lock() {
            let _ = guard.insert(Ok(self.connection.take().unwrap()));
        }

        // We do nothing if we can't immediately acquire the lock, since this would indicate
        // concurrent extraction for the same request, which we don't support.
    }
}

impl<DB: sqlx::Database> Deref for Connection<DB> {
    type Target = PoolConnection<DB>;

    fn deref(&self) -> &Self::Target {
        self.connection.as_ref().unwrap()
    }
}

impl<DB: sqlx::Database> DerefMut for Connection<DB> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.connection.as_mut().unwrap()
    }
}

impl<DB: sqlx::Database, B> FromRequest<B> for Connection<DB>
where
    B: Send,
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
            let ext: &Ext<DB> = req
                .extensions()
                .and_then(|ext| ext.get())
                .ok_or(Error::MissingExtension)?;

            let connection_res = if let Ok(mut guard) = ext.connection_slot.try_lock() {
                guard.take().ok_or(Error::Concurrent)?
            } else {
                return Err(Error::Concurrent);
            };

            let connection = connection_res.map_err(Error::Database)?;

            Ok(Connection {
                connection_slot: ext.connection_slot.clone(),
                connection: Some(connection),
            })
        })
    }
}

/// An `axum` extractor for a database transaction.
#[derive(Debug)]
pub struct Transaction;

/// An error returned from an extractor.
#[derive(Debug)]
pub enum Error {
    MissingExtension,
    Concurrent,
    Database(sqlx::Error),
}

impl axum::response::IntoResponse for Error {
    fn into_response(self) -> axum::response::Response {
        let message = match self {
            Self::MissingExtension => {
                "missing extension; did you register axum_sqlx::Layer?".to_owned()
            }
            Self::Concurrent => {
                "request extractor called concurrently, this is unsupported".to_owned()
            }
            Self::Database(error) => error.to_string(),
        };
        (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
    }
}

#[cfg(test)]
mod tests {
    use std::env;

    use axum::extract::FromRequest;
    use sqlx::{Connection as _, PgPool, Postgres};

    use crate::{Connection, Error, Layer};

    struct Auth;

    impl<B> FromRequest<B> for Auth
    where
        B: Send,
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
                let mut c = Connection::<Postgres>::from_request(req).await.unwrap();
                c.ping().await.unwrap();

                Ok(Auth)
            })
        }
    }

    #[tokio::test]
    async fn test_name() {
        async fn handler(_auth: Auth, mut c: Connection<Postgres>) -> String {
            let (message,): (String,) = sqlx::query_as("SELECT 'hello world'")
                .fetch_one(&mut c)
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
