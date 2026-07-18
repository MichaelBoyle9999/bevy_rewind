#![cfg(feature = "client")]

mod support;
#[path = "support/hist.rs"]
mod support_hist;
#[path = "support/ramp.rs"]
mod support_ramp;

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::{Resimulating, RollbackTarget};
use bevy_rewind_input::{
    HistoryFor, InputAuthority, InputHistory,
    client::{load_inputs, receive_inputs, send_input_messages, store_inputs},
};
use support::{A, Tick};
use support_hist::hist;
use support_ramp::Ramp;

#[test]
fn stores_inputs_with_authority() {
    let mut app = App::new();
    app.add_systems(Update, store_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e1 = app
        .world_mut()
        .spawn((A(4), InputHistory::<A>::default(), InputAuthority))
        .id();
    let e2 = app
        .world_mut()
        .spawn((A(5), InputHistory::<A>::default()))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(hist(5, [A(4)]), *e.get::<InputHistory<A>>().unwrap());
    assert_eq!(A(4), *e.get::<A>().unwrap());

    let e = app.world().entity(e2);
    assert_eq!(hist(0, []), *e.get::<InputHistory<A>>().unwrap());
}

#[test]
fn sends_inputs_with_authority() {
    let mut app = App::new();
    app.add_message::<InputHistory<A>>()
        .add_systems(Update, send_input_messages::<A>)
        .insert_resource(Tick(5));
    app.world_mut().spawn((hist(5, [A(2)]), InputAuthority));
    app.world_mut().spawn(hist(5, [A(1)]));
    app.world_mut().spawn(hist::<A>(0, []));
    app.world_mut().spawn((hist::<A>(0, []), InputAuthority));

    app.update();

    let mut messages = app
        .world()
        .resource::<Messages<InputHistory<A>>>()
        .iter_current_update_messages();
    assert_eq!(Some(&hist(5, [A(2)])), messages.next());
    assert_eq!(None, messages.next());
}

#[test]
fn loads_inputs_without_authority() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e1 = app
        .world_mut()
        .spawn((A(15), hist(2, [A(1), A(2), A(3)]), InputAuthority))
        .id();
    let e2 = app
        .world_mut()
        .spawn((A(0), hist(4, [A(1), A(2), A(3)])))
        .id();
    let e3 = app
        .world_mut()
        .spawn((A(0), hist(5, [A(1), A(2), A(3)])))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(A(15), *e.get::<A>().unwrap());

    let e = app.world().entity(e2);
    assert_eq!(A(2), *e.get::<A>().unwrap());
    let e = app.world().entity(e3);
    assert_eq!(A(1), *e.get::<A>().unwrap());
}

#[test]
fn load_inputs_remote_repeats_last_input_past_history_end() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e = app.world_mut().spawn((A(0), hist(2, [A(7)]))).id();

    app.update();

    assert_eq!(
        A(7),
        *app.world().entity(e).get::<A>().unwrap(),
        "a remote body must repeat (extrapolate) its last known input for a tick \
         past the end of its history, not fall back to the default input",
    );
}

#[test]
fn receive_input_reconstructs_past_window_by_forward_offset() {
    let mut app = App::new();
    app.add_message::<HistoryFor<Ramp>>()
        .init_resource::<RollbackTarget>()
        .insert_resource(Tick(11))
        .add_systems(Update, receive_inputs::<Ramp, Tick>);
    let mut seed = InputHistory::<Ramp>::default();
    seed.write(Tick(8), Ramp(0));
    let e = app.world_mut().spawn(seed).id();

    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(10).into(),
        past: [(2u8, Ramp(0))].into_iter().collect(),
        future: default(),
    });

    app.update();

    assert_eq!(
        Some(&hist(8, [Ramp(0), Ramp(1), Ramp(2)])),
        app.world().entity(e).get::<InputHistory<Ramp>>(),
        "reconstructing the past window from a single anchor must extrapolate it \
         forward across the offset (rt - rrt): tick 8 keeps the anchor, ticks 9 \
         and 10 are that anchor repeated 1 and 2 ticks later",
    );
}

#[test]
fn receive_input_writes_history() {
    let mut app = App::new();
    app.add_message::<HistoryFor<A>>()
        .init_resource::<RollbackTarget>()
        .insert_resource(Tick(8))
        .add_systems(Update, receive_inputs::<A, Tick>);
    let e1 = app.world_mut().spawn(InputHistory::<A>::default()).id();
    let e2 = app.world_mut().spawn(InputHistory::<A>::default()).id();

    app.world_mut().write_message(HistoryFor {
        entity: e1,
        tick: Tick(5).into(),
        past: [(4u8, A(1)), (1, A(2))].into_iter().collect(),
        future: [(0, A(3)), (2, A(4))].into_iter().collect(),
    });

    app.update();

    let actual = app.world().entity(e1).get::<InputHistory<A>>();
    let expected = hist(1, [A(1), A(1), A(1), A(2), A(3), A(3), A(4)]);
    assert_eq!(Some(&expected), actual);

    let actual = app.world().entity(e2).get::<InputHistory<A>>();
    let expected = hist(0, []);
    assert_eq!(Some(&expected), actual);
}

#[test]
fn receive_input_ignores_echo_for_own_authority_body() {
    let mut app = diverge_app(8);
    let local = hist(2, [A(1), A(1), A(1), A(1)]);
    let e = app.world_mut().spawn((local.clone(), InputAuthority)).id();

    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(4).into(),
        past: default(),
        future: [(0u8, A(0))].into_iter().collect(),
    });

    app.update();

    assert_eq!(
        Some(&local),
        app.world().entity(e).get::<InputHistory<A>>(),
        "an InputAuthority body's locally recorded history must not be \
         overwritten by the server's round-tripped echo",
    );
    assert_eq!(
        None,
        **app.world().resource::<RollbackTarget>(),
        "a stale echo of our own input must not request a rollback",
    );
}

fn diverge_app(cur_tick: u32) -> App {
    let mut app = App::new();
    app.add_message::<HistoryFor<A>>()
        .init_resource::<RollbackTarget>()
        .insert_resource(Tick(cur_tick))
        .add_systems(Update, receive_inputs::<A, Tick>);
    app
}

#[test]
fn receive_input_no_rollback_when_input_matches_prediction() {
    let mut app = diverge_app(8);
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(6).into(),
        past: default(),
        future: [(0u8, A(1))].into_iter().collect(),
    });

    app.update();

    assert_eq!(
        None,
        **app.world().resource::<RollbackTarget>(),
        "an input equal to the repeated prediction is not a misprediction",
    );
}

#[test]
fn receive_input_requests_rollback_on_diverging_input() {
    let mut app = diverge_app(8);
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(6).into(),
        past: default(),
        future: [(0u8, A(0))].into_iter().collect(),
    });

    app.update();

    assert_eq!(
        Some(RepliconTick::new(6)),
        **app.world().resource::<RollbackTarget>(),
        "a diverging input must request a rollback to the mispredicted tick",
    );
}

#[test]
fn receive_input_gap_from_lost_message_repeats_not_defaults() {
    let mut app = diverge_app(8);
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(7).into(),
        past: default(),
        future: [(0u8, A(1))].into_iter().collect(),
    });

    app.update();

    let history = app.world().entity(e).get::<InputHistory<A>>().unwrap();
    assert_eq!(
        Some(A(1)),
        history.get(Tick(6), true),
        "the gap tick left by a lost broadcast must repeat the last known input \
         (the steady walk), not fall to T::default() — a defaulted tick replays \
         as a one-tick halt + facing snap on the remote body",
    );
}

#[test]
fn receive_input_rollback_target_takes_min_with_existing() {
    let mut app = diverge_app(8);
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(3));
    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(6).into(),
        past: default(),
        future: [(0u8, A(0))].into_iter().collect(),
    });
    app.update();
    assert_eq!(
        Some(RepliconTick::new(3)),
        **app.world().resource::<RollbackTarget>(),
        "an earlier pre-existing target must not be raised",
    );

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(10));
    app.world_mut().write_message(HistoryFor {
        entity: e,
        tick: Tick(6).into(),
        past: default(),
        future: [(0u8, A(2))].into_iter().collect(),
    });
    app.update();
    assert_eq!(
        Some(RepliconTick::new(6)),
        **app.world().resource::<RollbackTarget>(),
        "a later pre-existing target must be lowered to the mispredicted tick",
    );
}

#[test]
fn store_inputs_resets_history_recorded_in_the_future() {
    let mut app = App::new();
    app.add_systems(Update, store_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e = app
        .world_mut()
        .spawn((A(7), hist(10, [A(1), A(2)]), InputAuthority))
        .id();

    app.update();

    assert_eq!(
        Some(&hist(5, [A(7)])),
        app.world().entity(e).get::<InputHistory<A>>(),
        "a history from the future must be reset and re-seeded at the current tick",
    );
}

#[test]
fn store_inputs_skips_already_recorded_tick() {
    let mut app = App::new();
    app.add_systems(Update, store_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e = app
        .world_mut()
        .spawn((A(7), hist(5, [A(9)]), InputAuthority))
        .id();

    app.update();

    assert_eq!(
        Some(&hist(5, [A(9)])),
        app.world().entity(e).get::<InputHistory<A>>(),
        "a tick that is already recorded must not be overwritten by the live input",
    );
}

#[test]
fn load_inputs_resim_replays_authority_history() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .insert_resource(Resimulating)
        .insert_resource(Tick(5));
    let replayed = app
        .world_mut()
        .spawn((A(0), hist(4, [A(1), A(2)]), InputAuthority))
        .id();
    let missing = app
        .world_mut()
        .spawn((A(42), hist::<A>(0, []), InputAuthority))
        .id();
    let remote_empty = app.world_mut().spawn((A(42), hist::<A>(0, []))).id();

    app.update();

    assert_eq!(
        A(2),
        *app.world().entity(replayed).get::<A>().unwrap(),
        "an authority body must replay its recorded input during resim",
    );
    assert_eq!(
        A(42),
        *app.world().entity(missing).get::<A>().unwrap(),
        "an authority body with no record for the tick keeps its live input",
    );
    assert_eq!(
        A(0),
        *app.world().entity(remote_empty).get::<A>().unwrap(),
        "a remote body with no record falls back to the default input",
    );
}

#[test]
fn receive_input_skips_entities_without_history() {
    let mut app = diverge_app(8);
    let missing = app.world_mut().spawn_empty().id();
    let tracked = app.world_mut().spawn(InputHistory::<A>::default()).id();

    for _ in 0..2 {
        app.world_mut().write_message(HistoryFor {
            entity: missing,
            tick: Tick(5).into(),
            past: default(),
            future: [(0u8, A(1))].into_iter().collect(),
        });
    }
    app.world_mut().write_message(HistoryFor {
        entity: tracked,
        tick: Tick(5).into(),
        past: default(),
        future: [(0u8, A(2))].into_iter().collect(),
    });

    app.update();

    assert_eq!(
        Some(&hist(5, [A(2)])),
        app.world().entity(tracked).get::<InputHistory<A>>(),
        "messages after the skipped entity must still be applied",
    );
    assert_eq!(
        Some(RepliconTick::new(5)),
        **app.world().resource::<RollbackTarget>(),
        "the applied past input must still request its rollback",
    );
}
