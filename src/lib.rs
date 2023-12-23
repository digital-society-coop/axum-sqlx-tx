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
//! # axum::serve(todo!(), app);
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
//! ## Multiple databases
//!
//! If you need to work with multiple databases, you can define marker structs for each. See
//! [`Marker`] for an example.
//!
//! It's not currently possible to use `Tx` for a dynamic number of databases. Feel free to open an
//! issue if you have a requirement for this.
//!
//! # Examples
//!
//! See [`examples/`][examples] in the repo for more examples.
//!
//! [examples]: https://github.com/digital-society-coop/axum-sqlx-tx/tree/master/examples

#![cfg_attr(doc, deny(warnings))]

mod config;
mod error;
mod extension;
mod layer;
mod marker;
mod slot;
mod state;
mod tx;

pub use crate::{
    config::Config,
    error::Error,
    layer::{Layer, Service},
    marker::Marker,
    state::State,
    tx::Tx,
};
