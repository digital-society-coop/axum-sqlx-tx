//! A request extension that enables the [`Tx`](crate::Tx) extractor.

use axum_core::extract::{FromRequest, RequestParts};
use sqlx::Transaction;

use crate::{
    slot::{Lease, Slot},
    Error,
};

/// An `axum` extractor for a database transaction.
#[derive(Debug)]
pub struct Tx<DB: sqlx::Database>(Lease<sqlx::Transaction<'static, DB>>);

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
    ) -> futures::future::BoxFuture<'ctx, Result<Self, Self::Rejection>>
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

    pub(crate) async fn commit(self) -> Result<(), Error> {
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
