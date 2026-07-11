//! Shorthand [`RepliconTick`] constructor.

use bevy_replicon::shared::replicon_tick::RepliconTick;

/// Shorthand for [`RepliconTick::new`]
pub fn r_tick(tick: u32) -> RepliconTick {
    RepliconTick::new(tick)
}
