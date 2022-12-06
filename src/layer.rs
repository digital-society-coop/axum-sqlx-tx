//! A [`tower_layer::Layer`] that enables the [`Tx`](crate::Tx) extractor.

use futures_core::future::BoxFuture;

use crate::{tx::TxSlot, TxLayerError};

/// A [`tower_layer::Layer`] that enables the [`Tx`] extractor.
///
/// This layer adds a lazily-initialised transaction to the [request extensions]. The first time the
/// [`Tx`] extractor is used on a request, a connection is acquired from the configured
/// [`sqlx::Pool`] and a transaction is started on it. The same transaction will be returned for
/// subsequent uses of [`Tx`] on the same request. The inner service is then called as normal. Once
/// the inner service responds, the transaction is committed or rolled back depending on the status
/// code of the response.
///
/// [`Tx`]: crate::Tx
/// [request extensions]: https://docs.rs/http/latest/http/struct.Extensions.html
pub struct Layer<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
}

impl<DB: sqlx::Database> Clone for Layer<DB> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl<DB: sqlx::Database> Layer<DB> {
    /// Construct a new layer with the given `pool`.
    ///
    /// A connection will be obtained from the pool the first time a [`Tx`](crate::Tx) is extracted
    /// from a request.
    ///
    /// If you want to access the pool outside of a transaction, you should add it also with
    /// [`axum::Extension`].
    ///
    /// [`axum::Extension`]: https://docs.rs/axum/latest/axum/extract/struct.Extension.html
    pub fn new(pool: sqlx::Pool<DB>) -> Self {
        Layer { pool }
    }
}

impl<DB: sqlx::Database, S> tower_layer::Layer<S> for Layer<DB> {
    type Service = Service<DB, S>;

    fn layer(&self, inner: S) -> Self::Service {
        Service {
            pool: self.pool.clone(),
            inner,
        }
    }
}

/// A [`tower_service::Service`] that enables the [`Tx`](crate::Tx) extractor.
///
/// See [`Layer`] for more information.
pub struct Service<DB: sqlx::Database, S> {
    pool: sqlx::Pool<DB>,
    inner: S,
}

// can't simply derive because `DB` isn't `Clone`
impl<DB: sqlx::Database, S: Clone> Clone for Service<DB, S> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<DB: sqlx::Database, S, ReqBody, ResBody> tower_service::Service<http::Request<ReqBody>>
    for Service<DB, S>
where
    S: tower_service::Service<
        http::Request<ReqBody>,
        Response = http::Response<ResBody>,
        Error = std::convert::Infallible,
    >,
    S::Future: Send + 'static,
    ResBody: Send,
{
    type Response = S::Response;
    type Error = TxLayerError;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|err| match err {})
    }

    fn call(&mut self, mut req: http::Request<ReqBody>) -> Self::Future {
        let transaction = TxSlot::bind(req.extensions_mut(), self.pool.clone());

        let res = self.inner.call(req);

        Box::pin(async move {
            let res = res.await.unwrap(); // inner service is infallible

            if res.status().is_success() {
                transaction.commit().await?;
            }

            Ok(res)
        })
    }
}

#[cfg(test)]
mod tests {
    use axum::error_handling::HandleErrorLayer;
    use tower::ServiceBuilder;

    use super::Layer;

    // The trait shenanigans required by axum for layers are significant, so this "test" ensures
    // we've got it right.
    #[allow(unused, unreachable_code, clippy::diverging_sub_expression)]
    fn layer_compiles() {
        let pool: sqlx::Pool<sqlx::Sqlite> = todo!();

        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "hello" }))
            .layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(|err| async move {
                        println!("Error while committing the transaction: {err:?}");

                        (
                            http::StatusCode::INTERNAL_SERVER_ERROR,
                            "internal server error",
                        )
                    }))
                    .layer(Layer::new(pool)),
            );

        axum::Server::bind(todo!()).serve(app.into_make_service());
    }
}
