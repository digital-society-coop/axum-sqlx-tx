use std::marker::PhantomData;

use crate::{Layer, Marker, State};

/// Configuration for [`Tx`](crate::Tx) extractors.
///
/// Use `Config` to configure and create a [`State`] and [`Layer`].
///
/// Access the `Config` API from [`Tx::config`](crate::Tx::config).
///
/// ```
/// # async fn foo() {
/// # let pool: sqlx::SqlitePool = todo!();
/// type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;
///
/// let config = Tx::config(pool);
/// # }
/// ```
pub struct Config<DB: Marker, LayerError> {
    pool: sqlx::Pool<DB::Driver>,
    _layer_error: PhantomData<LayerError>,
}

impl<DB: Marker, LayerError> Config<DB, LayerError>
where
    LayerError: axum_core::response::IntoResponse,
    sqlx::Error: Into<LayerError>,
{
    pub(crate) fn new(pool: sqlx::Pool<DB::Driver>) -> Self {
        Self {
            pool,
            _layer_error: PhantomData,
        }
    }

    /// Change the layer error type.
    pub fn layer_error<E>(self) -> Config<DB, E>
    where
        sqlx::Error: Into<E>,
    {
        Config {
            pool: self.pool,
            _layer_error: PhantomData,
        }
    }

    /// Create a [`State`] and [`Layer`] to enable the [`Tx`](crate::Tx) extractor.
    pub fn setup(self) -> (State<DB>, Layer<DB, LayerError>) {
        let state = State::new(self.pool);
        let layer = Layer::new(state.clone());
        (state, layer)
    }
}
