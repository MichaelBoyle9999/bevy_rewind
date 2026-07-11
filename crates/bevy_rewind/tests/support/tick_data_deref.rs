//! Deref helper for `TickData<Ptr>`.

use bevy::ptr::Ptr;
use bevy_rewind::history::component_history::TickData;

/// Deref a [`TickData`] of [`Ptr`] to typed references
pub trait TickDataDeref {
    /// Deref the pointer, if any
    fn deref<T>(&self) -> TickData<&T>;
}

// WARN: This function is actually unsafe, but not marked as such to avoid cluttering the tests
// DO NOT USE THIS OUTSIDE OF TESTS!
impl<'a> TickDataDeref for TickData<Ptr<'a>> {
    fn deref<T>(&self) -> TickData<&T> {
        self.map(|v| unsafe { v.deref::<T>() })
    }
}
