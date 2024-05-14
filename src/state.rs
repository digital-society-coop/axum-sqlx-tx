use axum_core::extract::FromRef;

use crate::Marker;

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
pub struct State<DB: Marker> {
    pool: sqlx::Pool<DB::Driver>,
}

impl<DB: Marker> State<DB> {
    pub(crate) fn new(pool: sqlx::Pool<DB::Driver>) -> Self {
        Self { pool }
    }

    pub(crate) async fn transaction(
        &self,
    ) -> Result<sqlx::Transaction<'static, DB::Driver>, sqlx::Error> {
        self.pool.begin().await
    }
}

impl<DB: Marker> Clone for State<DB> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
        }
    }
}

impl<DB: Marker> FromRef<State<DB>> for sqlx::Pool<DB::Driver> {
    fn from_ref(input: &State<DB>) -> Self {
        input.pool.clone()
    }
}
