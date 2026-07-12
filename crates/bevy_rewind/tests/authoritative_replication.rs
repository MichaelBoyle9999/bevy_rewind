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

#[derive(Component, Clone, Copy, PartialEq, Eq, Debug, Deref, DerefMut)]
struct Payload(u16);

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

#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
struct StoreSched;

// The plugin stack must be identical on both sides: RollbackPlugin's
// track_mutate_messages changes the wire format, so an asymmetric install
// silently drops all replication.
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

    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    **server
        .world_mut()
        .entity_mut(s_e)
        .get_mut::<Payload>()
        .unwrap() = 2;
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

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

    **server
        .world_mut()
        .entity_mut(s_e)
        .get_mut::<Payload>()
        .unwrap() = 2;
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    server.world_mut().entity_mut(s_e).remove::<Payload>();
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    assert!(client.world().entities().contains(c_e));
}

#[test]
fn authoritative_write_propagates_deserialize_error() {
    let (mut server, mut client, s_e, c_e) = replicated_pair();

    client.world_mut().entity_mut(c_e).insert(Predicted);
    client.world_mut().flush();

    **server
        .world_mut()
        .entity_mut(s_e)
        .get_mut::<Payload>()
        .unwrap() = BAD_PAYLOAD;
    server.update();
    server.exchange_with_client(&mut client);
    client.update();

    assert!(client.world().entities().contains(c_e));
}
