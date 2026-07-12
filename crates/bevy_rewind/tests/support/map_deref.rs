use bevy::ptr::Ptr;

pub trait MapDeref<'a> {
    fn deref<T>(self) -> Option<&'a T>;
}

// WARN: Actually unsafe, not marked as such to avoid cluttering the tests. Tests only.
impl<'a> MapDeref<'a> for Option<Ptr<'a>> {
    fn deref<T>(self) -> Option<&'a T> {
        self.map(|v| unsafe { v.deref::<T>() })
    }
}
