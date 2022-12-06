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
//! # async fn foo() {
//! let pool = /* any sqlx::Pool */
//! # sqlx::SqlitePool::connect(todo!()).await.unwrap();
//! let app = axum::Router::new()
//!     // .route(...)s
//!     .layer(
//!         tower::builder::ServiceBuilder::new()
//!             .layer(axum::error_handling::HandleErrorLayer::new(|err| async move {
//!             // Tx layer will throw an error if there are any issues while committing the transaction.
//!             // So handle those cases and return error response accordingly.
//!             println!("Error occurred while committing the transaction : {err:?}");
//!
//!             (
//!                  http::StatusCode::INTERNAL_SERVER_ERROR,
//!                  "internal server error",
//!             )
//!         }))
//!     .layer(axum_sqlx_tx::Layer::new(pool)),
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
//! If you forget to add the middleware you'll get [`Error::MissingExtension`] (internal server
//! error) when using the extractor. You'll also get an error ([`Error::OverlappingExtractors`]) if
//! you have multiple `Tx` arguments in a single handler, or call `Tx::from_request` multiple times
//! in a single middleware.
//!
//! ## Error handling
//!
//! `axum` requires that middleware do not return errors, and that the errors returned by extractors
//! implement `IntoResponse`. By default, [`Error`](Error) is used by [`Layer`] and [`Tx`] to
//! convert errors into HTTP 500 responses, with the error's `Display` value as the response body,
//! however it's generally not a good practice to return internal error details to clients!
//!
//! So use custom error responses to hide the implementation details.
//! Refer: [Customizing extractor responses] (https://docs.rs/axum/latest/axum/extract/index.html#customizing-extractor-responses)
//!
//! # Examples
//!
//! See [`examples/`][examples] in the repo for more examples.
//!
//! [examples]: https://github.com/wasdacraic/axum-sqlx-tx/tree/master/examples

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
/// extractor rejections.
///
/// Refer: [Customizing extractor responses] (https://docs.rs/axum/latest/axum/extract/index.html#customizing-extractor-responses)
/// ```
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

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct TxLayerError(
    // A database error occurred when committing the transaction
    #[from] sqlx::Error,
);
