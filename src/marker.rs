use std::fmt::Debug;

/// Extractor marker type.
///
/// Since the [`Tx`](crate::Tx) extractor operates at the type level, a generic type parameter is
/// used to identify different databases.
///
/// There is a blanket implementation for all implementors of [`sqlx::Database`], but you can create
/// your own types if you need to work with multiple databases.
///
/// ```
/// // Marker struct "database 1"
/// #[derive(Debug)]
/// struct Db1;
///
/// impl axum_sqlx_tx::Marker for Db1 {
///     type Driver = sqlx::Sqlite;
/// }
///
/// // Marker struct "database 2"
/// #[derive(Debug)]
/// struct Db2;
///
/// impl axum_sqlx_tx::Marker for Db2 {
///     type Driver = sqlx::Sqlite;
/// }
///
/// // You'll also need a "state" structure that implements `FromRef` for each `State<DB>`
/// #[derive(Clone)]
/// struct MyState {
///     state1: axum_sqlx_tx::State<Db1>,
///     state2: axum_sqlx_tx::State<Db2>,
/// }
///
/// impl axum::extract::FromRef<MyState> for axum_sqlx_tx::State<Db1> {
///     fn from_ref(state: &MyState) -> Self {
///         state.state1.clone()
///     }
/// }
///
/// impl axum::extract::FromRef<MyState> for axum_sqlx_tx::State<Db2> {
///     fn from_ref(state: &MyState) -> Self {
///         state.state2.clone()
///     }
/// }
///
/// // The extractor can then be aliased for each DB
/// type Tx1 = axum_sqlx_tx::Tx<Db1>;
/// type Tx2 = axum_sqlx_tx::Tx<Db2>;
///
/// # async fn foo() {
/// // Setup each extractor
/// let pool1 = sqlx::SqlitePool::connect("...").await.unwrap();
/// let (state1, layer1) = Tx1::setup(pool1);
///
/// let pool2 = sqlx::SqlitePool::connect("...").await.unwrap();
/// let (state2, layer2) = Tx2::setup(pool2);
///
/// let app = axum::Router::new()
///     .route("/", axum::routing::get(|tx1: Tx1, tx2: Tx2| async move {
///         /* ... */
///     }))
///     .layer(layer1)
///     .layer(layer2)
///     .with_state(MyState { state1, state2 });
/// # axum::Server::bind(todo!()).serve(app.into_make_service());
/// # }
/// ```
pub trait Marker: Debug + Send + Sized + 'static {
    /// The `sqlx` database driver.
    type Driver: sqlx::Database;
}

impl<DB: sqlx::Database> Marker for DB {
    type Driver = Self;
}
