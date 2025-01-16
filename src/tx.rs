//! A request extension that enables the [`Tx`](crate::Tx) extractor.

use std::{fmt, marker::PhantomData};

use axum_core::{
    extract::{FromRef, FromRequestParts},
    response::IntoResponse,
};
use futures_core::{future::BoxFuture, stream::BoxStream};
use http::request::Parts;
use parking_lot::{lock_api::ArcMutexGuard, RawMutex};

use crate::{
    extension::{Extension, LazyTransaction},
    Config, Error, Marker, State,
};

/// An `axum` extractor for a database transaction.
///
/// `&mut Tx` implements [`sqlx::Executor`] so it can be used directly with [`sqlx::query()`]
/// (and [`sqlx::query_as()`], the corresponding macros, etc.):
///
/// ```
/// use axum_sqlx_tx::Tx;
/// use sqlx::Sqlite;
///
/// async fn handler(mut tx: Tx<Sqlite>) -> Result<(), sqlx::Error> {
///     sqlx::query("...").execute(&mut tx).await?;
///     /* ... */
/// #   Ok(())
/// }
/// ```
///
/// It also implements `Deref<Target = `[`sqlx::Transaction`]`>` and `DerefMut`, so you can call
/// methods from `Transaction` and its traits:
///
/// ```
/// use axum_sqlx_tx::Tx;
/// use sqlx::{Acquire as _, Sqlite};
///
/// async fn handler(mut tx: Tx<Sqlite>) -> Result<(), sqlx::Error> {
///     let inner = tx.begin().await?;
///     /* ... */
/// #   Ok(())
/// }
/// ```
///
/// The `E` generic parameter controls the error type returned when the extractor fails. This can be
/// used to configure the error response returned when the extractor fails:
///
/// ```
/// use axum::response::IntoResponse;
/// use axum_sqlx_tx::Tx;
/// use sqlx::Sqlite;
///
/// struct MyError(axum_sqlx_tx::Error);
///
/// // The error type must implement From<axum_sqlx_tx::Error>
/// impl From<axum_sqlx_tx::Error> for MyError {
///     fn from(error: axum_sqlx_tx::Error) -> Self {
///         Self(error)
///     }
/// }
///
/// // The error type must implement IntoResponse
/// impl IntoResponse for MyError {
///     fn into_response(self) -> axum::response::Response {
///         (http::StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
///     }
/// }
///
/// async fn handler(tx: Tx<Sqlite, MyError>) {
///     /* ... */
/// }
/// ```
pub struct Tx<DB: Marker, E = Error> {
    tx: ArcMutexGuard<RawMutex, LazyTransaction<DB>>,
    _error: PhantomData<E>,
}

impl<DB: Marker, E> Tx<DB, E> {
    /// Crate a [`State`] and [`Layer`](crate::Layer) to enable the extractor.
    ///
    /// This is convenient to use from a type alias, e.g.
    ///
    /// ```
    /// # async fn foo() {
    /// type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;
    ///
    /// let pool: sqlx::SqlitePool = todo!();
    /// let (state, layer) = Tx::setup(pool);
    /// # }
    /// ```
    pub fn setup(pool: sqlx::Pool<DB::Driver>) -> (State<DB>, crate::Layer<DB, Error>) {
        Config::new(pool).setup()
    }

    /// Configure extractor behaviour.
    ///
    /// See the [`Config`] API for available options.
    ///
    /// This is convenient to use from a type alias, e.g.
    ///
    /// ```
    /// # async fn foo() {
    /// type Tx = axum_sqlx_tx::Tx<sqlx::Sqlite>;
    ///
    /// # let pool: sqlx::SqlitePool = todo!();
    /// let config = Tx::config(pool);
    /// # }
    /// ```
    pub fn config(pool: sqlx::Pool<DB::Driver>) -> Config<DB, Error> {
        Config::new(pool)
    }

    /// Explicitly commit the transaction.
    ///
    /// By default, the transaction will be committed when a successful response is returned
    /// (specifically, when the [`Service`](crate::Service) middleware intercepts an HTTP `2XX` or
    /// `3XX` response). This method allows the transaction to be committed explicitly.
    ///
    /// **Note:** trying to use the `Tx` extractor again after calling `commit` will currently
    /// generate [`Error::OverlappingExtractors`] errors. This may change in future.
    pub async fn commit(mut self) -> Result<(), sqlx::Error> {
        self.tx.commit().await
    }
}

impl<DB: Marker, E> fmt::Debug for Tx<DB, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tx").finish_non_exhaustive()
    }
}

impl<DB: Marker, E> AsRef<sqlx::Transaction<'static, DB::Driver>> for Tx<DB, E> {
    fn as_ref(&self) -> &sqlx::Transaction<'static, DB::Driver> {
        self.tx.as_ref()
    }
}

impl<DB: Marker, E> AsMut<sqlx::Transaction<'static, DB::Driver>> for Tx<DB, E> {
    fn as_mut(&mut self) -> &mut sqlx::Transaction<'static, DB::Driver> {
        self.tx.as_mut()
    }
}

impl<DB: Marker, E> std::ops::Deref for Tx<DB, E> {
    type Target = sqlx::Transaction<'static, DB::Driver>;

    fn deref(&self) -> &Self::Target {
        self.tx.as_ref()
    }
}

impl<DB: Marker, E> std::ops::DerefMut for Tx<DB, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.tx.as_mut()
    }
}

impl<DB: Marker, S, E> FromRequestParts<S> for Tx<DB, E>
where
    S: Sync,
    E: From<Error> + IntoResponse + Send,
    State<DB>: FromRef<S>,
{
    type Rejection = E;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let ext: &Extension<DB> = parts.extensions.get().ok_or(Error::MissingExtension)?;

        let tx = ext.acquire().await?;

        Ok(Self {
            tx,
            _error: PhantomData,
        })
    }
}

impl<'c, DB, E> sqlx::Executor<'c> for &'c mut Tx<DB, E>
where
    DB: Marker,
    for<'t> &'t mut <DB::Driver as sqlx::Database>::Connection:
        sqlx::Executor<'t, Database = DB::Driver>,
    E: std::fmt::Debug + Send,
{
    type Database = DB::Driver;

    #[allow(clippy::type_complexity)]
    fn fetch_many<'e, 'q: 'e, Q>(
        self,
        query: Q,
    ) -> BoxStream<
        'e,
        Result<
            sqlx::Either<
                <Self::Database as sqlx::Database>::QueryResult,
                <Self::Database as sqlx::Database>::Row,
            >,
            sqlx::Error,
        >,
    >
    where
        'c: 'e,
        Q: sqlx::Execute<'q, Self::Database> + 'q,
    {
        (&mut ***self).fetch_many(query)
    }

    fn fetch_optional<'e, 'q: 'e, Q>(
        self,
        query: Q,
    ) -> BoxFuture<'e, Result<Option<<Self::Database as sqlx::Database>::Row>, sqlx::Error>>
    where
        'c: 'e,
        Q: sqlx::Execute<'q, Self::Database> + 'q,
    {
        (&mut ***self).fetch_optional(query)
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [<Self::Database as sqlx::Database>::TypeInfo],
    ) -> BoxFuture<'e, Result<<Self::Database as sqlx::Database>::Statement<'q>, sqlx::Error>>
    where
        'c: 'e,
    {
        (&mut ***self).prepare_with(sql, parameters)
    }

    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, Result<sqlx::Describe<Self::Database>, sqlx::Error>>
    where
        'c: 'e,
    {
        (&mut ***self).describe(sql)
    }
}
