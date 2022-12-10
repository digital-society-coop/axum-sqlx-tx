//! Request-bound [SQLx] transactions for [axum].
//!
//! [SQLx]: https://github.com/launchbadge/sqlx#readme
//! [axum]: https://github.com/tokio-rs/axum#readme
//!
//! [`Tx`] is an `axum` [extractor][axum extractors] for obtaining a transaction that's bound to the
//! HTTP request. A transaction begins the first time the extractor is used for a request, and is
//! then stored in [request extensions] for use by other middleware/handlers. The transaction is
//! resolved depending on the status code of the eventual response – successful (HTTP `2XX`)
//! responses will cause the transaction to be committed, otherwise it will be rolled back.
//!
//! This behaviour is often a sensible default, and using the extractor (e.g. rather than directly
//! using [`sqlx::Transaction`]s) means you can't forget to commit the transactions!
//!
//! [axum extractors]: https://docs.rs/axum/latest/axum/#extractors
//! [request extensions]: https://docs.rs/http/latest/http/struct.Extensions.html
//!
//! # Usage
//!
//! To use the [`Tx`] extractor, you must first add [`Layer`] to your app:
//!
//! ```
//! use axum::error_handling::HandleErrorLayer;
//! use http::StatusCode;
//! use tower::builder::ServiceBuilder;
//!
//! # async fn foo() {
//! let pool = /* any sqlx::Pool */
//! # sqlx::SqlitePool::connect(todo!()).await.unwrap();
//! let app = axum::Router::new()
//!     // .route(...)s
//!     .layer(
//!         ServiceBuilder::new()
//!             .layer(HandleErrorLayer::new(|err| async move {
//!                 // Tx layer may throw an error if there are any error
//!                 // while committing the transaction.
//!         
//!                 // So handle those errors and return http response accordingly.
//!                 println!("Error occurred while committing the transaction : {err:?}");
//!                 
//!                 (StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
//!             }))
//!             .layer(axum_sqlx_tx::Layer::new(pool))
//!     );
//! # axum::Server::bind(todo!()).serve(app.into_make_service());
//! # }
//! ```
//!
//! You can then simply add [`Tx`] as an argument to your handlers:
//!
//! ```
//! use axum_sqlx_tx::Tx;
//! use sqlx::Sqlite;
//!
//! async fn create_user(mut tx: Tx<Sqlite>, /* ... */) {
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
//! If you forget to add the middleware you'll get [`TxRejection::MissingExtension`] (internal server
//! error) when using the extractor. You'll also get [`TxRejection::OverlappingExtractors`] error if
//! you have multiple `Tx` arguments in a single handler, or call `Tx::from_request` multiple times
//! in a single middleware.
//!
//! ## Extractor Error handling
//!
//! `axum` requires that middleware do not return errors, and that the errors returned by extractors
//! implement `IntoResponse`. By default, [`TxRejection`](TxRejection) is used by [`Tx`] extractor to
//! convert errors into HTTP 500 responses, with the error's `Display` value as the response body,
//! however it's generally not a good practice to return internal error details to clients!.
//! Refer `axum`'s [Customizing extractor responses][axum customizing extractor error] for futher details.
//!
//! # Examples
//!
//! See [`examples/`][examples] in the repo for more examples and how to customize extractor error.
//!
//! [examples]: https://github.com/wasdacraic/axum-sqlx-tx/tree/master/examples
//! [axum customizing extractor error]: https://docs.rs/axum/latest/axum/extract/index.html#customizing-extractor-responses

#![cfg_attr(doc, deny(warnings))]

mod layer;
mod slot;
mod tx;

pub use crate::{
    layer::{Layer, Service},
    tx::Tx,
};

/// Possible errors when extracting [`Tx`] from a request.
///
/// `axum` requires that the `FromRequestParts` `Rejection` implements `IntoResponse`, which this does
/// by returning the `Display` representation of the variant. Note that this means returning
/// configuration and database errors to clients, but you can override the error response by wrapping
/// extractor rejections. Refer `axum`'s [Customizing extractor responses][axum customizing extractor error]
/// for futher details.
///
/// [axum customizing extractor error]: https://docs.rs/axum/latest/axum/extract/index.html#customizing-extractor-responses
#[derive(Debug, thiserror::Error)]
pub enum TxRejection {
    /// Indicates that the [`Layer`](crate::Layer) middleware was not installed.
    #[error("required extension not registered; did you add the axum_sqlx_tx::Layer middleware?")]
    MissingExtension,

    /// Indicates that [`Tx`] was extracted multiple times in a single handler/middleware.
    #[error("axum_sqlx_tx::Tx extractor used multiple times in the same handler/middleware")]
    OverlappingExtractors,

    /// A database error occurred when starting the transaction.
    #[error(transparent)]
    Database {
        #[from]
        error: sqlx::Error,
    },
}

impl axum_core::response::IntoResponse for TxRejection {
    fn into_response(self) -> axum_core::response::Response {
        (http::StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}

/// Error which may occur while committing the changes done in the transaction.
///
/// `axum` requires `Service` layer to have `Infallible` error type.
/// But since a transaction may fail during commit, that needs to be
/// handled using the `HandleErrorLayer`.
///
/// ```
/// use axum::error_handling::HandleErrorLayer;
/// use http::StatusCode;
/// use tower::builder::ServiceBuilder;
///
/// # async fn foo() {
/// let pool = /* any sqlx::Pool */
/// # sqlx::SqlitePool::connect(todo!()).await.unwrap();
/// let tx_layer = ServiceBuilder::new()
///     .layer(HandleErrorLayer::new(|err| async move {
///         // Tx layer may throw an error if there are any error
///         // while committing the transaction.
///         
///         // So handle those errors and return http response accordingly.
///         println!("Error occurred while committing the transaction : {err:?}");
///         
///         (StatusCode::INTERNAL_SERVER_ERROR, "internal server error")
///     }))
///     .layer(axum_sqlx_tx::Layer::new(pool));
/// # let app = axum::Router::new().layer(tx_layer);
/// # axum::Server::bind(todo!()).serve(app.into_make_service());
/// # }
/// ```
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct TxLayerError(
    /// A database error occurred when committing the transaction
    #[from]
    pub sqlx::Error,
);
