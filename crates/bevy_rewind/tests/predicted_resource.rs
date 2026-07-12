#[path = "support/sim_tick.rs"]
mod sim_tick;

use sim_tick::Tick;

use std::collections::VecDeque;
use std::fmt::Debug;

use TickData::Missing;
use bevy::ecs::system::RunSystemOnce;
use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::predicted_resource::append_history;
use bevy_rewind::{ResourceHistory, RollbackFrames, TickData, set_store_tick};

#[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug)]
struct A(u8);
#[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug)]
struct B(u8);

fn a(v: u8) -> TickData<A> {
    TickData::Value(A(v))
}
fn b(v: u8) -> TickData<B> {
    TickData::Value(B(v))
}

#[track_caller]
fn list_array<T: Resource + Clone + Copy + Debug, const N: usize>(
    history: &ResourceHistory<T>,
) -> [TickData<T>; N] {
    assert_eq!(N, history.list.len(), "Length mismatch");
    let mut list = [TickData::<T>::Missing; N];
    for (i, item) in history.list.iter().take(N).enumerate() {
        list[i] = *item;
    }
    list
}

fn increment_tick(mut tick: ResMut<Tick>) {
    **tick += 1;
}

fn init_app() -> App {
    let mut app = App::new();
    let max_ticks = RollbackFrames::new(3);
    app.init_resource::<Tick>()
        .insert_resource(max_ticks)
        .add_systems(PreUpdate, set_store_tick::<Tick>)
        .add_systems(Update, (append_history::<A>, append_history::<B>))
        .add_systems(PostUpdate, increment_tick);

    app.insert_resource(A(1))
        .init_resource::<ResourceHistory<A>>()
        .insert_resource(B(2))
        .init_resource::<ResourceHistory<B>>();

    app
}

#[track_caller]
fn assert_lengths(app: &App, len: usize) {
    if let Some(a) = app.world().get_resource::<ResourceHistory<A>>() {
        assert_eq!(len, a.list.len(), "Length does not match");
    }
    if let Some(b) = app.world().get_resource::<ResourceHistory<B>>() {
        assert_eq!(len, b.list.len(), "Length does not match");
    }
}

#[track_caller]
fn assert_capacity(app: &App, cap: usize) {
    if let Some(a) = app.world().get_resource::<ResourceHistory<A>>() {
        assert_eq!(cap, a.list.capacity(), "Capacity does not match");
    }
    if let Some(b) = app.world().get_resource::<ResourceHistory<B>>() {
        assert_eq!(cap, b.list.capacity(), "Capacity does not match");
    }
}

fn increment_resources(app: &mut App) {
    if let Some(mut a) = app.world_mut().get_resource_mut::<A>() {
        **a += 1;
    }
    if let Some(mut b) = app.world_mut().get_resource_mut::<B>() {
        **b += 1;
    }
}

#[test]
fn history_appends() {
    let mut app = init_app();

    for length in [0, 1] {
        assert_lengths(&app, length);
        app.update();
        increment_resources(&mut app);
        assert_lengths(&app, length + 1);
    }

    let hist_a = app.world().resource::<ResourceHistory<A>>();
    assert_eq!([a(1), a(2)], list_array(hist_a));
    let hist_b = app.world().resource::<ResourceHistory<B>>();
    assert_eq!([b(2), b(3)], list_array(hist_b));
}

#[test]
fn history_removes_and_reinserts() {
    let mut app = init_app();

    assert_lengths(&app, 0);

    app.update();
    assert_lengths(&app, 1);

    increment_resources(&mut app);
    app.world_mut().remove_resource::<A>();
    app.update();
    assert_lengths(&app, 2);

    increment_resources(&mut app);
    app.world_mut().insert_resource(A(3));
    app.world_mut().remove_resource::<B>();
    app.update();
    assert_lengths(&app, 3);

    let hist_a = app.world().resource::<ResourceHistory<A>>();
    assert_eq!([a(1), TickData::Removed, a(3)], list_array(hist_a));
    let hist_b = app.world().resource::<ResourceHistory<B>>();
    assert_eq!([b(2), b(3), TickData::Removed], list_array(hist_b));
}

#[test]
fn history_wraps() {
    let mut app = init_app();

    for length in [1, 2, 3, 4, 5, 5, 5] {
        app.update();
        assert_lengths(&app, length);
        increment_resources(&mut app);
    }

    let hist_a = app.world().resource::<ResourceHistory<A>>();
    assert_eq!([a(3), a(4), a(5), a(6), a(7)], list_array(hist_a));
}

#[test]
fn history_resizes_to_match_rollback_frames() {
    let mut app = init_app();

    app.update();
    assert_lengths(&app, 1);
    assert_capacity(&app, 5);

    *app.world_mut().resource_mut::<RollbackFrames>() = RollbackFrames::new(1);
    for length in [2, 3, 3, 3] {
        app.update();
        assert_lengths(&app, length);
        assert_capacity(&app, 3);
    }

    *app.world_mut().resource_mut::<RollbackFrames>() = RollbackFrames::new(5);
    for length in [4, 5, 6, 7, 7, 7] {
        app.update();
        assert_lengths(&app, length);
        assert_capacity(&app, 7);
    }
}

#[test]
fn fast_forwarded() {
    let mut app = init_app();

    app.update();
    assert_lengths(&app, 1);

    *app.world_mut().resource_mut::<Tick>() = Tick(3);

    for _ in 0..3 {
        increment_resources(&mut app);
    }

    app.update();
    assert_lengths(&app, 4);

    let hist_a = app.world().resource::<ResourceHistory<A>>();
    assert_eq!([a(1), a(1), a(1), a(4)], list_array(hist_a));
}

#[test]
fn fast_forwarded_wraps() {
    let mut app = init_app();

    app.update();
    assert_lengths(&app, 1);

    *app.world_mut().resource_mut::<Tick>() = Tick(10);

    for _ in 0..10 {
        increment_resources(&mut app);
    }

    app.update();
    assert_lengths(&app, 5);

    let hist_a = app.world().resource::<ResourceHistory<A>>();
    assert_eq!([a(1), a(1), a(1), a(1), a(11)], list_array(hist_a));
}

#[test]
fn get() {
    let mut history = ResourceHistory {
        list: VecDeque::from([a(5), a(6), TickData::Removed, a(8)]),
        last_tick: 6,
    };

    assert_eq!(&a(5), history.get(RepliconTick::new(3)));
    assert_eq!(&a(6), history.get(RepliconTick::new(4)));
    assert_eq!(&TickData::Removed, history.get(RepliconTick::new(5)));
    assert_eq!(&a(8), history.get(RepliconTick::new(6)));

    assert_eq!(&Missing, history.get(RepliconTick::new(1)));
    assert_eq!(&Missing, history.get(RepliconTick::new(2)));

    assert_eq!(&Missing, history.get(RepliconTick::new(7)));
    assert_eq!(&Missing, history.get(RepliconTick::new(2589)));

    history.list[0] = TickData::Removed;
    assert_eq!(&TickData::Removed, history.get(RepliconTick::new(3)));
    assert_eq!(&TickData::Removed, history.get(RepliconTick::new(1)));
}

#[test]
fn clean() {
    let original = ResourceHistory {
        list: VecDeque::from([a(5), a(6), a(7)]),
        last_tick: 5,
    };

    for tick in [Tick(1), Tick(2)] {
        let mut history = original.clone();
        history.clean(tick.into());
        assert_eq!(0, history.list.len());
        assert_eq!(RepliconTick::from(tick).get(), history.last_tick);
    }

    for tick in [Tick(3), Tick(4), Tick(5)] {
        let mut history = original.clone();
        history.clean(tick.into());
        assert_eq!(3 - (5 - *tick as usize), history.list.len());
        assert_eq!(RepliconTick::from(tick).get(), history.last_tick);
    }

    for tick in [Tick(6), Tick(2589)] {
        let mut history = original.clone();
        history.clean(tick.into());
        assert_eq!(3, history.list.len());
        assert_eq!(5, history.last_tick);
    }
}

#[test]
fn keep_one() {
    let mut history = ResourceHistory {
        list: VecDeque::from([a(5), a(6), a(7)]),
        last_tick: 5,
    };
    assert_eq!(3, history.list.len());
    assert_eq!(5, history.last_tick);

    history.keep_one();

    assert_eq!(1, history.list.len());
    assert_eq!(3, history.last_tick);

    history.keep_one();

    assert_eq!(1, history.list.len());
    assert_eq!(3, history.last_tick);
}

#[test]
fn keep_one_empty() {
    let mut history = ResourceHistory::<A> {
        list: VecDeque::new(),
        last_tick: 5,
    };

    history.keep_one();
    assert_eq!(0, history.len());
    assert_eq!(5, history.last_tick);
}

#[test]
fn keep_one_doesnt_get_overridden() {
    let mut app = init_app();
    **app.world_mut().resource_mut::<Tick>() += 3;

    app.update();
    increment_resources(&mut app);
    app.update();

    assert_lengths(&app, 2);

    let world = app.world_mut();
    world.remove_resource::<A>();
    world.resource_mut::<ResourceHistory<A>>().keep_one();
    world.remove_resource::<B>();
    world.resource_mut::<ResourceHistory<B>>().keep_one();

    assert_lengths(&app, 1);

    **app.world_mut().resource_mut::<Tick>() -= 3;

    app.update();
    assert_lengths(&app, 1);
    app.update();
    assert_lengths(&app, 1);

    app.update();
    assert_lengths(&app, 2);

    let history = app.world().resource::<ResourceHistory<A>>();
    assert_eq!([a(1), TickData::Removed], list_array(history));
}

#[test]
fn rollback_frames_are_capped_at_60() {
    assert_eq!(60, RollbackFrames::new(61).max_frames());
    assert_eq!(5, RollbackFrames::new(5).max_frames());
}

#[test]
#[should_panic(expected = "Tick source")]
fn set_store_tick_requires_the_tick_resource() {
    let mut world = World::new();
    let _ = world.run_system_once(set_store_tick::<Tick>);
}
