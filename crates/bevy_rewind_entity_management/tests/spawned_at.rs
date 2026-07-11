//! Validation for the `SpawnedAt`-stamping observer added by
//! `EntityManagementPlugin`. Piece 1 of the spawn-rollback fix: confirms that
//! adding `Predicted` to an entity stamps the current tick, and that adds during
//! a resim do not overwrite a prior stamp.

#[path = "support/app.rs"]
mod app;
#[path = "support/tick.rs"]
mod tick;

use app::init_app;
use tick::TestTick;

use bevy::{ecs::entity_disabling::Disabled, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::{Predicted, Resimulating, RollbackSchedule, RollbackTarget};
use bevy_rewind_entity_management::{SpawnedAt, Unspawned};

/// Probe captured by an in-rollback system so the test can observe state that
/// only exists transiently inside the rollback machinery (e.g. an entity
/// `Unspawned` for ticks `T_target..S` before resim crosses its spawn tick).
#[derive(Resource, Default, Debug)]
struct UnspawnedAtPostRollback(Vec<Entity>);

#[test]
fn spawned_at_stamped_on_predicted_add() {
    let mut app = init_app(10);

    let e = app.world_mut().spawn(Predicted).id();
    // Observer fires on insertion; the deferred command needs an apply pass.
    app.update();

    let stamped = app
        .world()
        .entity(e)
        .get::<SpawnedAt>()
        .copied()
        .expect("SpawnedAt should be stamped on Predicted-add");
    assert_eq!(stamped, SpawnedAt(RepliconTick::new(10)));
}

#[test]
fn spawned_at_skipped_during_resim() {
    let mut app = init_app(10);
    app.world_mut().insert_resource(Resimulating);

    let e = app.world_mut().spawn(Predicted).id();
    app.update();

    assert!(
        app.world().entity(e).get::<SpawnedAt>().is_none(),
        "SpawnedAt should not be stamped while Resimulating is present"
    );
}

#[test]
fn preexisting_spawned_at_is_not_overwritten() {
    // An entity that already carries a SpawnedAt when Predicted is added (e.g.
    // a replicated stamp) must keep the original tick.
    let mut app = init_app(10);

    let e = app
        .world_mut()
        .spawn((SpawnedAt(RepliconTick::new(3)), Predicted))
        .id();
    app.update();

    assert_eq!(
        app.world().entity(e).get::<SpawnedAt>().copied(),
        Some(SpawnedAt(RepliconTick::new(3))),
        "a pre-existing SpawnedAt must not be overwritten by the observer"
    );
}

#[test]
fn rollback_past_spawn_marks_entity_unspawned() {
    // Entity spawned at tick 10; rollback target tick 5 (LoadFrom=4). The entity
    // should be Unspawned by the start of `Rollback` (before resim begins). Once
    // resim crosses tick 10 it gets re-enabled — that's correct end-state, but
    // here we want to observe the transient disable, so we probe in PostRollback
    // which fires once after the disable and before any resim ticks run.
    let mut app = init_app(15);
    app.init_resource::<UnspawnedAtPostRollback>();
    app.add_systems(
        RollbackSchedule::PostRollback,
        |q: Query<Entity, (With<Unspawned>, Or<(With<Disabled>, Without<Disabled>)>)>,
         mut probe: ResMut<UnspawnedAtPostRollback>| {
            probe.0 = q.iter().collect();
        },
    );

    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    app.world_mut()
        .entity_mut(e)
        .insert(SpawnedAt(RepliconTick::new(10)));

    // Request a rollback to tick 5.
    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(5));
    app.update();

    let probed = &app.world().resource::<UnspawnedAtPostRollback>().0;
    assert!(
        probed.contains(&e),
        "entity (spawned at 10) should be Unspawned in PostRollback when rollback targets tick 5; \
         saw {:?}",
        probed,
    );
}

#[test]
fn rollback_to_spawn_tick_or_later_does_not_unspawn() {
    // SpawnedAt(10), rollback to tick 10 (LoadFrom=9). The entity exists at
    // LoadFrom=9 only if SpawnedAt <= 9. Here SpawnedAt=10, so still Unspawned.
    // Adjust target to 11 → LoadFrom=10 → SpawnedAt=10 ≤ 10 → NOT Unspawned.
    let mut app = init_app(15);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    app.world_mut()
        .entity_mut(e)
        .insert(SpawnedAt(RepliconTick::new(10)));

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(11));
    app.update();

    let entity = app.world().entity(e);
    assert!(
        !entity.contains::<Unspawned>(),
        "entity spawned at 10 should not be Unspawned when rollback targets tick 11 (LoadFrom=10)"
    );
}

#[test]
fn resim_reenables_unspawned_at_spawn_tick() {
    // Rollback past spawn, then drive resim. At resim tick == spawn tick the
    // entity must lose Unspawned (and Disabled via the on_remove hook).
    let mut app = init_app(15);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    app.world_mut()
        .entity_mut(e)
        .insert(SpawnedAt(RepliconTick::new(10)));

    // Trigger a rollback to tick 8. Resim covers ticks 8, 9, 10, 11, 12, 13, 14, 15.
    // At ticks 8 and 9 the entity stays Unspawned/Disabled; at tick 10 it re-enables.
    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(8));
    app.update();

    let entity = app.world().entity(e);
    assert!(
        !entity.contains::<Unspawned>(),
        "entity should have shed Unspawned during resim crossing its spawn tick"
    );
    assert!(
        !entity.contains::<Disabled>(),
        "Disabled should have been removed by the on_remove hook on Unspawned"
    );
}

#[test]
fn spawned_at_preserved_across_resim_readd() {
    // First spawn outside resim: stamp captured.
    let mut app = init_app(10);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    assert_eq!(
        app.world().entity(e).get::<SpawnedAt>().copied(),
        Some(SpawnedAt(RepliconTick::new(10))),
    );

    // Simulate a re-add during resim at a later tick: the original stamp must win.
    app.world_mut().insert_resource(TestTick(25));
    app.world_mut().insert_resource(Resimulating);
    app.world_mut().entity_mut(e).remove::<Predicted>();
    app.update();
    app.world_mut().entity_mut(e).insert(Predicted);
    app.update();

    assert_eq!(
        app.world().entity(e).get::<SpawnedAt>().copied(),
        Some(SpawnedAt(RepliconTick::new(10))),
        "Original SpawnedAt should survive a resim-driven re-add of Predicted"
    );
}
