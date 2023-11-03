/// Application state that enables the [`Tx`] extractor.
///
/// `State` must be provided to `Router`s in order to use the [`Tx`] extractor, or else attempting
/// to use the `Router` will not compile.
///
/// `State` is constructed via [`Tx::setup`](crate::Tx::setup) or
/// [`Config::setup`](crate::Config::setup), which also return a middleware [`Layer`](crate::Layer).
/// The state and the middleware together enable the [`Tx`] extractor to work.
///
/// [`Tx`]: crate::Tx
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
