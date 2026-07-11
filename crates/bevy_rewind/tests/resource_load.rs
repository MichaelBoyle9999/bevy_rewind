//! Tests for predicted resource loading (`src/load.rs`).

use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::load::{load_and_clear_resource_prediction, reinsert_predicted_resource};
use bevy_rewind::{LoadFrom, ResourceHistory, TickData};

#[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug)]
struct A(u8);
fn a(v: u8) -> TickData<A> {
    TickData::Value(A(v))
}

#[test]
fn load_value() {
    let mut world = World::new();
    let predicted = ResourceHistory::<A>::from_list(1, [a(1), a(2), a(3)]);
    world.insert_resource(A(0));
    world.insert_resource(predicted);

    world.insert_resource(LoadFrom(RepliconTick::new(2)));
    world
        .run_system_once(load_and_clear_resource_prediction::<A>)
        .unwrap();

    assert_eq!(&A(2), world.resource::<A>());
}

#[test]
fn remove_and_insert() {
    let mut world = World::new();
    let predicted = ResourceHistory::<A>::from_list(1, [a(1), TickData::Removed, a(3)]);
    world.insert_resource(A(0));
    world.insert_resource(predicted.clone());

    world.insert_resource(LoadFrom(RepliconTick::new(2)));
    world
        .run_system_once(load_and_clear_resource_prediction::<A>)
        .unwrap();

    // The data for tick 2 marked it as removed, so it should not be in the world
    assert_eq!(None, world.get_resource::<A>());

    world.insert_resource(LoadFrom(RepliconTick::new(3)));
    world.insert_resource(predicted); // Reset history since it got cleared
    world
        .run_system_once(load_and_clear_resource_prediction::<A>)
        .unwrap();

    // It's back for tick 3, so it should be inserted again
    assert_eq!(Some(&A(3)), world.get_resource::<A>());
}

#[test]
fn remove_before_history_and_reinsert() {
    let mut world = World::new();
    let predicted = ResourceHistory::<A>::from_list(2, [a(1), a(2)]);
    world.insert_resource(A(0));
    world.insert_resource(predicted);

    world.insert_resource(LoadFrom(RepliconTick::new(0)));
    world
        .run_system_once(load_and_clear_resource_prediction::<A>)
        .unwrap();

    // We are loading data from before the history, so the component should be removed
    assert_eq!(None, world.get_resource::<A>());

    let hist = world.resource::<ResourceHistory<A>>();
    assert_eq!(1, hist.len());

    // The value isn't reinserted on a next frame that is outside of history
    world.insert_resource(LoadFrom(RepliconTick::new(1)));
    world
        .run_system_once(reinsert_predicted_resource::<A>)
        .unwrap();
    assert_eq!(None, world.get_resource::<A>());

    // When we get to the first value, we reinsert the component
    world.insert_resource(LoadFrom(RepliconTick::new(2)));
    world
        .run_system_once(reinsert_predicted_resource::<A>)
        .unwrap();
    assert_eq!(Some(&A(1)), world.get_resource::<A>());
}

#[test]
fn reinsert_leaves_present_resource_untouched() {
    let mut world = World::new();
    let predicted = ResourceHistory::<A>::from_list(1, [a(1)]);
    world.insert_resource(A(9));
    world.insert_resource(predicted);

    world.insert_resource(LoadFrom(RepliconTick::new(1)));
    world
        .run_system_once(reinsert_predicted_resource::<A>)
        .unwrap();

    // The resource was already present, so reinsert returns early and leaves it.
    assert_eq!(Some(&A(9)), world.get_resource::<A>());
}
