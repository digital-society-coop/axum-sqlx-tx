//! Request-bound [SQLx] transactions for [axum].
//!
//! [SQLx]: https://github.com/launchbadge/sqlx#readme
//! [axum]: https://github.com/tokio-rs/axum#readme
//!
//! [`Tx`] is an `axum` [extractor][axum extractors] for obtaining a transaction that's bound to the
//! HTTP request. A transaction begins the first time the extractor is used for a request, and is
//! then stored in [request extensions] for use by other middleware/handlers. The transaction is
//! resolved depending on the status code of the eventual response – successful (HTTP `2XX` or
//! `3XX`) responses will cause the transaction to be committed, otherwise it will be rolled back.
//!
//! This behaviour is often a sensible default, and using the extractor (e.g. rather than directly
//! using [`sqlx::Transaction`]s) means you can't forget to commit the transactions!
//!
//! [axum extractors]: https://docs.rs/axum/latest/axum/#extractors
//! [request extensions]: https://docs.rs/http/latest/http/struct.Extensions.html
//!
//! # Usage
//!
//! To use the [`Tx`] extractor, you must first add [`State`] and [`Layer`] to your app. [`State`]
//! holds the configuration for the extractor, and the [`Layer`] middleware manages the
//! request-bound transaction.
//!
//! ```
//! # async fn foo() {
//! // It's recommended to create aliases specialised for your extractor(s)
//! type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;
//!
//! let pool = sqlx::SqlitePool::connect("...").await.unwrap();
//!
//! let (state, layer) = Tx::setup(pool);
//!
//! let app = axum::Router::new()
//!     // .route(...)s
//! #   .route("/", axum::routing::get(|tx: Tx| async move {}))
//!     .layer(layer)
//!     .with_state(state);
//! # axum::Server::bind(todo!()).serve(app.into_make_service());
//! # }
//! ```
//!
//! You can then simply add [`Tx`] as an argument to your handlers:
//!
//! ```
//! type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;
//!
//! async fn create_user(mut tx: Tx, /* ... */) {
//!     // `&mut Tx` implements `sqlx::Executor`
//!     let user = sqlx::query("INSERT INTO users (...) VALUES (...)")
//!         .fetch_one(&mut tx)
//!         .await
//!         .unwrap();
//!
//!     // `Tx` also implements `Deref<Target = sqlx::Transaction>` and `DerefMut`
//!     use sqlx::Acquire;
//!     let inner = tx.begin().await.unwrap();
//!     /* ... */
//! }
//! ```
//!
//! ## Error handling
//!
//! `axum` requires that errors can be turned into responses. The [`Error`] type converts into a
//! HTTP 500 response with the error message as the response body. This may be suitable for
//! development or internal services but it's generally not advisable to return internal error
//! details to clients.
//!
//! See [`Error`] for how to customise error handling.
//!
//! # Examples
//!
//! See [`examples/`][examples] in the repo for more examples.
//!
//! [examples]: https://github.com/digital-society-coop/axum-sqlx-tx/tree/master/examples

#![cfg_attr(doc, deny(warnings))]

mod layer;
mod slot;
mod tx;

use std::marker::PhantomData;

pub use crate::{
    layer::{Layer, Service},
    tx::Tx,
};

/// Configuration for [`Tx`] extractors.
///
/// Use `Config` to configure and create a [`State`] and [`Layer`].
///
/// Access the `Config` API from [`Tx::config`].
///
/// ```
/// # async fn foo() {
/// # let pool: sqlx::SqlitePool = todo!();
/// type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;
///
/// let config = Tx::config(pool);
/// # }
/// ```
pub struct Config<DB: sqlx::Database, LayerError> {
    pool: sqlx::Pool<DB>,
    _layer_error: PhantomData<LayerError>,
}

impl<DB: sqlx::Database, LayerError> Config<DB, LayerError> {
    fn new(pool: sqlx::Pool<DB>) -> Self {
        Self {
            pool,
            _layer_error: PhantomData,
        }
    }

    /// Change the layer error type.
    pub fn layer_error<E>(self) -> Config<DB, E>
    where
        Error: Into<E>,
    {
        Config {
            pool: self.pool,
            _layer_error: PhantomData,
        }
    }

    /// Create a [`State`] and [`Layer`] to enable the [`Tx`] extractor.
    pub fn setup(self) -> (State<DB>, Layer<DB, LayerError>) {
        let state = State::new(self.pool);
        let layer = Layer::new(state.clone());
        (state, layer)
    }
}

/// Application state that enables the [`Tx`] extractor.
///
/// `State` must be provided to `Router`s in order to use the [`Tx`] extractor, or else attempting
/// to use the `Router` will not compile.
///
/// `State` is constructed via [`Tx::setup`] or [`Config::setup`], which also return a middleware
/// [`Layer`]. The state and the middleware together enable the [`Tx`] extractor to work.
#[derive(Debug)]
pub struct State<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
}

impl<DB: sqlx::Database> State<DB> {
    pub(crate) fn new(pool: sqlx::Pool<DB>) -> Self {
        Self { pool }
    }

    pub(crate) async fn transaction(&self) -> Result<sqlx::Transaction<'static, DB>, sqlx::Error> {
        self.pool.begin().await
    }
}

impl<DB: sqlx::Database> Clone for State<DB> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

/// Possible errors when extracting [`Tx`] from a request.
///
/// Errors can occur at two points during the request lifecycle:
///
/// 1. The [`Tx`] extractor might fail to obtain a connection from the pool and `BEGIN` a
///    transaction. This could be due to:
///
///    - Forgetting to add the middleware: [`Error::MissingExtension`].
///    - Calling the extractor multiple times in the same request: [`Error::OverlappingExtractors`].
///    - A problem communicating with the database: [`Error::Database`].
///
/// 2. The middleware [`Layer`] might fail to commit the transaction. This could be due to a problem
///    communicating with the database, or else a logic error (e.g. unsatisfied deferred
///    constraint): [`Error::Database`].
///
/// `axum` requires that errors can be turned into responses. The [`Error`] type converts into a
/// HTTP 500 response with the error message as the response body. This may be suitable for
/// development or internal services but it's generally not advisable to return internal error
/// details to clients.
///
/// You can override the error types for both the [`Tx`] extractor and [`Layer`]:
///
/// - Override the [`Tx`]`<DB, E>` error type using the `E` generic type parameter.
/// - Override the [`Layer`] error type using [`Config::layer_error`].
///
/// In both cases, the error type must implement `From<`[`Error`]`>` and
/// `axum::response::IntoResponse`.
///
/// ```
/// use axum::{response::IntoResponse, routing::post};
///
/// struct MyError(axum_sqlx_tx::Error);
///
/// impl From<axum_sqlx_tx::Error> for MyError {
///     fn from(error: axum_sqlx_tx::Error) -> Self {
///         Self(error)
///     }
/// }
///
/// impl IntoResponse for MyError {
///     fn into_response(self) -> axum::response::Response {
///         // note that you would probably want to log the error as well
///         (http::StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
///     }
/// }
///
/// // Override the `Tx` error type using the second generic type parameter
/// type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite, MyError>;
///
/// # async fn foo() {
/// let pool = sqlx::SqlitePool::connect("...").await.unwrap();
///
/// let (state, layer) = Tx::config(pool)
///     // Override the `Layer` error type using the `Config` API
///     .layer_error::<MyError>()
///     .setup();
/// # let app = axum::Router::new()
/// #    .route("/", post(create_user))
/// #    .layer(layer)
/// #    .with_state(state);
/// # axum::Server::bind(todo!()).serve(app.into_make_service());
/// # }
/// # async fn create_user(mut tx: Tx, /* ... */) {
/// #     /* ... */
/// # }
/// ```
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Indicates that the [`Layer`] middleware was not installed.
    #[error("required extension not registered; did you add the axum_sqlx_tx::Layer middleware?")]
    MissingExtension,

    /// Indicates that [`Tx`] was extracted multiple times in a single handler/middleware.
    #[error("axum_sqlx_tx::Tx extractor used multiple times in the same handler/middleware")]
    OverlappingExtractors,

    /// A database error occurred when starting or committing the transaction.
    #[error(transparent)]
    Database {
        #[from]
        error: sqlx::Error,
    },
}

impl axum_core::response::IntoResponse for Error {
    fn into_response(self) -> axum_core::response::Response {
        (http::StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}
