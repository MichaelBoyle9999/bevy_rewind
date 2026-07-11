//! Shared spawn-reason fixture for the entity-reuse tests.

use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_entity_management::SpawnReason;

/// A minimal spawn reason keyed by an integer.
#[derive(PartialEq, Eq, Hash, Debug)]
pub struct TestReason(pub u32);

impl SpawnReason for TestReason {
    fn tick(&self) -> RepliconTick {
        RepliconTick::new(self.0)
    }
}
