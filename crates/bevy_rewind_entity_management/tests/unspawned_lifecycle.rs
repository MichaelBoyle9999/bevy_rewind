//! Lifecycle coverage for the `Unspawned`/`Despawned` disable-and-reenable
//! machinery: the shared `reenable` hook only drops `Disabled` once *both*
//! lifecycle markers are gone, entities that never re-reach their spawn tick
//! are despawned at `BackToPresent`, and a `PreRollback` pass without a target
//! marks nothing.

#[path = "support/app.rs"]
mod app;
#[path = "support/tick.rs"]
mod tick;

use app::init_app;

use bevy::ecs::entity_disabling::Disabled;
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::{Predicted, RollbackSchedule, RollbackTarget};
use bevy_rewind_entity_management::{Despawned, SpawnedAt, Unspawned};
use proptest::prelude::*;

#[test]
fn pre_rollback_without_target_marks_nothing() {
    let mut app = init_app(10);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();

    // Run PreRollback directly with no rollback target requested.
    assert!(app.world().resource::<RollbackTarget>().is_none());
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);
    app.world_mut().flush();

    assert!(
        !app.world().entity(e).contains::<Unspawned>(),
        "no entity may be marked Unspawned when there is no rollback target"
    );
}

#[test]
fn entity_spawned_after_present_is_despawned_at_back_to_present() {
    // SpawnedAt(20) with present tick 15: resim never crosses the spawn tick,
    // so the entity stays Unspawned through the whole rollback and must be
    // despawned by `BackToPresent`.
    let mut app = init_app(15);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    app.world_mut()
        .entity_mut(e)
        .insert(SpawnedAt(RepliconTick::new(20)));

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(5));
    app.update();

    assert!(
        !app.world().entities().contains(e),
        "entity spawned after the present must be despawned once rollback returns to present"
    );
}

#[test]
fn removing_unspawned_keeps_disabled_while_despawned_remains() {
    let mut app = init_app(10);
    let world = app.world_mut();
    let e = world.spawn((Despawned, Unspawned)).id();
    world.flush();

    world.entity_mut(e).remove::<Unspawned>();
    world.flush();

    let entity = world.entity(e);
    assert!(entity.contains::<Despawned>());
    assert!(
        entity.contains::<Disabled>(),
        "Disabled must be retained while the entity is still Despawned"
    );
}

#[test]
fn removing_despawned_keeps_disabled_while_unspawned_remains() {
    let mut app = init_app(10);
    let world = app.world_mut();
    let e = world.spawn((Despawned, Unspawned)).id();
    world.flush();

    world.entity_mut(e).remove::<Despawned>();
    world.flush();

    let entity = world.entity(e);
    assert!(entity.contains::<Unspawned>());
    assert!(
        entity.contains::<Disabled>(),
        "Disabled must be retained while the entity is still Unspawned"
    );

    // Dropping the last marker re-enables the entity.
    world.entity_mut(e).remove::<Unspawned>();
    world.flush();
    assert!(
        !world.entity(e).contains::<Disabled>(),
        "Disabled must be removed once the last lifecycle marker is gone"
    );
}

#[test]
fn removing_sole_despawned_marker_reenables() {
    let mut app = init_app(10);
    let world = app.world_mut();
    let e = world.spawn(Despawned).id();
    world.flush();
    assert!(world.entity(e).contains::<Disabled>());

    world.entity_mut(e).remove::<Despawned>();
    world.flush();

    assert!(
        !world.entity(e).contains::<Disabled>(),
        "Disabled must be removed when the only lifecycle marker is removed"
    );
}

proptest! {
    /// After a rollback to `target`, an entity stamped `SpawnedAt(spawn)` must
    /// survive (re-enabled) exactly when its spawn tick is at or before the
    /// present tick; later stamps never re-enable and are culled at
    /// `BackToPresent`.
    #[test]
    fn rollback_preserves_entities_spawned_at_or_before_present(
        spawn in 0u32..=30,
        target in 1u32..=15,
    ) {
        const PRESENT: u32 = 15;
        let mut app = init_app(PRESENT);
        let e = app.world_mut().spawn(Predicted).id();
        app.update();
        app.world_mut()
            .entity_mut(e)
            .insert(SpawnedAt(RepliconTick::new(spawn)));

        **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(target));
        app.update();

        let alive = app.world().entities().contains(e);
        prop_assert_eq!(
            alive,
            spawn <= PRESENT,
            "spawn={} target={} present={}", spawn, target, PRESENT
        );
        if alive {
            let entity = app.world().entity(e);
            prop_assert!(!entity.contains::<Unspawned>());
            prop_assert!(!entity.contains::<Disabled>());
        }
    }
}
