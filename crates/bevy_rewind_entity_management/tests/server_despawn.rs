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

// replicon only emits an entity spawn for a non-withheld replicated component;
// a bare `Replicated` entity is never sent to the client.
#[derive(Component, Clone, Copy, Serialize, Deserialize)]
struct Tagged;

#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
struct StoreSched;

// Both sides must install the same stack: `RollbackPlugin` calls replicon's
// `track_mutate_messages`, which changes the wire format, so an asymmetric
// install can't parse the peer's messages.
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
    let (mut server, mut client, s_e, c_e) = replicated_pair(1_000);
    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    for _ in 0..3 {
        server.update();
    }
    deliver_despawn(&mut server, &mut client, s_e);

    assert!(client.world().entities().contains(c_e));
    assert!(!client.world().entity(c_e).contains::<Despawned>());
    assert!(!client.world().entity(c_e).contains::<Disabled>());

    client.world_mut().insert_resource(TestTick(0));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    assert!(!client.world().entity(c_e).contains::<Despawned>());

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

    client.world_mut().insert_resource(TestTick(history));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    assert!(
        client.world().entities().contains(c_e),
        "entity must be retained while still within the history window"
    );

    client.world_mut().insert_resource(TestTick(history + 100));
    client.world_mut().run_schedule(StoreSched);
    client.world_mut().flush();
    assert!(
        !client.world().entities().contains(c_e),
        "entity must be despawned once outside the history window"
    );
}
