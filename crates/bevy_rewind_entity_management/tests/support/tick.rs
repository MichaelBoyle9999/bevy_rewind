use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

#[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug, Default)]
pub struct TestTick(pub u32);

impl From<RepliconTick> for TestTick {
    fn from(v: RepliconTick) -> Self {
        Self(v.get())
    }
}

impl From<TestTick> for RepliconTick {
    fn from(v: TestTick) -> Self {
        RepliconTick::new(v.0)
    }
}
