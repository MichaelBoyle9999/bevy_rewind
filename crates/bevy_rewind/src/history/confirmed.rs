//! Host-side confirmed replication source.
//!
//! Installs a [`ConfirmedLookupFn`](bevy_replicon::prelude::ConfirmedLookupFn)
//! into replicon's [`ConfirmedReplicationSource`] so the authority serializes
//! each predicted entity's *confirmed* value — `PredictedHistory[ServerTick]` —
//! instead of its live component, which (once the host runs its present ahead of
//! the confirmed tick) holds a prediction it must not assert as authoritative.
//!
//! This is the general case of replicon's default "serialize the live component":
//! when the authority's simulation *is* the confirmed tick (lead 0),
//! `PredictedHistory[ServerTick]` equals the live value, so installing the source
//! is behaviour-preserving; the divergence only appears once `ServerTick` lags the
//! host's present.
//!
//! Only rollback-tracked components on entities carrying [`PredictedHistory`] are
//! redirected; every other replicated component (markers, non-predicted entities)
//! falls through to live serialization unchanged. A component with no stored value
//! at `ServerTick` (not yet spawned at that tick, or removed) is *withheld* rather
//! than served from the live present.

use bevy::{
    ecs::{component::ComponentId, world::unsafe_world_cell::UnsafeWorldCell},
    prelude::*,
};
use bevy_replicon::{
    prelude::{ConfirmedLookup, ConfirmedReplicationSource},
    shared::replicon_tick::RepliconTick,
};

use super::component_history::TickData;
use super::predicted::PredictedHistory;

/// Install the confirmed replication source on this app.
///
/// Call on the authority (the listen-server host). On a pure client it is inert —
/// replication sending runs only while the server is `Running` — so installing it
/// unconditionally is safe; the host is the only side whose `collect_changes`
/// consults it.
///
/// Requires replicon's [`ConfirmedReplicationSource`] to already exist (added by
/// `ServerPlugin`), i.e. call after `RepliconPlugins` are added.
pub fn install_confirmed_replication_source(app: &mut App) {
    app.world_mut()
        .resource_mut::<ConfirmedReplicationSource>()
        .set(confirmed_lookup);
}

/// Resolve the confirmed value for one replicated component on one entity at the
/// current `ServerTick`, from the entity's [`PredictedHistory`].
///
/// # Safety
///
/// Obeys the [`ConfirmedLookupFn`](bevy_replicon::prelude::ConfirmedLookupFn)
/// contract: read-only world access; any returned `Ptr` borrows the entity's
/// history blob (valid for `'w`) and points to a value of `component`'s type
/// (the rollback history stores values by their real component type).
unsafe fn confirmed_lookup<'w>(
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
        // Not a predicted entity — serialize live, as usual.
        return ConfirmedLookup::Live;
    };

    let Some(comp_hist) = history.get(&component) else {
        // Replicated but not rollback-tracked (e.g. a marker component): live.
        return ConfirmedLookup::Live;
    };

    match comp_hist.get_latest(tick.get()) {
        TickData::Value(ptr) => ConfirmedLookup::Confirmed(ptr),
        // No confirmed value at or before `ServerTick` (not yet spawned at that
        // tick, or removed): withhold — never leak the unconfirmed live present.
        TickData::Removed | TickData::Missing => ConfirmedLookup::Unconfirmed,
    }
}
