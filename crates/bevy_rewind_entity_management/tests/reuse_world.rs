//! Coverage for the `World`-level entity-reuse API (`EntityManagementWorld`,
//! `EntityManagementEntityWorldMut`, `EntityManagementDeferredWorld`) plus the
//! `SpawnPlugin` bookkeeping that evicts stale reuse entries at
//! `BackToPresent`.

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
    ecs::{entity_disabling::Disabled, world::DeferredWorld},
    prelude::*,
};
use bevy_replicon::prelude::{ClientState, Signature};
use bevy_rewind::{RollbackFrames, RollbackSchedule};
use bevy_rewind_entity_management::{
    Despawned, EntityManagementDeferredWorld, EntityManagementEntityWorldMut,
    EntityManagementWorld, SpawnPlugin, Unspawned,
};
use proptest::prelude::*;

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
struct Payload(u32);

/// An app with the reuse plugin installed, still in the authoritative
/// (`Disconnected`) state.
fn authority_app() -> App {
    let mut app = init_app(10);
    app.add_plugins(SpawnPlugin::<TestReason>::default());
    app
}

/// An app with the reuse plugin installed and a connected client state, so the
/// reuse machinery (rather than plain spawning) is active.
fn client_app() -> App {
    let mut app = authority_app();
    app.world_mut()
        .insert_resource(State::new(ClientState::Connected));
    app
}

#[test]
fn authority_reuse_spawn_spawns_plain_entity() {
    let mut app = authority_app();
    let e = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();

    let entity = app.world().entity(e);
    assert_eq!(entity.get::<Payload>(), Some(&Payload(1)));
    assert!(
        !entity.contains::<Signature>(),
        "authoritative spawns must not get a reuse Signature"
    );
}

#[test]
fn missing_client_state_grants_authority() {
    let mut app = authority_app();
    app.world_mut().remove_resource::<State<ClientState>>();

    let e = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();
    assert!(!app.world().entity(e).contains::<Signature>());
}

#[test]
fn client_reuse_spawn_registers_then_reuses() {
    let mut app = client_app();
    let e1 = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();
    assert!(
        app.world().entity(e1).contains::<Signature>(),
        "client reuse spawns carry a Signature derived from the reason"
    );

    let e2 = app.world_mut().reuse_spawn(TestReason(1), Payload(2)).id();
    app.world_mut().flush();

    assert_eq!(e2, e1, "same reason must reuse the same entity");
    assert_eq!(app.world().entity(e1).get::<Payload>(), Some(&Payload(2)));
}

#[test]
fn client_reuse_spawn_clears_both_lifecycle_markers() {
    let mut app = client_app();
    let e1 = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();
    app.world_mut()
        .entity_mut(e1)
        .insert((Despawned, Unspawned));
    app.world_mut().flush();
    assert!(app.world().entity(e1).contains::<Disabled>());

    let e2 = app.world_mut().reuse_spawn(TestReason(1), Payload(2)).id();
    app.world_mut().flush();

    assert_eq!(e2, e1);
    let entity = app.world().entity(e1);
    assert!(!entity.contains::<Despawned>());
    assert!(!entity.contains::<Unspawned>());
    assert!(
        !entity.contains::<Disabled>(),
        "reusing an entity must re-enable it even when both lifecycle markers were present"
    );
}

#[test]
fn client_reuse_spawn_skips_entity_marked_for_removal() {
    let mut app = client_app();
    let e1 = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();

    // Despawning drops the internal `Reuse` marker, which flags the entity in
    // `ToRemove`; the next reuse for this reason must spawn fresh.
    app.world_mut().despawn(e1);
    let e2 = app.world_mut().reuse_spawn(TestReason(1), Payload(2)).id();

    assert_ne!(e2, e1);
    assert_eq!(app.world().entity(e2).get::<Payload>(), Some(&Payload(2)));
}

#[test]
fn client_reuse_spawn_skips_dead_registered_entity() {
    let mut app = client_app();
    // Register a plain entity (no internal `Reuse` marker), then kill it: the
    // registration outlives the entity without any `ToRemove` flag.
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().register_reuse(TestReason(2), e);
    app.world_mut().despawn(e);

    let e2 = app.world_mut().reuse_spawn(TestReason(2), Payload(2)).id();
    assert_ne!(e2, e);
}

#[test]
fn client_register_reuse_makes_entity_reusable() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().register_reuse(TestReason(3), e);

    let reused = app.world_mut().reuse_spawn(TestReason(3), Payload(3)).id();
    app.world_mut().flush();
    assert_eq!(reused, e);
}

#[test]
fn authority_register_reuse_is_noop() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().register_reuse(TestReason(4), e);

    // Flip to a connected client: nothing was registered, so reuse misses.
    app.world_mut()
        .insert_resource(State::new(ClientState::Connected));
    let spawned = app.world_mut().reuse_spawn(TestReason(4), Payload(4)).id();
    assert_ne!(spawned, e);
}

#[test]
fn deferred_world_register_reuse_makes_entity_reusable() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();

    let mut deferred: DeferredWorld = app.world_mut().into();
    deferred.register_reuse(TestReason(5), e);

    let reused = app.world_mut().reuse_spawn(TestReason(5), Payload(5)).id();
    app.world_mut().flush();
    assert_eq!(reused, e);
}

#[test]
fn deferred_world_register_reuse_is_noop_with_authority() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();

    let mut deferred: DeferredWorld = app.world_mut().into();
    deferred.register_reuse(TestReason(6), e);

    app.world_mut()
        .insert_resource(State::new(ClientState::Connected));
    let spawned = app.world_mut().reuse_spawn(TestReason(6), Payload(6)).id();
    assert_ne!(spawned, e);
}

#[test]
fn world_disable_or_despawn_despawns_with_authority() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().disable_or_despawn(e);
    assert!(!app.world().entities().contains(e));
}

#[test]
fn world_disable_or_despawn_disables_on_client() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().disable_or_despawn(e);
    app.world_mut().flush();

    let entity = app.world().entity(e);
    assert!(entity.contains::<Despawned>());
    assert!(entity.contains::<Disabled>());
}

#[test]
fn world_disable_or_despawn_missing_entity_is_noop() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().despawn(e);
    // Must not panic on a dead entity.
    app.world_mut().disable_or_despawn(e);
}

#[test]
fn entity_world_mut_disable_or_despawn_despawns_with_authority() {
    let mut app = authority_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().entity_mut(e).disable_or_despawn();
    assert!(!app.world().entities().contains(e));
}

#[test]
fn entity_world_mut_disable_or_despawn_disables_on_client() {
    let mut app = client_app();
    let e = app.world_mut().spawn(Payload(0)).id();
    app.world_mut().entity_mut(e).disable_or_despawn();

    let entity = app.world().entity(e);
    assert!(entity.contains::<Despawned>());
    assert!(entity.contains::<Disabled>());
}

#[test]
fn back_to_present_evicts_entries_marked_for_removal() {
    let mut app = client_app();
    let e1 = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();
    app.world_mut().despawn(e1);

    // The clean pass drops the entry (its entity is in `ToRemove`) and then
    // resets the removal set.
    app.world_mut()
        .run_schedule(RollbackSchedule::BackToPresent);

    let e2 = app.world_mut().reuse_spawn(TestReason(1), Payload(2)).id();
    assert_ne!(e2, e1);
    assert!(app.world().entity(e2).contains::<Signature>());
}

proptest! {
    /// A reuse registration lives for `history_size` ticks past its last
    /// touch: cleaning at `clean` evicts the entry exactly when
    /// `clean >= max(t0, touch) + history_size`.
    #[test]
    fn eviction_window_governs_reuse(t0 in 0u32..40, touch in 0u32..40, clean in 0u32..80) {
        let mut app = client_app();
        let history = app.world().resource::<RollbackFrames>().history_size() as u32;

        app.world_mut().insert_resource(TestTick(t0));
        let e1 = app.world_mut().reuse_spawn(TestReason(1), Payload(1)).id();

        // Touch the entry again (possibly at an earlier tick, e.g. during a
        // resim): the recorded last-spawned tick must be the max of both.
        app.world_mut().insert_resource(TestTick(touch));
        let e2 = app.world_mut().reuse_spawn(TestReason(1), Payload(2)).id();
        app.world_mut().flush();
        prop_assert_eq!(e2, e1);

        app.world_mut().insert_resource(TestTick(clean));
        app.world_mut().run_schedule(RollbackSchedule::BackToPresent);

        let e3 = app.world_mut().reuse_spawn(TestReason(1), Payload(3)).id();
        app.world_mut().flush();
        let last = t0.max(touch);
        if clean < last + history {
            prop_assert_eq!(e3, e1, "entry still in window must be reused");
        } else {
            prop_assert_ne!(e3, e1, "entry outside window must be evicted");
        }
    }
}
