use std::sync::Arc;

use parking_lot::{lock_api::ArcMutexGuard, Mutex, RawMutex};
use sqlx::Transaction;

use crate::{Error, Marker, State};

/// The request extension.
pub(crate) struct Extension<DB: Marker> {
    slot: Arc<Mutex<LazyTransaction<DB>>>,
}

impl<DB: Marker> Extension<DB> {
    pub(crate) fn new(state: State<DB>) -> Self {
        let slot = Arc::new(Mutex::new(LazyTransaction::new(state)));
        Self { slot }
    }

    pub(crate) async fn acquire(
        &self,
    ) -> Result<ArcMutexGuard<RawMutex, LazyTransaction<DB>>, Error> {
        let mut tx = self
            .slot
            .try_lock_arc()
            .ok_or(Error::OverlappingExtractors)?;
        tx.acquire().await?;

        Ok(tx)
    }

    pub(crate) async fn resolve(&self) -> Result<(), sqlx::Error> {
        if let Some(mut tx) = self.slot.try_lock_arc() {
            tx.resolve().await?;
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
    Resolved,
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
            LazyTransactionState::Resolved => panic!("BUG: exposed resolved LazyTransaction"),
        }
    }

    pub(crate) fn as_mut(&mut self) -> &mut Transaction<'static, DB::Driver> {
        match &mut self.0 {
            LazyTransactionState::Unacquired { .. } => {
                panic!("BUG: exposed unacquired LazyTransaction")
            }
            LazyTransactionState::Acquired { tx } => tx,
            LazyTransactionState::Resolved => panic!("BUG: exposed resolved LazyTransaction"),
        }
    }

    async fn acquire(&mut self) -> Result<(), Error> {
        match &self.0 {
            LazyTransactionState::Unacquired { state } => {
                let tx = state.transaction().await?;
                self.0 = LazyTransactionState::Acquired { tx };
                Ok(())
            }
            LazyTransactionState::Acquired { .. } => Ok(()),
            LazyTransactionState::Resolved => Err(Error::OverlappingExtractors),
        }
    }

    pub(crate) async fn resolve(&mut self) -> Result<(), sqlx::Error> {
        match std::mem::replace(&mut self.0, LazyTransactionState::Resolved) {
            LazyTransactionState::Unacquired { .. } | LazyTransactionState::Resolved => Ok(()),
            LazyTransactionState::Acquired { tx } => tx.commit().await,
        }
    }

    pub(crate) async fn commit(&mut self) -> Result<(), sqlx::Error> {
        match std::mem::replace(&mut self.0, LazyTransactionState::Resolved) {
            LazyTransactionState::Unacquired { .. } => {
                panic!("BUG: tried to commit unacquired transaction")
            }
            LazyTransactionState::Acquired { tx } => tx.commit().await,
            LazyTransactionState::Resolved => panic!("BUG: tried to commit resolved transaction"),
        }
    }
}
