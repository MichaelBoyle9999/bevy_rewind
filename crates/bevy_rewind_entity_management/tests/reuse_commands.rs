//! Coverage for the `Commands`-level entity-reuse API
//! (`EntityManagementCommands` with the `Spawned` system param) including the
//! deferred `InsertSpawnedEntity`/`UpdateSpawnedEntity` commands.

#[path = "support/app.rs"]
mod app;
#[path = "support/reason.rs"]
mod reason;
#[path = "support/tick.rs"]
mod tick;

use app::init_app;
use reason::TestReason;
use tick::TestTick;

use bevy::{
    ecs::{entity_disabling::Disabled, system::SystemState},
    prelude::*,
};
use bevy_replicon::prelude::{ClientState, Signature};
use bevy_rewind::{RollbackFrames, RollbackSchedule};
use bevy_rewind_entity_management::{
    Despawned, EntityManagementCommands, SpawnPlugin, Spawned, Unspawned,
};

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
struct Payload(u32);

/// An app with the reuse plugin installed, still in the authoritative
/// (`Disconnected`) state.
fn authority_app() -> App {
    let mut app = init_app(10);
    app.add_plugins(SpawnPlugin::<TestReason>::default());
    app
}

/// An app with the reuse plugin installed and a connected client state.
fn client_app() -> App {
    let mut app = authority_app();
    app.world_mut()
        .insert_resource(State::new(ClientState::Connected));
    app
}

/// Drive `Commands::reuse_spawn` through a real system-param fetch and apply
/// the queued commands.
fn run_reuse(world: &mut World, reason: TestReason, payload: Payload) -> Entity {
    let mut state = SystemState::<(Commands, Spawned<TestReason>)>::new(world);
    let entity = {
        let (mut commands, spawned) = state.get_mut(world);
        commands.reuse_spawn(&spawned, reason, payload)
    };
    state.apply(world);
    world.flush();
    entity
}

/// Drive `Commands::register_reuse` through a real system-param fetch.
fn run_register(world: &mut World, reason: TestReason, entity: Entity) {
    let mut state = SystemState::<(Commands, Spawned<TestReason>)>::new(world);
    {
        let (mut commands, spawned) = state.get_mut(world);
        commands.register_reuse(&spawned, reason, entity);
    }
    state.apply(world);
    world.flush();
}

/// Drive `Commands::disable_or_despawn`.
fn run_disable_or_despawn(world: &mut World, entity: Entity) {
    let mut state = SystemState::<Commands>::new(world);
    {
        let mut commands = state.get_mut(world);
        commands.disable_or_despawn(entity);
    }
    state.apply(world);
    world.flush();
}

#[test]
fn authority_commands_spawn_plain_entity() {
    let mut app = authority_app();
    let e = run_reuse(app.world_mut(), TestReason(1), Payload(1));

    let entity = app.world().entity(e);
    assert_eq!(entity.get::<Payload>(), Some(&Payload(1)));
    assert!(!entity.contains::<Signature>());
}

#[test]
fn missing_client_state_grants_commands_authority() {
    let mut app = authority_app();
    app.world_mut().remove_resource::<State<ClientState>>();

    let e = run_reuse(app.world_mut(), TestReason(1), Payload(1));
    assert!(!app.world().entity(e).contains::<Signature>());
}

#[test]
fn client_commands_reuse_miss_then_hit() {
    let mut app = client_app();
    let e1 = run_reuse(app.world_mut(), TestReason(1), Payload(1));
    assert!(app.world().entity(e1).contains::<Signature>());

    // Mark the entity with both lifecycle markers; a reuse must clear them.
    app.world_mut()
        .entity_mut(e1)
        .insert((Despawned, Unspawned));
    app.world_mut().flush();

    let e2 = run_reuse(app.world_mut(), TestReason(1), Payload(2));
    assert_eq!(e2, e1);
    let entity = app.world().entity(e1);
    assert_eq!(entity.get::<Payload>(), Some(&Payload(2)));
    assert!(!entity.contains::<Despawned>());
    assert!(!entity.contains::<Unspawned>());
    assert!(
        !entity.contains::<Disabled>(),
        "reusing an entity must re-enable it even when both lifecycle markers were present"
    );
}

#[test]
fn client_commands_reuse_skips_entity_marked_for_removal() {
    let mut app = client_app();
    let e1 = run_reuse(app.world_mut(), TestReason(1), Payload(1));

    // Despawning flags the entity in `ToRemove`; the next reuse must spawn
    // fresh. The fresh registration also overwrites the stale map entry (the
    // duplicate-insert debug path).
    app.world_mut().despawn(e1);
    let e2 = run_reuse(app.world_mut(), TestReason(1), Payload(2));

    assert_ne!(e2, e1);
    assert_eq!(app.world().entity(e2).get::<Payload>(), Some(&Payload(2)));
}

#[test]
fn client_commands_reuse_recovers_from_dead_registered_entity() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    run_register(app.world_mut(), TestReason(2), e);
    app.world_mut().despawn(e);

    // Registration points at a dead entity with no ToRemove flag: reuse warns
    // and falls back to a fresh spawn.
    let e2 = run_reuse(app.world_mut(), TestReason(2), Payload(2));
    assert_ne!(e2, e);
    assert_eq!(app.world().entity(e2).get::<Payload>(), Some(&Payload(2)));
}

#[test]
fn client_commands_register_reuse_makes_entity_reusable() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    run_register(app.world_mut(), TestReason(3), e);

    let reused = run_reuse(app.world_mut(), TestReason(3), Payload(3));
    assert_eq!(reused, e);
}

#[test]
fn authority_commands_register_reuse_is_noop() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    run_register(app.world_mut(), TestReason(4), e);

    app.world_mut()
        .insert_resource(State::new(ClientState::Connected));
    let spawned = run_reuse(app.world_mut(), TestReason(4), Payload(4));
    assert_ne!(spawned, e);
}

#[test]
fn commands_disable_or_despawn_missing_entity_is_noop() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().despawn(e);
    run_disable_or_despawn(app.world_mut(), e);
}

#[test]
fn commands_disable_or_despawn_despawns_with_authority() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    run_disable_or_despawn(app.world_mut(), e);
    assert!(!app.world().entities().contains(e));
}

#[test]
fn commands_disable_or_despawn_disables_on_client() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    run_disable_or_despawn(app.world_mut(), e);

    let entity = app.world().entity(e);
    assert!(entity.contains::<Despawned>());
    assert!(entity.contains::<Disabled>());
}

#[test]
fn queued_update_for_evicted_entry_is_noop() {
    // A reuse hit queues an `UpdateSpawnedEntity`; if the entry is evicted by
    // the clean pass before the command applies (a rollback completing between
    // queue and flush), the update must be a no-op.
    let mut app = client_app();
    let history = app.world().resource::<RollbackFrames>().history_size() as u32;
    let e1 = run_reuse(app.world_mut(), TestReason(1), Payload(1));

    let world = app.world_mut();
    let mut state = SystemState::<(Commands, Spawned<TestReason>)>::new(world);
    let hit = {
        let (mut commands, spawned) = state.get_mut(world);
        commands.reuse_spawn(&spawned, TestReason(1), Payload(2))
    };
    assert_eq!(hit, e1);

    // Evict the entry before the queued commands apply.
    world.insert_resource(TestTick(10 + history + 1));
    world.run_schedule(RollbackSchedule::BackToPresent);
    state.apply(world);
    world.flush();

    // The queued bundle insert still landed on the (live) entity...
    assert_eq!(world.entity(e1).get::<Payload>(), Some(&Payload(2)));
    // ...but the registration is gone: the next reuse spawns fresh.
    let e2 = run_reuse(world, TestReason(1), Payload(3));
    assert_ne!(e2, e1);
}
