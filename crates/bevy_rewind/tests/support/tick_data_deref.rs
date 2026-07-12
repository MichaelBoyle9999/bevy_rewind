use bevy::ptr::Ptr;
use bevy_rewind::history::component_history::TickData;

pub trait TickDataDeref {
    fn deref<T>(&self) -> TickData<&T>;
}

// WARN: Actually unsafe, not marked as such to avoid cluttering the tests. Tests only.
impl<'a> TickDataDeref for TickData<Ptr<'a>> {
    fn deref<T>(&self) -> TickData<&T> {
        self.map(|v| unsafe { v.deref::<T>() })
    }
}
