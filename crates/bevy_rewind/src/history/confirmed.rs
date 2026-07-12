use bevy::{
    ecs::{component::ComponentId, world::unsafe_world_cell::UnsafeWorldCell},
    prelude::*,
};
use bevy_replicon::{
    prelude::{ConfirmedLookup, ConfirmedReplicationSource},
    shared::replicon_tick::RepliconTick,
};

use serde::{Deserialize, Serialize};

use super::component_history::TickData;
use super::predicted::PredictedHistory;

/// Highest tick for which this body's real input was received; beyond it the
/// state is extrapolated. Replicated so an observer refuses to reconcile past it
/// (see `load.rs`) rather than snap to a guess. Absent means no cap.
#[derive(Component, Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConfirmedInputHorizon(pub u32);

/// Requires replicon's [`ConfirmedReplicationSource`] to already exist (added by
/// `ServerPlugin`), i.e. call after `RepliconPlugins` are added.
pub fn install_confirmed_replication_source(app: &mut App) {
    app.world_mut()
        .resource_mut::<ConfirmedReplicationSource>()
        .set(confirmed_lookup);
}

/// # Safety
/// Obeys the [`ConfirmedLookupFn`](bevy_replicon::prelude::ConfirmedLookupFn)
/// contract: read-only world access; any returned `Ptr` borrows the entity's
/// history blob (valid for `'w`) and points to a value of `component`'s type.
pub unsafe fn confirmed_lookup<'w>(
    world: UnsafeWorldCell<'w>,
    entity: Entity,
    component: ComponentId,
    tick: RepliconTick,
) -> ConfirmedLookup<'w> {
    let Ok(entity_cell) = world.get_entity(entity) else {
        return ConfirmedLookup::Live;
    };

    // SAFETY: `PredictedHistory` is read-only here and never mutably aliased during
    // the replication send schedule (PostUpdate `ServerSystems::Send`).
    let Some(history) = (unsafe { entity_cell.get::<PredictedHistory>() }) else {
        return ConfirmedLookup::Live;
    };

    let Some(comp_hist) = history.get(&component) else {
        return ConfirmedLookup::Live;
    };

    match comp_hist.get_latest(tick.get()) {
        TickData::Value(ptr) => ConfirmedLookup::Confirmed(ptr),
        // Withhold rather than leak the unconfirmed live present.
        TickData::Removed | TickData::Missing => ConfirmedLookup::Unconfirmed,
    }
}
