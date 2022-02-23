//! A request extension that enables the [`Tx`](crate::Tx) extractor.

use axum_core::extract::{FromRequest, RequestParts};
use sqlx::Transaction;

use crate::{
    slot::{Lease, Slot},
    Error,
};

/// A request extension that enables the [`Tx`](crate::Tx) extractor.
///
/// This is private to the library. It is added to requests by [`Layer`](crate::layer::Layer).
pub(crate) struct Extension<DB: sqlx::Database> {
    pool: sqlx::Pool<DB>,
    tx: Lease<Option<Slot<Transaction<'static, DB>>>>,
}

impl<DB: sqlx::Database> Extension<DB> {
    pub(crate) fn new(pool: sqlx::Pool<DB>) -> (Self, TxSlot<DB>) {
        let mut slot = Slot::new(None);
        (
            Self {
                pool,
                // We can unwrap here because we just populated the slot
                tx: slot.lease().unwrap(),
            },
            TxSlot(slot),
        )
    }

    pub(crate) async fn get_or_begin(&mut self) -> Result<Lease<Transaction<'static, DB>>, Error> {
        let tx = if let Some(tx) = self.tx.as_mut() {
            tx
        } else {
            let tx = self.pool.begin().await?;
            self.tx.insert(Slot::new(tx))
        };

        tx.lease().ok_or(Error::OverlappingExtractors)
    }
}

/// A convenience wrapper to leak fewer implementation details.
pub(crate) struct TxSlot<DB: sqlx::Database>(Slot<Option<Slot<Transaction<'static, DB>>>>);

impl<DB: sqlx::Database> TxSlot<DB> {
    pub(crate) fn into_inner(self) -> Option<Transaction<'static, DB>> {
        self.0.into_inner().flatten().and_then(Slot::into_inner)
    }
}

/// An `axum` extractor for a database transaction.
#[derive(Debug)]
pub struct Tx<DB: sqlx::Database>(Lease<sqlx::Transaction<'static, DB>>);

impl<DB: sqlx::Database> Tx<DB> {
    async fn from_request<B>(req: &mut RequestParts<B>) -> Result<Self, Error> {
        let ext: &mut Extension<DB> = req
            .extensions_mut()
            .and_then(|ext| ext.get_mut())
            .ok_or(Error::MissingExtension)?;

        let tx = ext.get_or_begin().await?;

        Ok(Self(tx))
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
    ) -> futures::future::BoxFuture<'ctx, Result<Self, Self::Rejection>>
    where
        'req: 'ctx,
        Self: 'ctx,
    {
        Box::pin(Self::from_request(req))
    }
}
