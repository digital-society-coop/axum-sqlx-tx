//! A [`tower_layer::Layer`] that enables the [`Tx`](crate::Tx) extractor.

use std::marker::PhantomData;

use axum_core::response::IntoResponse;
use bytes::Bytes;
use futures_core::future::BoxFuture;
use http_body::{combinators::UnsyncBoxBody, Body};

use crate::{tx::TxSlot, Error, State};

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
pub struct Layer<DB: sqlx::Database, E = Error> {
    state: State<DB>,
    _error: PhantomData<E>,
}

impl<DB: sqlx::Database> Layer<DB> {
    /// Construct a new layer and [`State`] with the given `pool`.
    ///
    /// A connection will be obtained from the pool the first time a [`Tx`](crate::Tx) is extracted
    /// from a request.
    ///
    /// If you want to access the pool outside of a transaction, you should add it also with
    /// [`axum::Extension`] or as router state.
    ///
    /// To use a different type than [`Error`] to convert commit errors into responses, see
    /// [`new_with_error`](Self::new_with_error).
    ///
    /// [`axum::Extension`]: https://docs.rs/axum/latest/axum/extract/struct.Extension.html
    pub fn new(pool: sqlx::Pool<DB>) -> (Self, State<DB>) {
        Self::new_with_error(pool)
    }

    /// Construct a new layer with a specific error type.
    ///
    /// See [`Layer::new`] for more information.
    pub fn new_with_error<E>(pool: sqlx::Pool<DB>) -> (Layer<DB, E>, State<DB>) {
        let state = State::new(pool);
        (
            Layer {
                state: state.clone(),
                _error: PhantomData,
            },
            state,
        )
    }
}

impl<DB: sqlx::Database, E> Clone for Layer<DB, E> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            _error: self._error,
        }
    }
}

impl<DB: sqlx::Database, S, E> tower_layer::Layer<S> for Layer<DB, E> {
    type Service = Service<DB, S, E>;

    fn layer(&self, inner: S) -> Self::Service {
        Service {
            state: self.state.clone(),
            inner,
            _error: self._error,
        }
    }
}

/// A [`tower_service::Service`] that enables the [`Tx`](crate::Tx) extractor.
///
/// See [`Layer`] for more information.
pub struct Service<DB: sqlx::Database, S, E = Error> {
    state: State<DB>,
    inner: S,
    _error: PhantomData<E>,
}

// can't simply derive because `DB` isn't `Clone`
impl<DB: sqlx::Database, S: Clone, E> Clone for Service<DB, S, E> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            inner: self.inner.clone(),
            _error: self._error,
        }
    }
}

impl<DB: sqlx::Database, S, E, ReqBody, ResBody> tower_service::Service<http::Request<ReqBody>>
    for Service<DB, S, E>
where
    S: tower_service::Service<
        http::Request<ReqBody>,
        Response = http::Response<ResBody>,
        Error = std::convert::Infallible,
    >,
    S::Future: Send + 'static,
    E: From<Error> + IntoResponse,
    ResBody: Body<Data = Bytes> + Send + 'static,
    ResBody::Error: Into<Box<dyn std::error::Error + Send + Sync + 'static>>,
{
    type Response = http::Response<UnsyncBoxBody<ResBody::Data, axum_core::Error>>;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(|err| match err {})
    }

    fn call(&mut self, mut req: http::Request<ReqBody>) -> Self::Future {
        let transaction = TxSlot::bind(req.extensions_mut(), self.state.clone());

        let res = self.inner.call(req);

        Box::pin(async move {
            let res = res.await.unwrap(); // inner service is infallible

            if !res.status().is_server_error() && !res.status().is_client_error() {
                if let Err(error) = transaction.commit().await {
                    return Ok(E::from(Error::Database { error }).into_response());
                }
            }

            Ok(res.map(|body| body.map_err(axum_core::Error::new).boxed_unsync()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Layer;

    // The trait shenanigans required by axum for layers are significant, so this "test" ensures
    // we've got it right.
    #[allow(unused, unreachable_code, clippy::diverging_sub_expression)]
    fn layer_compiles() {
        let pool: sqlx::Pool<sqlx::Sqlite> = todo!();

        let (layer, _state) = Layer::new(pool);

        let app = axum::Router::new()
            .route("/", axum::routing::get(|| async { "hello" }))
            .layer(layer);

        axum::Server::bind(todo!()).serve(app.into_make_service());
    }
}
