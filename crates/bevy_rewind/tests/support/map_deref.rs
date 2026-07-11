//! Deref helper for `Option<Ptr>`.

use bevy::ptr::Ptr;

/// Deref an optional [`Ptr`] to a typed reference
pub trait MapDeref<'a> {
    /// Deref the pointer, if any
    fn deref<T>(self) -> Option<&'a T>;
}

// WARN: This function is actually unsafe, but not marked as such to avoid cluttering the tests
// DO NOT USE THIS OUTSIDE OF TESTS!
impl<'a> MapDeref<'a> for Option<Ptr<'a>> {
    fn deref<T>(self) -> Option<&'a T> {
        self.map(|v| unsafe { v.deref::<T>() })
    }
}
