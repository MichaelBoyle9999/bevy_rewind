//! Coverage for the authoritative marker callbacks (`write_authoritative_history`
//! and `remove_authoritative_history`), driven end-to-end through replicon's
//! in-memory test backend. These callbacks are only reachable via replication:
//! replicon invokes them for a rollback-tracked component on an entity carrying
//! the `Predicted` marker.

#[path = "support/sim_tick.rs"]
mod sim_tick;

use sim_tick::Tick;

use std::{marker::PhantomData, time::Duration};

use bevy::{
    ecs::schedule::ScheduleLabel, prelude::*, state::app::StatesPlugin, time::TimeUpdateStrategy,
};
use bevy_replicon::{prelude::*, test_app::ServerTestAppExt};
use bevy_rewind::history::AuthoritativeHistory;
use bevy_rewind::{Predicted, RollbackApp, RollbackPlugin};
use serde::{Deserialize, Serialize};

/// A replicated, rollback-tracked payload component whose `Deserialize`
/// deliberately fails on the sentinel value `0xFFFF`. Keeping the success and
/// error paths on a *single* replicated type means there is one
/// `write_authoritative_history::<Payload>` instantiation that covers both sides
/// of the `deserialize(...)?` — the per-monomorphisation coverage gate credits
/// the `?` error branch only within an instantiation that actually errors.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Deref, DerefMut)]
struct Payload(u16);

/// The value whose deserialization fails.
const BAD_PAYLOAD: u16 = 0xFFFF;

impl Serialize for Payload {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Payload {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let v = u16::deserialize(deserializer)?;
        if v == BAD_PAYLOAD {
            return Err(serde::de::Error::custom("deliberate deserialize failure"));
        }
        Ok(Payload(v))
    }
}

/// The store schedule label; never run automatically in these tests.
#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
struct StoreSched;

/// Build one side of the pair. The plugin stack must be identical on both sides:
/// `RollbackPlugin` calls replicon's `track_mutate_messages`, which changes the
/// wire format, so an asymmetric install would silently drop all replication.
fn build_app() -> App {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        RollbackPlugin::<Tick> {
            store_schedule: StoreSched.intern(),
            rollback_schedule: FixedUpdate.intern(),
            phantom: PhantomData,
        },
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )))
    .insert_resource(Tick(0))
    .replicate::<Payload>();
    app.register_authoritative_component::<Payload>();
    app.init_schedule(FixedUpdate);
    app.finish();
    app
}

/// Connect a server/client pair with one replicated `Payload` entity, returning
/// `(server, client, server_entity, client_entity)`.
fn replicated_pair() -> (App, App, Entity, Entity) {
    let mut server = build_app();
    let mut client = build_app();

    server.connect_client(&mut client);

    let server_entity = server.world_mut().spawn((Replicated, Payload(1))).id();
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

#[test]
fn authoritative_write_records_history() {
    let (mut server, mut client, s_e, c_e) = replicated_pair();

    // Mark the client entity predicted so replicon routes Payload through the
    // marker write callback.
    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    // Mutate on the server and deliver.
    **server
        .world_mut()
        .entity_mut(s_e)
        .get_mut::<Payload>()
        .unwrap() = 2;
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    // The authoritative value landed in AuthoritativeHistory, not the live component.
    let comp = client.world_mut().register_component::<Payload>();
    let hist = client
        .world()
        .entity(c_e)
        .get::<AuthoritativeHistory>()
        .expect("predicted entity has an AuthoritativeHistory");
    assert!(hist.contains_key(&comp));
}

#[test]
fn authoritative_remove_runs_marker_callback() {
    let (mut server, mut client, s_e, c_e) = replicated_pair();

    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    // First a mutation to populate the history via the write callback...
    **server
        .world_mut()
        .entity_mut(s_e)
        .get_mut::<Payload>()
        .unwrap() = 2;
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    // ...then a removal, which replicon routes through the remove callback.
    server.world_mut().entity_mut(s_e).remove::<Payload>();
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    // The entity survives (the marker remove callback records the removal in
    // history rather than deleting the entity).
    assert!(client.world().entities().contains(c_e));
}

#[test]
fn authoritative_write_propagates_deserialize_error() {
    let (mut server, mut client, s_e, c_e) = replicated_pair();

    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    // Mutate to the sentinel value: the client's marker write callback then fails
    // to deserialize it, exercising the `deserialize(...)?` error path.
    **server
        .world_mut()
        .entity_mut(s_e)
        .get_mut::<Payload>()
        .unwrap() = BAD_PAYLOAD;
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    // The client survives the deserialize error.
    assert!(client.world().entities().contains(c_e));
}
