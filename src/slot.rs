//! An API for transferring ownership of a resource.
//!
//! The [`Slot`]`<T>` and [`Lease`]`<T>` types implement an API for sharing access to a resource
//! `T`. Conceptually, the `Slot<T>` is the "primary" owner of the value, but access can be
//! leased to one other owner through an associated `Lease<T>`.
//!
//! ```
//! use axum_sqlx::slot::Slot;
//!
//! // Create a slot containing a resource.
//! let mut slot = Slot::new("Hello".to_string());
//!
//! // Lease the resource, taking it from the slot.
//! let mut lease = slot.lease().unwrap();
//!
//! std::thread::spawn(move || {
//!     // We have exclusive access to the resource through the lease, which implements `Deref[Mut]`
//!     lease.push_str(", world!");
//!
//!     // By default the value is returned to the slot on drop
//! }).join().unwrap();
//!
//! // The value is back in the slot – but this is not guaranteed!
//! assert_eq!(slot.lease().as_deref().map(|s| s.as_str()), Some("Hello, world!"));
//!
//! // We can also take ownership of the value – though again the slot might be empty
//! assert_eq!(slot.into_inner(), Some("Hello, world!".to_string()));
//! ```
//!
//! The lease can be "stolen" (forgive the cute terminology) to take ownership of the value, leaving
//! the slot empty.
//!
//! ```
//! use axum_sqlx::slot::Slot;
//!
//! let mut slot = Slot::new("Hello".to_string());
//!
//! let lease = slot.lease().unwrap();
//! std::thread::spawn(move || {
//!     // We can steal ownership of the resource, leaving the slot permanently empty
//!     let _: String = lease.steal();
//! }).join().unwrap();
//!
//! // The slot is now permanently empty
//! assert!(slot.lease().is_none());
//! ```
//!
//! It's implemented as a wrapper around a `futures::channel::oneshot` channel, where the `Slot`
//! wraps the `Receiver` and the `Lease` the `Sender` and the value. All methods are non-blocking.

use futures::channel::oneshot;

/// A slot that may contain a value that can be leased.
///
/// See the [module documentation](crate::slot) for more information.
pub struct Slot<T>(oneshot::Receiver<T>);

impl<T> Slot<T> {
    /// Construct a new `Slot` holding the given value.
    pub fn new(value: T) -> Self {
        let (tx, rx) = oneshot::channel();

        // ignore the possible error, which can't happen since we know the channel is empty
        let _ = tx.send(value);

        Self(rx)
    }

    /// Lease the value from the slot, leaving it empty.
    ///
    /// Ownership of the contained value moves to the `Lease` for the duration. The value may return
    /// to the slot when the `Lease` is dropped, or the value may be "stolen", leaving the slot
    /// permanently empty.
    pub fn lease(&mut self) -> Option<Lease<T>> {
        if let Ok(Some(value)) = self.0.try_recv() {
            let (tx, rx) = oneshot::channel();
            self.0 = rx;
            Some(Lease::new(value, tx))
        } else {
            None
        }
    }

    /// Get the inner value from the slot, if any.
    ///
    /// Note that if this returns `Some`, there are no oustanding leases. If it returns `None` then
    /// the value has been leased, and since this consumes the slot the value will be dropped once
    /// the lease is done.
    pub fn into_inner(mut self) -> Option<T> {
        self.0.try_recv().ok().flatten()
    }
}

/// A lease of a value from a [`Slot`].
///
/// See the [module documentation](crate::slot) for more information.
#[derive(Debug)]
pub struct Lease<T> {
    // INVARIANT: must be Some until dropped or stolen
    value: Option<T>,
    // INVARIANT: must be Some until dropped
    slot: Option<oneshot::Sender<T>>,
}

impl<T> Lease<T> {
    fn new(value: T, slot: oneshot::Sender<T>) -> Self {
        Self {
            value: Some(value),
            slot: Some(slot),
        }
    }

    /// Steal the value, meaning it will never return to the slot.
    pub fn steal(mut self) -> T {
        self.value.take().unwrap()
    }
}

impl<T> Drop for Lease<T> {
    fn drop(&mut self) {
        if let Some(value) = self.value.take() {
            let slot = self.slot.take().unwrap();

            // This could fail if the Slot is dropped, in which case we just let the returned value drop
            let _ = slot.send(value);
        }
    }
}

impl<T: std::ops::Deref> Lease<T> {
    /// Converts from `&Lease<T>` to `&T::Target`.
    pub fn as_deref(&self) -> &T::Target {
        self.value.as_deref().unwrap()
    }
}

impl<T: std::ops::DerefMut> Lease<T> {
    /// Converts from `&mut Lease<T>` to `&mut T::Target`.
    pub fn as_deref_mut(&mut self) -> &mut T::Target {
        self.value.as_deref_mut().unwrap()
    }
}

impl<T> AsRef<T> for Lease<T> {
    fn as_ref(&self) -> &T {
        &*self
    }
}

impl<T> AsMut<T> for Lease<T> {
    fn as_mut(&mut self) -> &mut T {
        &mut *self
    }
}

impl<T> std::ops::Deref for Lease<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.value.as_ref().unwrap()
    }
}

impl<T> std::ops::DerefMut for Lease<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.value.as_mut().unwrap()
    }
}
