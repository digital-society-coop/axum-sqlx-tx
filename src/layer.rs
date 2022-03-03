//! A [`tower_layer::Layer`] that enables the [`Tx`](crate::Tx) extractor.

use futures::future::BoxFuture;

use crate::{tx::TxSlot, Error};

/// A [`tower_layer::Layer`] that enables the [`Tx`](crate::Tx) extractor.
///
/// This adds a value to request extensions that the [`Tx`](crate::Tx) extractor
/// depends on.
pub struct Layer<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
}

impl<DB: sqlx::Database> Layer<DB> {
    /// Construct a new layer with the given `pool`.
    ///
    /// A connection will be obtained from the pool the first time a [`Tx`](crate::Tx) is extracted
    /// from a request.
    ///
    /// If you want to access the pool outside of a transaction, you should add it also with
    /// `axum::AddExtensionLayer`.
    pub fn new(pool: sqlx::Pool<DB>) -> Self {
        Self { pool }
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
/// This is constructed by [`Layer`].
pub struct Service<DB: sqlx::Database, S> {
    pool: sqlx::Pool<DB>,
    inner: S,
}

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
    type Error = Error;
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

    use crate::Error;

    use super::Layer;

    // The trait shenanigans required by axum for layers are significant, so this "test" ensures
    // we've got it right.
    #[allow(unused, unreachable_code, clippy::diverging_sub_expression)]
    fn layer_compiles() {
        let pool: sqlx::Pool<sqlx::Postgres> = todo!();

        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "hello" }))
            .layer(
                ServiceBuilder::new()
                    .layer(HandleErrorLayer::new(|_: Error| async {}))
                    .layer(Layer::new(pool)),
            );

        axum::Server::bind(todo!()).serve(app.into_make_service());
    }
}
