use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

#[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug, Default)]
pub struct Tick(pub u32);

impl From<RepliconTick> for Tick {
    fn from(value: RepliconTick) -> Self {
        Self(value.get())
    }
}

impl From<Tick> for RepliconTick {
    fn from(value: Tick) -> Self {
        RepliconTick::new(value.0)
    }
}
