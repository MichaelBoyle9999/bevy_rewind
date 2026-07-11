//! Coverage for the server-driven despawn flow, driven end-to-end through
//! replicon's in-memory test backend: the plugin's replicon despawn override
//! converts a replicated despawn of a `Predicted` entity into
//! `Despawned`-then-disabled retention, and the store-time systems convert
//! future despawn marks and cull entities once they fall out of the rollback
//! history window.

#[path = "support/tick.rs"]
mod tick;

use tick::TestTick;

use std::{marker::PhantomData, time::Duration};

use bevy::{
    ecs::{entity_disabling::Disabled, schedule::ScheduleLabel},
    prelude::*,
    state::app::StatesPlugin,
    time::TimeUpdateStrategy,
};
use bevy_replicon::{prelude::*, test_app::ServerTestAppExt};
use bevy_rewind::{Predicted, RollbackFrames, RollbackPlugin};
use bevy_rewind_entity_management::{Despawned, EntityManagementPlugin};
use serde::{Deserialize, Serialize};

/// A replicated payload component: entity-spawn emission is gated on a
/// non-withheld replicated component, so a bare `Replicated` entity would
/// never be sent to the client.
#[derive(Component, Clone, Copy, Serialize, Deserialize)]
struct Tagged;

/// The store schedule of the client app; run it manually via
/// `world.run_schedule(StoreSched)` to exercise store-time systems at a
/// controlled tick.
#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
struct StoreSched;

/// Build one side of the pair. The plugin stack must be identical on both
/// sides: `RollbackPlugin` calls replicon's `track_mutate_messages`, which
/// changes the replication wire format, so an asymmetric install would make
/// the client silently fail to parse the server's messages.
fn build_app(start_tick: u32) -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        RollbackPlugin::<TestTick> {
            store_schedule: StoreSched.intern(),
            rollback_schedule: FixedUpdate.intern(),
            phantom: PhantomData,
        },
        EntityManagementPlugin::<TestTick>::default(),
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )))
    .insert_resource(TestTick(start_tick))
    .replicate::<Tagged>();
    app.init_schedule(FixedUpdate);
    app.finish();
    app
}

/// Build a connected server/client pair with one replicated entity, returning
/// `(server, client, server_entity, client_entity)`. The client's tick source
/// is seeded at `client_start_tick`.
fn replicated_pair(client_start_tick: u32) -> (App, App, Entity, Entity) {
    let mut server = build_app(0);
    let mut client = build_app(client_start_tick);

    server.connect_client(&mut client);

    let server_entity = server.world_mut().spawn((Replicated, Tagged)).id();
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    let client_entity = client
        .world_mut()
        .query_filtered::<Entity, With<Remote>>()
        .single(client.world())
        .expect("replicated entity should exist on the client");

    (server, client, server_entity, client_entity)
}

/// Despawn the entity server-side and deliver the despawn to the client.
fn deliver_despawn(server: &mut App, client: &mut App, server_entity: Entity) {
    server.world_mut().despawn(server_entity);
    server.update();
    server.exchange_with_client(client);
    client.update();
}

#[test]
fn non_predicted_entity_despawns_immediately() {
    let (mut server, mut client, s_e, c_e) = replicated_pair(0);

    deliver_despawn(&mut server, &mut client, s_e);

    assert!(
        !client.world().entities().contains(c_e),
        "non-predicted entities are despawned outright"
    );
}

#[test]
fn predicted_despawn_at_or_after_current_tick_retains_disabled() {
    // Client tick 0: any real message tick is at/after it, so the despawn
    // lands as immediate Despawned retention.
    let (mut server, mut client, s_e, c_e) = replicated_pair(0);
    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    deliver_despawn(&mut server, &mut client, s_e);

    assert!(client.world().entities().contains(c_e));
    let entity = client.world().entity(c_e);
    assert!(entity.contains::<Despawned>());
    assert!(entity.contains::<Disabled>());
}

#[test]
fn predicted_despawn_below_current_tick_marks_without_disabling() {
    // Client tick far ahead of the server: the message tick is in the past,
    // so only the removal mark lands; the store pass converts it once the
    // tick source reaches the mark.
    let (mut server, mut client, s_e, c_e) = replicated_pair(1_000);
    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    // Advance the server a few ticks so the despawn's message tick is
    // strictly positive.
    for _ in 0..3 {
        server.update();
    }
    deliver_despawn(&mut server, &mut client, s_e);

    assert!(client.world().entities().contains(c_e));
    assert!(!client.world().entity(c_e).contains::<Despawned>());
    assert!(!client.world().entity(c_e).contains::<Disabled>());

    // At a resim tick before the mark, the store pass leaves the entity alone.
    client.world_mut().insert_resource(TestTick(0));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    assert!(!client.world().entity(c_e).contains::<Despawned>());

    // Once the tick reaches the mark, the store pass converts it to Despawned.
    client.world_mut().insert_resource(TestTick(1_000));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    let entity = client.world().entity(c_e);
    assert!(entity.contains::<Despawned>());
    assert!(entity.contains::<Disabled>());
}

#[test]
fn despawned_entity_culled_once_outside_history_window() {
    let (mut server, mut client, s_e, c_e) = replicated_pair(0);
    let history = client.world().resource::<RollbackFrames>().history_size() as u32;
    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    deliver_despawn(&mut server, &mut client, s_e);
    assert!(client.world().entity(c_e).contains::<Despawned>());

    // Still within the history window: retained.
    client.world_mut().insert_resource(TestTick(history));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    assert!(
        client.world().entities().contains(c_e),
        "entity must be retained while still within the history window"
    );

    // Far past the window: culled for good.
    client.world_mut().insert_resource(TestTick(history + 100));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    assert!(
        !client.world().entities().contains(c_e),
        "entity must be despawned once outside the history window"
    );
}
