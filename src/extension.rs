use sqlx::Transaction;

use crate::{
    slot::{Lease, Slot},
    Error, Marker, State,
};

/// The request extension.
pub(crate) struct Extension<DB: Marker> {
    slot: Slot<LazyTransaction<DB>>,
}

impl<DB: Marker> Extension<DB> {
    pub(crate) fn new(state: State<DB>) -> Self {
        let slot = Slot::new(LazyTransaction::new(state));
        Self { slot }
    }

    pub(crate) async fn acquire(&self) -> Result<Lease<LazyTransaction<DB>>, Error> {
        let mut tx = self.slot.lease().ok_or(Error::OverlappingExtractors)?;
        tx.acquire().await?;

        Ok(tx)
    }

    pub(crate) async fn resolve(&self) -> Result<(), sqlx::Error> {
        if let Some(tx) = self.slot.lease() {
            tx.steal().resolve().await?;
        }
        Ok(())
    }
}

impl<DB: Marker> Clone for Extension<DB> {
    fn clone(&self) -> Self {
        Self {
            slot: self.slot.clone(),
        }
    }
}

/// The lazy transaction.
pub(crate) struct LazyTransaction<DB: Marker>(LazyTransactionState<DB>);

enum LazyTransactionState<DB: Marker> {
    Unacquired {
        state: State<DB>,
    },
    Acquired {
        tx: Transaction<'static, DB::Driver>,
    },
}

impl<DB: Marker> LazyTransaction<DB> {
    fn new(state: State<DB>) -> Self {
        Self(LazyTransactionState::Unacquired { state })
    }

    pub(crate) fn as_ref(&self) -> &Transaction<'static, DB::Driver> {
        match &self.0 {
            LazyTransactionState::Unacquired { .. } => {
                panic!("BUG: exposed unacquired LazyTransaction")
            }
            LazyTransactionState::Acquired { tx } => tx,
        }
    }

    pub(crate) fn as_mut(&mut self) -> &mut Transaction<'static, DB::Driver> {
        match &mut self.0 {
            LazyTransactionState::Unacquired { .. } => {
                panic!("BUG: exposed unacquired LazyTransaction")
            }
            LazyTransactionState::Acquired { tx } => tx,
        }
    }

    async fn acquire(&mut self) -> Result<(), sqlx::Error> {
        match &self.0 {
            LazyTransactionState::Unacquired { state } => {
                let tx = state.transaction().await?;
                self.0 = LazyTransactionState::Acquired { tx };
                Ok(())
            }
            LazyTransactionState::Acquired { .. } => Ok(()),
        }
    }

    pub(crate) async fn resolve(self) -> Result<(), sqlx::Error> {
        match self.0 {
            LazyTransactionState::Unacquired { .. } => Ok(()),
            LazyTransactionState::Acquired { tx } => tx.commit().await,
        }
    }

    pub(crate) async fn commit(self) -> Result<(), sqlx::Error> {
        match self.0 {
            LazyTransactionState::Unacquired { .. } => {
                panic!("BUG: tried to commit unacquired transaction")
            }
            LazyTransactionState::Acquired { tx } => tx.commit().await,
        }
    }
}
