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
/// - Override the [`Tx`]`<DB, E>` error type using the `E` generic type parameter. `E` must be
///   convertible from [`Error`] (e.g. [`Error`]`: Into<E>`).
///
/// - Override the [`Layer`] error type using [`Config::layer_error`](crate::Config::layer_error).
///   The layer error type must be convertible from `sqlx::Error` (e.g.
///   `sqlx::Error: Into<LayerError>`).
///
/// In both cases, the error type must implement `axum::response::IntoResponse`.
///
/// ```
/// use axum::{response::IntoResponse, routing::post};
///
/// enum MyError{
///     Extractor(axum_sqlx_tx::Error),
///     Layer(sqlx::Error),
/// }
///
/// impl From<axum_sqlx_tx::Error> for MyError {
///     fn from(error: axum_sqlx_tx::Error) -> Self {
///         Self::Extractor(error)
///     }
/// }
///
/// impl From<sqlx::Error> for MyError {
///     fn from(error: sqlx::Error) -> Self {
///         Self::Layer(error)
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
/// # let listener: tokio::net::TcpListener = todo!();
/// # axum::serve(listener, app);
/// # }
/// # async fn create_user(mut tx: Tx, /* ... */) {
/// #     /* ... */
/// # }
/// ```
///
/// [`Tx`]: crate::Tx
/// [`Layer`]: crate::Layer
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Indicates that the [`Layer`](crate::Layer) middleware was not installed.
    #[error("required extension not registered; did you add the axum_sqlx_tx::Layer middleware?")]
    MissingExtension,

    /// Indicates that [`Tx`](crate::Tx) was extracted multiple times in a single
    /// handler/middleware.
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
