//! Shared tick-source resource for the entity-management test suite.

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

/// A simple tick-source resource for driving the plugin in tests.
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
