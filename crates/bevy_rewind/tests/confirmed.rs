#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/comp_b.rs"]
mod comp_b;

use comp_a::A;
use comp_b::B;

use std::num::NonZero;
use std::time::Duration;

use bevy::prelude::*;
use bevy::state::app::StatesPlugin;
use bevy::time::TimeUpdateStrategy;
use bevy_replicon::prelude::{ConfirmedLookup, RepliconPlugins, ServerPlugin};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::history::PredictedHistory;
use bevy_rewind::history::component::HistoryComponent;
use bevy_rewind::history::component_history::ComponentHistory;
use bevy_rewind::history::confirmed::{confirmed_lookup, install_confirmed_replication_source};

fn value_history<T: Component + Clone + PartialEq>(tick: u32, value: T) -> ComponentHistory {
    let mut hist =
        ComponentHistory::from_component(&HistoryComponent::new::<T>(), NonZero::new(5).unwrap());
    unsafe { hist.write(tick, |ptr| *ptr.deref_mut() = value) };
    hist
}

#[test]
fn confirmed_lookup_resolves_each_arm() {
    let mut world = World::new();
    let comp_a = world.register_component::<A>();
    let comp_b = world.register_component::<B>();

    let mut hist = PredictedHistory::default();
    hist.insert(comp_a, value_history(5, A(7)));
    let predicted = world.spawn(hist).id();

    let plain = world.spawn_empty().id();

    let gone = world.spawn_empty().id();
    world.despawn(gone);

    let cell = world.as_unsafe_world_cell();

    assert!(matches!(
        unsafe { confirmed_lookup(cell, gone, comp_a, RepliconTick::new(5)) },
        ConfirmedLookup::Live
    ));
    assert!(matches!(
        unsafe { confirmed_lookup(cell, plain, comp_a, RepliconTick::new(5)) },
        ConfirmedLookup::Live
    ));
    assert!(matches!(
        unsafe { confirmed_lookup(cell, predicted, comp_b, RepliconTick::new(5)) },
        ConfirmedLookup::Live
    ));
    assert!(matches!(
        unsafe { confirmed_lookup(cell, predicted, comp_a, RepliconTick::new(5)) },
        ConfirmedLookup::Confirmed(_)
    ));
    assert!(matches!(
        unsafe { confirmed_lookup(cell, predicted, comp_a, RepliconTick::new(1)) },
        ConfirmedLookup::Unconfirmed
    ));
}

#[test]
fn confirmed_lookup_withholds_removed() {
    let mut world = World::new();
    let comp_a = world.register_component::<A>();

    let mut hist = PredictedHistory::default();
    let mut comp_hist =
        ComponentHistory::from_component(&HistoryComponent::new::<A>(), NonZero::new(5).unwrap());
    comp_hist.mark_removed(5);
    hist.insert(comp_a, comp_hist);
    let entity = world.spawn(hist).id();

    let cell = world.as_unsafe_world_cell();
    assert!(matches!(
        unsafe { confirmed_lookup(cell, entity, comp_a, RepliconTick::new(5)) },
        ConfirmedLookup::Unconfirmed
    ));
}

#[test]
fn install_sets_the_confirmed_source() {
    let mut app = App::new();
    app.add_plugins((
        MinimalPlugins,
        StatesPlugin,
        RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
    ))
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )));
    app.finish();

    install_confirmed_replication_source(&mut app);
}
