//! Tests for the client-side input systems (src/client.rs).

#![cfg(feature = "client")]

mod support;
#[path = "support/hist.rs"]
mod support_hist;

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind::{Resimulating, RollbackTarget};
use bevy_rewind_input::{
    HistoryFor, InputAuthority, InputHistory,
    client::{load_inputs, receive_inputs, send_input_messages, store_inputs},
};
use support::{A, Tick};
use support_hist::hist;

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

    // Entities with InputAuthority should have history written
    let e = app.world().entity(e1);
    assert_eq!(hist(5, [A(4)]), *e.get::<InputHistory<A>>().unwrap());
    // ... and the live component must survive the store untouched: it is a
    // snapshot the capture rewrites, and fields the capture only writes
    // conditionally (press counters, slewed axes) carry their value across
    // ticks. A `mem::take` here once reverted a press counter to default
    // the tick after a press — a phantom edge downstream.
    assert_eq!(A(4), *e.get::<A>().unwrap());

    // Entities without should not
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
    // An authority body whose history has never been fed (e.g. before its
    // first stored step) has nothing to ship.
    app.world_mut().spawn((hist::<A>(0, []), InputAuthority));

    app.update();

    let mut messages = app
        .world()
        .resource::<Messages<InputHistory<A>>>()
        .iter_current_update_messages();
    // An update was sent for the entity with authority
    assert_eq!(Some(&hist(5, [A(2)])), messages.next());
    // But not for other entities
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

    // Entities with InputAuthority should be untouched if the history is empty
    let e = app.world().entity(e1);
    assert_eq!(A(15), *e.get::<A>().unwrap());

    // Entities with InputAuthority should load history
    let e = app.world().entity(e2);
    assert_eq!(A(2), *e.get::<A>().unwrap());
    let e = app.world().entity(e3);
    assert_eq!(A(1), *e.get::<A>().unwrap());
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

    // The target entity needs to have history written. The future gap at
    // tick 6 (offsets 0 and 2 sent, 1 missing) repeats the last known input
    // A(3) rather than defaulting.
    let actual = app.world().entity(e1).get::<InputHistory<A>>();
    let expected = hist(1, [A(1), A(1), A(1), A(2), A(3), A(3), A(4)]);
    assert_eq!(Some(&expected), actual);

    // Other entities need to stay untouched
    let actual = app.world().entity(e2).get::<InputHistory<A>>();
    let expected = hist(0, []);
    assert_eq!(Some(&expected), actual);
}

/// The server's broadcast of a client's *own* body history is a stale echo:
/// it can only repeat what the server has consumed, lagging the live record
/// by the full round trip. Accepting it overwrites genuinely recorded inputs
/// with the server's input-repeat (zeros over ticks still in flight) and
/// requests a bogus rollback to the overwritten tick — the corruption loop
/// that froze a driven body under bursty delivery (see
/// `game/tests/netcode_invariants_proptest.rs`, Schedule band). An
/// `InputAuthority` entity's history must be left untouched.
#[test]
fn receive_input_ignores_echo_for_own_authority_body() {
    let mut app = diverge_app(8);
    // Locally recorded walk: ticks 2..=5 = A(1).
    let local = hist(2, [A(1), A(1), A(1), A(1)]);
    let e = app.world_mut().spawn((local.clone(), InputAuthority)).id();

    // The server echoes its stale view: tick 4 = A(0) (it never saw the walk).
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

/// Build a single-system app exercising the client `receive_inputs`
/// misprediction trigger: a `Tick` source, a `RollbackTarget`, and a body
/// whose `InputHistory` is the target of the broadcast.
fn diverge_app(cur_tick: u32) -> App {
    let mut app = App::new();
    app.add_message::<HistoryFor<A>>()
        .init_resource::<RollbackTarget>()
        .insert_resource(Tick(cur_tick))
        .add_systems(Update, receive_inputs::<A, Tick>);
    app
}

/// A broadcast input asserting a value at an already-extrapolated past tick
/// that EQUALS what input-repeat predicted there must NOT request a rollback —
/// the body did not mispredict. This is the novelty guard that keeps a remote
/// body whose present runs ahead from rolling back every tick of a steady walk.
#[test]
fn receive_input_no_rollback_when_input_matches_prediction() {
    let mut app = diverge_app(8);
    // Mirror extrapolated ticks 2..=5 = A(1) (the repeated walk input).
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    // Stamp 6 fills past tick 6 with A(1) — exactly the repeat. No correction.
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

/// The contrast: a broadcast input that DIFFERS from the repeated prediction
/// at a past tick (the host's "stop" landing while we extrapolated the walk)
/// is a real misprediction and must request a rollback to that tick.
#[test]
fn receive_input_requests_rollback_on_diverging_input() {
    let mut app = diverge_app(8);
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    // Stamp 6 fills past tick 6 with A(0) (stop) — diverges from the predicted A(1).
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

/// A lost broadcast must not plant `T::default()` in a remote body's history.
///
/// The server broadcasts an `InputAuthority` (listen-server host) body's input
/// one tick per message with no redundancy (`server::send_inputs` pushes a
/// single `future` entry), on the unreliable channel. When the message for one
/// tick is lost — or the server's present skips a tick (a lead slew) — the next
/// message's stamp arrives with a gap, and `InputHistory::write` fills the gap
/// with `T::default()`. For a repeating input that is wrong twice over: the gap
/// tick replays as a spurious zero input (a walking body halts and snaps its
/// facing to default for one tick), and the documented "last input drives
/// forever" extrapolation contract says the gap should repeat the last known
/// input instead.
#[test]
fn receive_input_gap_from_lost_message_repeats_not_defaults() {
    let mut app = diverge_app(8);
    // Steady walk received through tick 5.
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    // The tick-6 broadcast was lost; tick 7's arrives (single future entry,
    // exactly the host-body broadcast shape).
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

/// The misprediction target composes with an existing `RollbackTarget` via
/// `min`: a pre-existing earlier target is preserved, a later one is lowered.
#[test]
fn receive_input_rollback_target_takes_min_with_existing() {
    let mut app = diverge_app(8);
    let e = app
        .world_mut()
        .spawn(hist(2, [A(1), A(1), A(1), A(1)]))
        .id();

    // Earlier pre-existing target wins over a later misprediction.
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

    // A later pre-existing target is lowered to the mispredicted tick.
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

/// `store_inputs` on a history stamped ahead of the current tick — the tick
/// source landed below the recorded horizon (e.g. a tick-source reset) — must
/// reset the stale record and re-record from the live input.
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

/// `store_inputs` must be idempotent within a tick: a history already updated
/// at the current tick is left untouched (the capture only records once per
/// real simulation step).
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

/// During a rollback resim the recorded history is the canonical input source:
/// an authority body replays it, an authority body whose history misses the
/// tick keeps its live input (never zeroed mid-resim), and a remote body with
/// no record falls to the default.
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

/// A broadcast for an entity that has no `InputHistory` (e.g. a body despawned
/// locally while its input was in flight) warns and is skipped — later
/// messages in the same batch must still be processed.
#[test]
fn receive_input_skips_entities_without_history() {
    let mut app = diverge_app(8);
    let missing = app.world_mut().spawn_empty().id();
    let tracked = app.world_mut().spawn(InputHistory::<A>::default()).id();

    // Two messages for the un-tracked entity: the warn fires on the first and
    // takes its already-warned arm on the second.
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
