//! A request extension that enables the [`Tx`](crate::Tx) extractor.

use std::marker::PhantomData;

use axum_core::{extract::FromRequestParts, response::IntoResponse};
use http::request::Parts;
use sqlx::Transaction;

use crate::{
    slot::{Lease, Slot},
    Error,
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
#[derive(Debug)]
pub struct Tx<DB: sqlx::Database, E = Error>(Lease<sqlx::Transaction<'static, DB>>, PhantomData<E>);

impl<DB: sqlx::Database, E> Tx<DB, E> {
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

impl<DB: sqlx::Database, E> AsRef<sqlx::Transaction<'static, DB>> for Tx<DB, E> {
    fn as_ref(&self) -> &sqlx::Transaction<'static, DB> {
        &self.0
    }
}

impl<DB: sqlx::Database, E> AsMut<sqlx::Transaction<'static, DB>> for Tx<DB, E> {
    fn as_mut(&mut self) -> &mut sqlx::Transaction<'static, DB> {
        &mut self.0
    }
}

impl<DB: sqlx::Database, E> std::ops::Deref for Tx<DB, E> {
    type Target = sqlx::Transaction<'static, DB>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<DB: sqlx::Database, E> std::ops::DerefMut for Tx<DB, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<DB: sqlx::Database, S, E> FromRequestParts<S> for Tx<DB, E>
    where
        E: From<Error> + IntoResponse,
{
    type Rejection = E;

    fn from_request_parts<'req, 'state, 'ctx>(
        parts: &'req mut Parts,
        _state: &'state S,
    ) -> futures_core::future::BoxFuture<'ctx, Result<Self, Self::Rejection>>
        where
            Self: 'ctx,
            'req: 'ctx,
            'state: 'ctx,
    {
        Box::pin(async move {
            let ext: &mut Lazy<DB> = parts.extensions.get_mut().ok_or(Error::MissingExtension)?;

            let tx = ext.get_or_begin().await?;

            Ok(Self(tx, PhantomData))
        })
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

impl<DB: sqlx::Database> Clone for Lazy<DB> {
    fn clone(&self) -> Self {
        Self {
            pool: self.pool.clone(),
            tx: self.tx.clone(),
        }
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
    use std::fmt::Debug;

    use futures_core::{future::BoxFuture, stream::BoxStream};

    macro_rules! impl_executor {
        ($db:path) => {
            impl<'c, E: Debug + Send> sqlx::Executor<'c> for &'c mut super::Tx<$db, E> {
                type Database = $db;

                #[allow(clippy::type_complexity)]
                fn fetch_many<'e, 'q: 'e, Q: 'q>(
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
                    Q: sqlx::Execute<'q, Self::Database>,
                {
                    (&mut **self).fetch_many(query)
                }

                fn fetch_optional<'e, 'q: 'e, Q: 'q>(
                    self,
                    query: Q,
                ) -> BoxFuture<
                    'e,
                    Result<Option<<Self::Database as sqlx::Database>::Row>, sqlx::Error>,
                >
                where
                    'c: 'e,
                    Q: sqlx::Execute<'q, Self::Database>,
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
