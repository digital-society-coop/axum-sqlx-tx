//! An API for transferring ownership of a resource.
//!
//! The `Slot<T>` and `Lease<T>` types implement an API for sharing access to a resource `T`.
//! Conceptually, the `Slot<T>` is the "primary" owner of the value, and access can be leased to one
//! other owner through an associated `Lease<T>`.
//!
//! It's implemented as a wrapper around an `Arc<Mutex<Option_>>>`, where the `Slot` `take`s the
//! value from the `Option` on lease, and the `Lease` puts it back in on drop.
//!
//! Note that while this is **safe** to use across threads (it is `Send` + `Sync`), concurrently
//! `lease`ing and dropping `Lease`s is not supported. It's intended for use in synchronous
//! scenarios that the compiler thinks may be concurrent, like middleware stacks!

use parking_lot::Mutex;
use std::sync::{Arc, Weak};

/// A slot that may contain a value that can be leased.
///
/// The slot itself is opaque, the only way to see the value (if any) is with `lease` or
/// `into_inner`.
pub(crate) struct Slot<T>(Arc<Mutex<Option<T>>>);

impl<T> Slot<T> {
    /// Construct a new `Slot` holding the given value.
    pub(crate) fn new(value: T) -> Self {
        Self(Arc::new(Mutex::new(Some(value))))
    }

    /// Construct a new `Slot` and an immediately acquired `Lease`.
    ///
    /// This avoids the impossible `None` vs. calling `new()` then `lease()`.
    pub(crate) fn new_leased(value: T) -> (Self, Lease<T>) {
        let mut slot = Self::new(value);
        let lease = slot.lease().expect("BUG: new slot empty");
        (slot, lease)
    }

    /// Lease the value from the slot, leaving it empty.
    ///
    /// Ownership of the contained value moves to the `Lease` for the duration. The value may return
    /// to the slot when the `Lease` is dropped, or the value may be "stolen", leaving the slot
    /// permanently empty.
    pub(crate) fn lease(&mut self) -> Option<Lease<T>> {
        if let Some(value) = self.0.try_lock().and_then(|mut slot| slot.take()) {
            Some(Lease::new(value, Arc::downgrade(&self.0)))
        } else {
            None
        }
    }

    /// Get the inner value from the slot, if any.
    ///
    /// Note that if this returns `Some`, there are no oustanding leases. If it returns `None` then
    /// the value has been leased, and since this consumes the slot the value will be dropped once
    /// the lease is done.
    pub(crate) fn into_inner(self) -> Option<T> {
        self.0.try_lock().and_then(|mut slot| slot.take())
    }
}

/// A lease of a value from a `Slot`.
#[derive(Debug)]
pub(crate) struct Lease<T>(lease::State<T>);

impl<T> Lease<T> {
    fn new(value: T, slot: Weak<Mutex<Option<T>>>) -> Self {
        Self(lease::State::new(value, slot))
    }

    /// Steal the value, meaning it will never return to the slot.
    pub(crate) fn steal(mut self) -> T {
        self.0.steal()
    }
}

impl<T> Drop for Lease<T> {
    fn drop(&mut self) {
        self.0.drop()
    }
}

impl<T> AsRef<T> for Lease<T> {
    fn as_ref(&self) -> &T {
        self.0.as_ref()
    }
}

impl<T> AsMut<T> for Lease<T> {
    fn as_mut(&mut self) -> &mut T {
        self.0.as_mut()
    }
}

impl<T> std::ops::Deref for Lease<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref()
    }
}

impl<T> std::ops::DerefMut for Lease<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut()
    }
}

mod lease {
    use std::sync::Weak;

    use parking_lot::Mutex;

    #[derive(Debug)]
    pub(super) struct State<T>(Inner<T>);

    #[derive(Debug)]
    enum Inner<T> {
        Dropped,
        Stolen,
        Live {
            value: T,
            slot: Weak<Mutex<Option<T>>>,
        },
    }

    impl<T> State<T> {
        pub(super) fn new(value: T, slot: Weak<Mutex<Option<T>>>) -> Self {
            Self(Inner::Live { value, slot })
        }

        pub(super) fn as_ref(&self) -> &T {
            match &self.0 {
                Inner::Dropped | Inner::Stolen => panic!("BUG: LeaseState used after drop/steal"),
                Inner::Live { value, .. } => value,
            }
        }

        pub(super) fn as_mut(&mut self) -> &mut T {
            match &mut self.0 {
                Inner::Dropped | Inner::Stolen => panic!("BUG: LeaseState used after drop/steal"),
                Inner::Live { value, .. } => value,
            }
        }

        pub(super) fn drop(&mut self) {
            match std::mem::replace(&mut self.0, Inner::Dropped) {
                Inner::Dropped => panic!("BUG: LeaseState::drop called twice"),
                Inner::Stolen => {} // nothing to do if the value was stolen
                Inner::Live { value, slot } => {
                    // try to return value to the slot, if it fails just drop value
                    if let Some(slot) = slot.upgrade() {
                        if let Some(mut slot) = slot.try_lock() {
                            assert!(slot.is_none(), "BUG: slot repopulated during lease");
                            *slot = Some(value);
                        }
                    }
                }
            }
        }

        pub(super) fn steal(&mut self) -> T {
            match std::mem::replace(&mut self.0, Inner::Stolen) {
                Inner::Dropped => panic!("BUG: LeaseState::steal called after drop"),
                Inner::Stolen => panic!("BUG: LeaseState::steal called twice"),
                Inner::Live { value, .. } => value,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Slot;

    #[test]
    fn lease_and_return() {
        // Create a slot containing a resource.
        let mut slot = Slot::new("Hello".to_string());

        // Lease the resource, taking it from the slot.
        let mut lease = slot.lease().unwrap();

        std::thread::spawn(move || {
            // We have exclusive access to the resource through the lease, which implements `Deref[Mut]`
            lease.push_str(", world!");

            // By default the value is returned to the slot on drop
        })
        .join()
        .unwrap();

        // The value is now back in the slot
        assert_eq!(
            slot.lease().as_deref().map(|s| s.as_str()),
            Some("Hello, world!")
        );

        // We can also take ownership of the value in the slot (if any)
        assert_eq!(slot.into_inner(), Some("Hello, world!".to_string()));
    }

    #[test]
    fn lease_and_steal() {
        let mut slot = Slot::new("Hello".to_string());

        let lease = slot.lease().unwrap();
        std::thread::spawn(move || {
            // We can steal ownership of the resource, leaving the slot permanently empty
            let _: String = lease.steal();
        })
        .join()
        .unwrap();

        // The slot is now permanently empty
        assert!(slot.lease().is_none());
    }
}
