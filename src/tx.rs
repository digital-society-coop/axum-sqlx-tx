//! A request extension that enables the [`Tx`](crate::Tx) extractor.

use axum_core::{
    extract::{FromRequest, RequestParts},
    response::{IntoResponse, Response},
};
use sqlx::Transaction;

use crate::slot::{Lease, Slot};

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
#[derive(Debug)]
pub struct Tx<DB: sqlx::Database>(Lease<sqlx::Transaction<'static, DB>>);

impl<DB: sqlx::Database> Tx<DB> {
    /// Explicitly commit the transaction.
    ///
    /// By default, the transaction will be committed when a successful response is returned
    /// (specifically, when the [`Service`](crate::Service) middleware intercepts an HTTP `2XX`
    /// response). This method allows the transaction to be committed explicitly.
    ///
    /// **Note:** trying to use the `Tx` extractor again after calling `commit` will currently
    /// generate [`Error::OverlappingExtractors`] errors. This may change in future.
    pub async fn commit(self) -> Result<(), sqlx::Error> {
        self.0.steal().commit().await
    }
}

impl<DB: sqlx::Database> AsRef<sqlx::Transaction<'static, DB>> for Tx<DB> {
    fn as_ref(&self) -> &sqlx::Transaction<'static, DB> {
        &self.0
    }
}

impl<DB: sqlx::Database> AsMut<sqlx::Transaction<'static, DB>> for Tx<DB> {
    fn as_mut(&mut self) -> &mut sqlx::Transaction<'static, DB> {
        &mut self.0
    }
}

impl<DB: sqlx::Database> std::ops::Deref for Tx<DB> {
    type Target = sqlx::Transaction<'static, DB>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<DB: sqlx::Database> std::ops::DerefMut for Tx<DB> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<DB: sqlx::Database, B: Send> FromRequest<B> for Tx<DB> {
    type Rejection = Error;

    fn from_request<'req, 'ctx>(
        req: &'req mut RequestParts<B>,
    ) -> futures_core::future::BoxFuture<'ctx, Result<Self, Self::Rejection>>
    where
        'req: 'ctx,
        Self: 'ctx,
    {
        Box::pin(async move {
            let ext: &mut Lazy<DB> = req
                .extensions_mut()
                .and_then(|ext| ext.get_mut())
                .ok_or(Error::MissingExtension)?;

            let tx = ext.get_or_begin().await?;

            Ok(Self(tx))
        })
    }
}

/// Possible errors when extracting [`Tx`] from a request.
///
/// `axum` requires that the [`FromRequest`] `Rejection` implements `IntoResponse`, which this does
/// by returning the `Display` representation of the variant. Note that this means returning
/// configuration and database errors to clients, but there's sadly not currently a better default
/// (open to feedback on this).
///
/// You could avoid this by extracting `Result<`[`Tx`]`, `[`Error`]`>` and handling the error:
///
/// ```
/// use axum_sqlx_tx::Tx;
/// use sqlx::Sqlite;
///
/// // Hypothetical application error type implementing IntoResponse
/// enum AppError {
///     Db(axum_sqlx_tx::Error),
/// }
///
/// async fn handler(tx: Result<Tx<Sqlite>, axum_sqlx_tx::Error>) -> Result<(), AppError> {
///     let tx = tx.map_err(AppError::Db)?;
///     /* ... */
/// # Ok(())
/// }
/// ```
#[derive(Debug, thiserror::Error)]
pub enum Error {
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

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        (http::StatusCode::INTERNAL_SERVER_ERROR, self.to_string()).into_response()
    }
}

/// The OG `Slot` – the transaction (if any) returns here when the `Extension` is dropped.
pub(crate) struct TxSlot<DB: sqlx::Database>(Slot<Option<Slot<Transaction<'static, DB>>>>);

impl<DB: sqlx::Database> TxSlot<DB> {
    /// Create a `TxSlot` bound to the given request extensions.
    ///
    /// When the request extensions are dropped, `commit` can be called to commit the transaction
    /// (if any).
    pub(crate) fn bind(extensions: &mut http::Extensions, pool: sqlx::Pool<DB>) -> Self {
        let (slot, tx) = Slot::new_leased(None);
        extensions.insert(Lazy { pool, tx });
        Self(slot)
    }

    pub(crate) async fn commit(self) -> Result<(), sqlx::Error> {
        if let Some(tx) = self.0.into_inner().flatten().and_then(Slot::into_inner) {
            tx.commit().await?;
        }
        Ok(())
    }
}

/// A lazily acquired transaction.
///
/// When the transaction is started, it's inserted into the `Option` leased from the `TxSlot`, so
/// that when `Lazy` is dropped the transaction is moved to the `TxSlot`.
struct Lazy<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
    tx: Lease<Option<Slot<Transaction<'static, DB>>>>,
}

impl<DB: sqlx::Database> Lazy<DB> {
    async fn get_or_begin(&mut self) -> Result<Lease<Transaction<'static, DB>>, Error> {
        let tx = if let Some(tx) = self.tx.as_mut() {
            tx
        } else {
            let tx = self.pool.begin().await?;
            self.tx.insert(Slot::new(tx))
        };

        tx.lease().ok_or(Error::OverlappingExtractors)
    }
}

#[cfg(any(
    feature = "any",
    feature = "mssql",
    feature = "mysql",
    feature = "postgres",
    feature = "sqlite"
))]
mod sqlx_impls {
    use futures_core::{future::BoxFuture, stream::BoxStream};

    macro_rules! impl_executor {
        ($db:path) => {
            impl<'c> sqlx::Executor<'c> for &'c mut super::Tx<$db> {
                type Database = $db;

                #[allow(clippy::type_complexity)]
                fn fetch_many<'e, 'q: 'e, E: 'q>(
                    self,
                    query: E,
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
                    E: sqlx::Execute<'q, Self::Database>,
                {
                    (&mut **self).fetch_many(query)
                }

                fn fetch_optional<'e, 'q: 'e, E: 'q>(
                    self,
                    query: E,
                ) -> BoxFuture<
                    'e,
                    Result<Option<<Self::Database as sqlx::Database>::Row>, sqlx::Error>,
                >
                where
                    'c: 'e,
                    E: sqlx::Execute<'q, Self::Database>,
                {
                    (&mut **self).fetch_optional(query)
                }

                fn prepare_with<'e, 'q: 'e>(
                    self,
                    sql: &'q str,
                    parameters: &'e [<Self::Database as sqlx::Database>::TypeInfo],
                ) -> BoxFuture<
                    'e,
                    Result<
                        <Self::Database as sqlx::database::HasStatement<'q>>::Statement,
                        sqlx::Error,
                    >,
                >
                where
                    'c: 'e,
                {
                    (&mut **self).prepare_with(sql, parameters)
                }

                fn describe<'e, 'q: 'e>(
                    self,
                    sql: &'q str,
                ) -> BoxFuture<'e, Result<sqlx::Describe<Self::Database>, sqlx::Error>>
                where
                    'c: 'e,
                {
                    (&mut **self).describe(sql)
                }
            }
        };
    }

    #[cfg(feature = "any")]
    impl_executor!(sqlx::Any);

    #[cfg(feature = "mssql")]
    impl_executor!(sqlx::Mssql);

    #[cfg(feature = "mysql")]
    impl_executor!(sqlx::MySql);

    #[cfg(feature = "postgres")]
    impl_executor!(sqlx::Postgres);

    #[cfg(feature = "sqlite")]
    impl_executor!(sqlx::Sqlite);
}
