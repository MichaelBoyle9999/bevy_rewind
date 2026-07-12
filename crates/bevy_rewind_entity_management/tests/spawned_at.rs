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

#[derive(Resource, Default, Debug)]
struct UnspawnedAtPostRollback(Vec<Entity>);

#[test]
fn spawned_at_stamped_on_predicted_add() {
    let mut app = init_app(10);

    let e = app.world_mut().spawn(Predicted).id();
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
    let mut app = init_app(15);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    app.world_mut()
        .entity_mut(e)
        .insert(SpawnedAt(RepliconTick::new(10)));

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
    let mut app = init_app(10);
    let e = app.world_mut().spawn(Predicted).id();
    app.update();
    assert_eq!(
        app.world().entity(e).get::<SpawnedAt>().copied(),
        Some(SpawnedAt(RepliconTick::new(10))),
    );

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
