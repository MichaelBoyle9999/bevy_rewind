//! Tests for the server-side input systems (src/server.rs).

#![cfg(feature = "server")]

mod support;
#[path = "support/hist.rs"]
mod support_hist;

use bevy::{ecs::schedule::ScheduleLabel, prelude::*, state::app::StatesPlugin};
use bevy_replicon::{prelude::*, shared::replicon_tick::RepliconTick};
use bevy_rewind::{ConfirmedInputHorizon, Resimulating, RollbackTarget};
use bevy_rewind_input::{
    ConfirmedHorizon, HistoryFor, InputAuthority, InputHistory, InputQueue, InputTarget,
    server::{
        InputQueueServerPlugin, load_inputs, receive_inputs, send_inputs, store_authority_inputs,
    },
};
use support::{A, Tick};
use support_hist::hist;

#[test]
fn receives_inputs() {
    let mut app = App::new();

    let e1 = app.world_mut().spawn(InputQueue::<A>::default()).id();
    let e2 = app.world_mut().spawn(InputQueue::<A>::default()).id();
    let e3 = app.world_mut().spawn(InputQueue::<A>::default()).id();
    let e4 = app
        .world_mut()
        .spawn((InputQueue::<A>::default(), InputTarget::all(e3)))
        .id();

    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(5));

    app.world_mut().write_message_batch([
        FromClient {
            client_id: ClientId::Client(e1),
            message: hist(4, [A(1), A(2), A(3)]),
        },
        FromClient {
            client_id: ClientId::Client(e2),
            message: hist(5, [A(1), A(2), A(3)]),
        },
        FromClient {
            client_id: ClientId::Client(e4),
            message: hist(6, [A(1), A(2), A(3)]),
        },
    ]);

    app.update();

    // We should not spawn new entities for the unknown clients
    assert_eq!(
        4,
        app.world_mut()
            .query::<&InputQueue<A>>()
            .iter(app.world())
            .count()
    );

    let [e1, e2, e3, e4] = app.world().get_entity([e1, e2, e3, e4]).unwrap();
    // e1's history starts at tick 4 (one tick in the past relative to
    // cur_tick=5). Under the eager-rollback contract the past tick is
    // accepted into the queue rather than dropped.
    assert_eq!(
        vec![
            &(Tick(4).into(), A(1)),
            &(Tick(5).into(), A(2)),
            &(Tick(6).into(), A(3)),
        ],
        e1.get::<InputQueue<A>>()
            .unwrap()
            .queue()
            .collect::<Vec<_>>()
    );
    assert_eq!(
        vec![
            &(Tick(5).into(), A(1)),
            &(Tick(6).into(), A(2)),
            &(Tick(7).into(), A(3))
        ],
        e2.get::<InputQueue<A>>()
            .unwrap()
            .queue()
            .collect::<Vec<_>>()
    );
    // e3 got a message targeted for e4 because of InputTarget
    assert_eq!(
        vec![
            &(Tick(6).into(), A(1)),
            &(Tick(7).into(), A(2)),
            &(Tick(8).into(), A(3))
        ],
        e3.get::<InputQueue<A>>()
            .unwrap()
            .queue()
            .collect::<Vec<_>>()
    );
    // e4 isn't the target itself and thus received nothing
    assert_eq!(0, e4.get::<InputQueue<A>>().unwrap().queue().count());

    // The past-tick history from e1 (hist(4, ...)) should have requested a
    // rollback to tick 4. The other two messages (hist(5,..) and hist(6,..))
    // are at or in the future and don't request rollback.
    assert_eq!(
        Some(RepliconTick::new(4)),
        **app.world().resource::<RollbackTarget>()
    );
}

/// Multiple past-input messages in one frame collapse to the earliest tick
/// via min, and an already-present (state-confirm-driven) rollback target
/// is preserved when it's earlier than any incoming past input. This is
/// the "compose with other rollback triggers" invariant of `receive_inputs`.
#[test]
fn receive_inputs_takes_min_with_existing_rollback_target() {
    let mut app = App::new();

    let e_late = app.world_mut().spawn(InputQueue::<A>::default()).id();

    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(20));

    // Seed a pre-existing rollback target (as if a state confirm had already
    // requested it). It is *earlier* than any incoming past input, so the
    // min should preserve it.
    **app.world_mut().resource_mut::<RollbackTarget>() = Some(RepliconTick::new(7));

    app.world_mut().write_message_batch([FromClient {
        client_id: ClientId::Client(e_late),
        message: hist(15, [A(1), A(2), A(3)]),
    }]);

    app.update();

    assert_eq!(
        Some(RepliconTick::new(7)),
        **app.world().resource::<RollbackTarget>(),
        "an earlier pre-existing target must not be raised by a later past input",
    );

    // Now write a message whose past tick is earlier than the existing
    // target — the min should lower the target to the new past tick.
    app.world_mut().write_message(FromClient {
        client_id: ClientId::Client(e_late),
        message: hist(3, [A(0), A(0), A(0), A(0)]),
    });
    app.update();

    assert_eq!(
        Some(RepliconTick::new(3)),
        **app.world().resource::<RollbackTarget>(),
        "an earlier incoming past input must lower an existing target",
    );
}

/// A future-only input message (history fully at or after `cur_tick`)
/// must not write to `RollbackTarget`. The eager-rollback path is
/// strictly opt-in on past arrivals; future arrivals follow the legacy
/// "queue and apply on tick advance" path.
#[test]
fn receive_inputs_does_not_request_rollback_for_future_input() {
    let mut app = App::new();
    let e = app.world_mut().spawn(InputQueue::<A>::default()).id();

    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(5));

    app.world_mut().write_message(FromClient {
        client_id: ClientId::Client(e),
        message: hist(5, [A(1), A(2), A(3)]),
    });
    app.update();

    assert_eq!(None, **app.world().resource::<RollbackTarget>());
}

/// Seal guard: a client input stamped at or below the sealed horizon
/// (`ConfirmedHorizon - SEAL_GRACE_TICKS`) is too late to revise authoritative
/// state — the host has already simulated and replicated those ticks — so it must
/// NOT request a rollback. Without the guard the host rewinds below its replicated
/// `ServerTick` and re-ships a corrected value to every client, the asymmetric
/// move→stop overshoot.
#[test]
fn receive_inputs_seals_too_late_input() {
    let mut app = App::new();
    let e = app.world_mut().spawn(InputQueue::<A>::default()).id();
    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        // Horizon 10, grace 2 → sealed floor at tick 8.
        .insert_resource(ConfirmedHorizon(10))
        .insert_resource(Tick(15));

    // Novel past tick 8 == 10 - SEAL_GRACE_TICKS(2): sealed, must be dropped.
    app.world_mut().write_message(FromClient {
        client_id: ClientId::Client(e),
        message: hist(8, [A(1)]),
    });
    app.update();

    assert_eq!(
        None,
        **app.world().resource::<RollbackTarget>(),
        "an input for an already-sealed tick must not request a rollback",
    );
}

/// The complement: an input just *above* the sealed horizon lands in the host's
/// unsealed lead window, so a genuinely-late but not-yet-replicated input must
/// still request a rollback and be applied.
#[test]
fn receive_inputs_rolls_back_unsealed_input() {
    let mut app = App::new();
    let e = app.world_mut().spawn(InputQueue::<A>::default()).id();
    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .insert_resource(ConfirmedHorizon(10))
        .insert_resource(Tick(15));

    // Novel past tick 9 > 8: unsealed, must roll back.
    app.world_mut().write_message(FromClient {
        client_id: ClientId::Client(e),
        message: hist(9, [A(1)]),
    });
    app.update();

    assert_eq!(
        Some(RepliconTick::new(9)),
        **app.world().resource::<RollbackTarget>(),
        "an input above the sealed horizon must still request a rollback",
    );
}

/// With no seal published yet (`ConfirmedHorizon` at its `u32::MAX` default —
/// e.g. before the host's first fixed step), the eager path is unguarded: even a
/// deep past input rolls back, preserving the prior behaviour and the
/// zero-latency depth-1 rollback.
#[test]
fn receive_inputs_unsealed_when_horizon_unset() {
    let mut app = App::new();
    let e = app.world_mut().spawn(InputQueue::<A>::default()).id();
    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(15));

    app.world_mut().write_message(FromClient {
        client_id: ClientId::Client(e),
        message: hist(4, [A(1)]),
    });
    app.update();

    assert_eq!(
        Some(RepliconTick::new(4)),
        **app.world().resource::<RollbackTarget>(),
        "with no seal published the eager path must remain unguarded",
    );
}

#[test]
fn sends_inputs() {
    let mut app = App::new();
    app.add_message::<ToClients<HistoryFor<A>>>()
        .add_systems(Update, send_inputs::<A, Tick>)
        .insert_resource(Tick(5));

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(1)]));
    assert_eq!(Some(A(1)), queue.next(Tick(5)));
    queue.add(Tick(6), &hist(7, [A(3), A(4)]));

    let e1 = app.world_mut().spawn(queue).id();

    app.update();

    let mut messages = app
        .world()
        .resource::<Messages<ToClients<HistoryFor<A>>>>()
        .iter_current_update_messages();
    assert_eq!(
        HistoryFor {
            entity: e1,
            tick: Tick(5).into(),
            past: [(0u8, A(1))].into_iter().collect(),
            future: [(2u8, A(3)), (3, A(4))].into_iter().collect(),
        },
        messages.next().unwrap().message,
    );
    assert!(messages.next().is_none());
}

#[test]
fn loads_inputs_with_queue() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(5));

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(4, [A(0), A(1), A(2)]));
    let e1 = app.world_mut().spawn((A(94), queue)).id();

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(6, [A(1), A(2)]));
    let e2 = app.world_mut().spawn((A(94), queue)).id();

    app.update();

    // These is input so it should be used
    let e = app.world().entity(e1);
    assert_eq!(A(1), *e.get::<A>().unwrap());
    // There is no input for this tick so this entity goes back to default
    let e = app.world().entity(e2);
    assert_eq!(A(0), *e.get::<A>().unwrap());

    app.insert_resource(Tick(6));
    app.update();

    // We load the next input
    let e = app.world().entity(e1);
    assert_eq!(A(2), *e.get::<A>().unwrap());
    // This entity has input now
    let e = app.world().entity(e2);
    assert_eq!(A(1), *e.get::<A>().unwrap());

    app.insert_resource(Tick(7));
    app.update();

    // We repeat an old input
    let e = app.world().entity(e1);
    assert_eq!(A(2), *e.get::<A>().unwrap());
    // This entity has a new input
    let e = app.world().entity(e2);
    assert_eq!(A(2), *e.get::<A>().unwrap());
}

/// A remote (non-authority) body whose queue holds input both at the confirmed
/// horizon and ahead of it (the present) must load the CONFIRMED-tick input — so
/// it extrapolates the present from the confirmed horizon — while an authority
/// body ignores the horizon and replays at the simulated tick (its own input is
/// confirmed by definition). Run under `Resimulating` because that is when an
/// authority body loads at all: during forward simulation its live input is
/// authoritative and never overwritten. This is what makes the host's render of
/// a remote body extrapolate symmetrically with the client's.
#[test]
fn remote_body_loads_at_confirmed_horizon_authority_loads_at_present() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .insert_resource(ConfirmedHorizon(5))
        .insert_resource(Resimulating)
        .insert_resource(Tick(7));

    // Remote body: queue holds ticks 5 (A(10), confirmed), 6, 7 (A(30), present).
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(7), &hist(5, [A(10), A(20), A(30)]));
    let remote = app.world_mut().spawn((A(0), queue)).id();

    // Authority body: same record in its own history; it replays at the
    // simulated tick regardless of the horizon.
    let own = app
        .world_mut()
        .spawn((
            A(0),
            InputQueue::<A>::default(),
            hist(5, [A(10), A(20), A(30)]),
            InputAuthority,
        ))
        .id();

    app.update();

    // Remote clamps to `min(present 7, confirmed 5) = 5` → tick 5's input.
    assert_eq!(A(10), *app.world().entity(remote).get::<A>().unwrap());
    // Authority replays the simulated tick 7's input.
    assert_eq!(A(30), *app.world().entity(own).get::<A>().unwrap());
}

/// During forward simulation an authority body's live input must never be
/// overwritten by the load — the per-tick capture is the source of truth —
/// but its own tick is still consumed from the self-fed queue so the
/// consumed ring (`InputQueue::past`) carries the broadcast redundancy.
#[test]
fn authority_forward_load_keeps_live_input_and_consumes_queue() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(5));

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(9)]));
    let own = app
        .world_mut()
        .spawn((A(42), queue, hist(5, [A(9)]), InputAuthority))
        .id();

    app.update();

    // Live input untouched (the queued A(9) would have clobbered the
    // captured A(42) under the old unconditional assignment)...
    assert_eq!(A(42), *app.world().entity(own).get::<A>().unwrap());
    // ...but the tick was consumed into the redundancy ring.
    assert_eq!(
        vec![&(Tick(5).into(), A(9))],
        app.world()
            .entity(own)
            .get::<InputQueue<A>>()
            .unwrap()
            .past()
            .collect::<Vec<_>>(),
    );
}

/// The listen-server loopback: an `InputAuthority` body's live input is
/// recorded into its own history and fed through `InputQueue::add` — the same
/// entry point a client's `FromClient` message feeds — and `send_inputs` then
/// broadcasts it with the same consumed-ring redundancy a client body gets.
/// Three stored+consumed ticks must broadcast as three `past` entries, so a
/// client can lose up to two consecutive messages without a gap. This replaces
/// the former special case that sent the live input as a single, unprotected
/// `future` entry (one lost datagram = one permanently corrupted tick).
#[test]
fn authority_body_self_feeds_and_broadcasts_with_redundancy() {
    let mut app = App::new();
    app.add_message::<ToClients<HistoryFor<A>>>()
        .init_resource::<ConfirmedHorizon>()
        .add_systems(
            Update,
            (
                store_authority_inputs::<A, Tick>,
                load_inputs::<A, Tick>,
                send_inputs::<A, Tick>,
            )
                .chain(),
        )
        .insert_resource(Tick(5));

    let e = app
        .world_mut()
        .spawn((
            A(1),
            InputHistory::<A>::default(),
            InputQueue::<A>::default(),
            InputAuthority,
        ))
        .id();

    for (tick, value) in [(5, 1), (6, 2), (7, 3)] {
        app.insert_resource(Tick(tick));
        app.world_mut().entity_mut(e).insert(A(value));
        app.update();
    }

    let mut messages = app
        .world()
        .resource::<Messages<ToClients<HistoryFor<A>>>>()
        .iter_current_update_messages();
    assert_eq!(
        HistoryFor {
            entity: e,
            tick: Tick(7).into(),
            past: [(2u8, A(1)), (1, A(2)), (0, A(3))].into_iter().collect(),
            future: default(),
        },
        messages.next().unwrap().message,
        "three self-fed ticks must broadcast as a three-deep past-redundant message",
    );
    assert!(messages.next().is_none());
}

/// The self-feed must not run during a rollback resim: the recorded history is
/// what the resim replays, and re-recording the live `T` mid-resim would
/// corrupt it at the resimulated tick.
#[test]
fn self_feed_is_inert_during_resim() {
    let mut app = App::new();
    app.add_systems(Update, store_authority_inputs::<A, Tick>)
        .insert_resource(Resimulating)
        .insert_resource(Tick(9));

    let recorded = hist(5, [A(1), A(2)]);
    let e = app
        .world_mut()
        .spawn((
            A(42),
            recorded.clone(),
            InputQueue::<A>::default(),
            InputAuthority,
        ))
        .id();

    app.update();

    assert_eq!(
        Some(&recorded),
        app.world().entity(e).get::<InputHistory<A>>(),
        "a resim step must not re-record the live input over the history it replays",
    );
}

/// A body whose queue has never been fed broadcasts nothing — an empty
/// `HistoryFor` would be pure traffic and the client-side receive would do
/// nothing with it.
#[test]
fn send_inputs_skips_unfed_queue() {
    let mut app = App::new();
    app.add_message::<ToClients<HistoryFor<A>>>()
        .add_systems(Update, send_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    app.world_mut().spawn(InputQueue::<A>::default());

    app.update();

    assert!(
        app.world()
            .resource::<Messages<ToClients<HistoryFor<A>>>>()
            .iter_current_update_messages()
            .next()
            .is_none(),
        "an unfed queue has nothing to say",
    );
}

#[test]
fn clears_inputs_empty_queue() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(5));

    let e1 = app
        .world_mut()
        .spawn((A(94), InputQueue::<A>::default()))
        .id();

    app.update();

    // There is no history, so the input is cleared
    let e = app.world().entity(e1);
    assert_eq!(A(0), *e.get::<A>().unwrap());
}

#[test]
fn receive_and_send_has_no_frame_delay() {
    let mut app = App::new();

    let e1 = app.world_mut().spawn(InputQueue::<A>::default()).id();

    app.init_resource::<ServerMessages>()
        .init_resource::<RollbackTarget>()
        .add_message::<FromClient<InputHistory<A>>>()
        .add_message::<ToClients<HistoryFor<A>>>()
        .add_plugins((
            StatesPlugin,
            InputQueueServerPlugin::<A, Tick>::new(Update.intern()),
        ))
        .insert_resource(Tick(5));

    app.init_state::<ServerState>();
    app.insert_resource(NextState::Pending(ServerState::Running));

    app.world_mut().write_message_batch([FromClient {
        client_id: ClientId::Client(e1),
        message: hist(4, [A(1), A(2), A(3)]),
    }]);

    app.update();

    let mut messages = app
        .world()
        .resource::<Messages<ToClients<HistoryFor<A>>>>()
        .iter_current_update_messages();
    assert_eq!(
        HistoryFor {
            entity: e1,
            tick: Tick(5).into(),
            past: default(),
            future: [(0u8, A(2)), (1, A(3))].into_iter().collect(),
        },
        messages.next().unwrap().message,
    );
    assert!(messages.next().is_none());
}

#[test]
fn repeat_late_inputs() {
    let mut app = App::new();

    app.add_systems(Update, load_inputs::<A, Tick>)
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(7));

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(0), &hist(4, [A(0), A(1), A(2)]));
    let e1 = app.world_mut().spawn((A(94), queue)).id();

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(0), &hist(0, [A(0), A(1)]));
    let e2 = app.world_mut().spawn((A(94), queue)).id();

    app.update();

    // All the data was old, but they could still be repeated
    let e = app.world().entity(e1);
    assert_eq!(A(2), *e.get::<A>().unwrap());

    // The latest known input is repeated indefinitely (the former 5-tick
    // cap was dropped because it produced default()-fallback prediction
    // bugs under any jitter).
    let e = app.world().entity(e2);
    assert_eq!(A(1), *e.get::<A>().unwrap());
}

/// Messages the routing cannot place — the listen server's own loopback id
/// (which never ships `FromClient` messages) and a client entity with no
/// `InputQueue` — are skipped, and later messages in the batch still apply.
#[test]
fn receive_inputs_skips_unroutable_messages() {
    let mut app = App::new();
    let no_queue = app.world_mut().spawn_empty().id();
    let queued = app.world_mut().spawn(InputQueue::<A>::default()).id();

    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(5));

    app.world_mut().write_message_batch([
        FromClient {
            client_id: ClientId::Server,
            message: hist(3, [A(1)]),
        },
        FromClient {
            client_id: ClientId::Client(no_queue),
            message: hist(3, [A(1)]),
        },
        FromClient {
            client_id: ClientId::Client(queued),
            message: hist(4, [A(1)]),
        },
    ]);

    app.update();

    assert_eq!(
        1,
        app.world()
            .entity(queued)
            .get::<InputQueue<A>>()
            .unwrap()
            .queue()
            .count(),
        "the routable message after the skipped ones must still be merged",
    );
    assert_eq!(
        Some(RepliconTick::new(4)),
        **app.world().resource::<RollbackTarget>(),
        "only the routable past input may request a rollback (3 was skipped)",
    );
}

/// `publish_confirmed_input_horizon` mirrors each queue's received-input
/// horizon onto a `ConfirmedInputHorizon` component: an unfed queue publishes
/// nothing, the first received input inserts the component, and an unchanged
/// horizon is not re-inserted (no replication churn).
#[test]
fn publishes_confirmed_input_horizon_once_per_change() {
    let mut app = App::new();
    let fed = app.world_mut().spawn(InputQueue::<A>::default()).id();
    let unfed = app.world_mut().spawn(InputQueue::<A>::default()).id();

    app.init_resource::<ServerMessages>()
        .init_resource::<RollbackTarget>()
        .add_message::<FromClient<InputHistory<A>>>()
        .add_message::<ToClients<HistoryFor<A>>>()
        .add_plugins((
            StatesPlugin,
            InputQueueServerPlugin::<A, Tick>::new(Update.intern()),
        ))
        .insert_resource(Tick(5));
    app.init_state::<ServerState>();
    app.insert_resource(NextState::Pending(ServerState::Running));

    app.world_mut().write_message(FromClient {
        client_id: ClientId::Client(fed),
        message: hist(5, [A(1)]),
    });
    app.update();

    assert_eq!(
        Some(5),
        app.world()
            .entity(fed)
            .get::<ConfirmedInputHorizon>()
            .map(|c| c.0),
        "the received horizon must be published onto the body",
    );
    assert!(
        app.world()
            .entity(unfed)
            .get::<ConfirmedInputHorizon>()
            .is_none(),
        "a never-fed queue has no horizon to publish",
    );

    let stamp = app
        .world()
        .entity(fed)
        .get_ref::<ConfirmedInputHorizon>()
        .unwrap()
        .last_changed();
    app.update();
    assert_eq!(
        stamp,
        app.world()
            .entity(fed)
            .get_ref::<ConfirmedInputHorizon>()
            .unwrap()
            .last_changed(),
        "an unchanged horizon must not be re-inserted",
    );
}

/// `store_authority_inputs` on a history stamped ahead of the current tick
/// resets the stale record, re-records from the live input, and self-feeds the
/// queue at the current tick.
#[test]
fn store_authority_inputs_resets_history_recorded_in_the_future() {
    let mut app = App::new();
    app.add_systems(Update, store_authority_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e = app
        .world_mut()
        .spawn((
            A(7),
            hist(10, [A(1), A(2)]),
            InputQueue::<A>::default(),
            InputAuthority,
        ))
        .id();

    app.update();

    let entity = app.world().entity(e);
    assert_eq!(
        Some(&hist(5, [A(7)])),
        entity.get::<InputHistory<A>>(),
        "a history from the future must be reset and re-seeded at the current tick",
    );
    assert_eq!(
        vec![&(Tick(5).into(), A(7))],
        entity
            .get::<InputQueue<A>>()
            .unwrap()
            .queue()
            .collect::<Vec<_>>(),
        "the re-recorded input must still self-feed the queue",
    );
}

/// The self-feed must be idempotent within a tick: a history already updated
/// at the current tick is left alone and nothing is re-fed into the queue.
#[test]
fn store_authority_inputs_skips_already_recorded_tick() {
    let mut app = App::new();
    app.add_systems(Update, store_authority_inputs::<A, Tick>)
        .insert_resource(Tick(5));
    let e = app
        .world_mut()
        .spawn((
            A(7),
            hist(5, [A(9)]),
            InputQueue::<A>::default(),
            InputAuthority,
        ))
        .id();

    app.update();

    let entity = app.world().entity(e);
    assert_eq!(
        Some(&hist(5, [A(9)])),
        entity.get::<InputHistory<A>>(),
        "a tick that is already recorded must not be overwritten by the live input",
    );
    assert_eq!(
        0,
        entity.get::<InputQueue<A>>().unwrap().queue().count(),
        "an already-recorded tick must not self-feed the queue again",
    );
}

/// A queue whose consumed ring claims ticks from the future (the tick source
/// moved backwards underneath it — an invariant violation) is warned about and
/// skipped: broadcasting it would compute negative past offsets.
#[test]
fn send_inputs_skips_queue_with_future_consumed_ticks() {
    let mut app = App::new();
    app.add_message::<ToClients<HistoryFor<A>>>()
        .add_systems(Update, send_inputs::<A, Tick>)
        .insert_resource(Tick(5));

    // Two corrupt bodies: the warn fires for the first and takes its
    // already-warned arm for the second.
    for _ in 0..2 {
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(10), &hist(10, [A(1)]));
        assert_eq!(Some(A(1)), queue.next(Tick(10)));
        app.world_mut().spawn(queue);
    }

    app.update();

    assert!(
        app.world()
            .resource::<Messages<ToClients<HistoryFor<A>>>>()
            .iter_current_update_messages()
            .next()
            .is_none(),
        "a queue with future consumed ticks must not be broadcast",
    );
}

/// An authority body resimulating a tick its history has no record for keeps
/// its live input — the replay must never zero a body over a missing record.
#[test]
fn authority_resim_missing_history_keeps_live_input() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Resimulating)
        .insert_resource(Tick(9));
    let e = app
        .world_mut()
        .spawn((
            A(42),
            InputQueue::<A>::default(),
            hist(5, [A(1)]),
            InputAuthority,
        ))
        .id();

    app.update();

    assert_eq!(
        A(42),
        *app.world().entity(e).get::<A>().unwrap(),
        "a history miss during resim must leave the live input untouched",
    );
}
