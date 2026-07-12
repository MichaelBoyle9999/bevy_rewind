#![cfg(all(feature = "client", feature = "server"))]

#[path = "support/entity_input.rs"]
mod entity_input;
mod support;

use std::time::Duration;

use bevy::{
    ecs::entity::MapEntities, prelude::*, state::app::StatesPlugin, time::TimeUpdateStrategy,
};
use bevy_replicon::prelude::*;
use bevy_rewind::RollbackTarget;
use bevy_rewind_input::{ConfirmedHorizon, HistoryFor, InputHistory, InputQueuePlugin};
use entity_input::E;
use support::{A, Tick};

#[test]
fn plugin_registers_messages_and_both_halves() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        InputQueuePlugin::<A, Tick>::new(FixedUpdate),
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )))
    .insert_resource(Tick(0))
    .init_resource::<RollbackTarget>();
    app.finish();
    app.update();

    assert!(
        app.world().contains_resource::<ConfirmedHorizon>(),
        "the server half must install (it owns ConfirmedHorizon)",
    );
    assert!(
        app.world().contains_resource::<Messages<InputHistory<A>>>(),
        "the client->server input message must be registered",
    );
    assert!(
        app.world().contains_resource::<Messages<HistoryFor<A>>>(),
        "the server->client broadcast message must be registered",
    );
}

#[test]
fn history_for_maps_entities() {
    let mut world = World::new();
    let from = world.spawn_empty().id();
    let to = world.spawn_empty().id();

    let mut message = HistoryFor::<E> {
        entity: from,
        tick: Tick(5).into(),
        past: [(1u8, E(from))].into_iter().collect(),
        future: [(0u8, E(from))].into_iter().collect(),
    };
    message.map_entities(&mut (from, to));

    assert_eq!(
        HistoryFor::<E> {
            entity: to,
            tick: Tick(5).into(),
            past: [(1u8, E(to))].into_iter().collect(),
            future: [(0u8, E(to))].into_iter().collect(),
        },
        message,
        "the body entity and every carried input entity must be remapped",
    );
}
