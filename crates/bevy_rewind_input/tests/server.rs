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

    assert_eq!(
        4,
        app.world_mut()
            .query::<&InputQueue<A>>()
            .iter(app.world())
            .count()
    );

    let [e1, e2, e3, e4] = app.world().get_entity([e1, e2, e3, e4]).unwrap();
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
    assert_eq!(0, e4.get::<InputQueue<A>>().unwrap().queue().count());

    assert_eq!(
        Some(RepliconTick::new(4)),
        **app.world().resource::<RollbackTarget>()
    );
}

#[test]
fn receive_inputs_takes_min_with_existing_rollback_target() {
    let mut app = App::new();

    let e_late = app.world_mut().spawn(InputQueue::<A>::default()).id();

    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .init_resource::<ConfirmedHorizon>()
        .insert_resource(Tick(20));

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

#[test]
fn receive_inputs_seals_too_late_input() {
    let mut app = App::new();
    let e = app.world_mut().spawn(InputQueue::<A>::default()).id();
    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .insert_resource(ConfirmedHorizon(10))
        .insert_resource(Tick(15));

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

#[test]
fn receive_inputs_rolls_back_unsealed_input() {
    let mut app = App::new();
    let e = app.world_mut().spawn(InputQueue::<A>::default()).id();
    app.add_message::<FromClient<InputHistory<A>>>()
        .add_systems(Update, receive_inputs::<A, Tick>)
        .init_resource::<RollbackTarget>()
        .insert_resource(ConfirmedHorizon(10))
        .insert_resource(Tick(15));

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

    let e = app.world().entity(e1);
    assert_eq!(A(1), *e.get::<A>().unwrap());
    let e = app.world().entity(e2);
    assert_eq!(A(0), *e.get::<A>().unwrap());

    app.insert_resource(Tick(6));
    app.update();

    let e = app.world().entity(e1);
    assert_eq!(A(2), *e.get::<A>().unwrap());
    let e = app.world().entity(e2);
    assert_eq!(A(1), *e.get::<A>().unwrap());

    app.insert_resource(Tick(7));
    app.update();

    let e = app.world().entity(e1);
    assert_eq!(A(2), *e.get::<A>().unwrap());
    let e = app.world().entity(e2);
    assert_eq!(A(2), *e.get::<A>().unwrap());
}

#[test]
fn remote_body_loads_at_confirmed_horizon_authority_loads_at_present() {
    let mut app = App::new();
    app.add_systems(Update, load_inputs::<A, Tick>)
        .insert_resource(ConfirmedHorizon(5))
        .insert_resource(Resimulating)
        .insert_resource(Tick(7));

    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(7), &hist(5, [A(10), A(20), A(30)]));
    let remote = app.world_mut().spawn((A(0), queue)).id();

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

    assert_eq!(A(10), *app.world().entity(remote).get::<A>().unwrap());
    assert_eq!(A(30), *app.world().entity(own).get::<A>().unwrap());
}

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

    assert_eq!(A(42), *app.world().entity(own).get::<A>().unwrap());
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

    let e = app.world().entity(e1);
    assert_eq!(A(2), *e.get::<A>().unwrap());

    let e = app.world().entity(e2);
    assert_eq!(A(1), *e.get::<A>().unwrap());
}

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

#[test]
fn send_inputs_skips_queue_with_future_consumed_ticks() {
    let mut app = App::new();
    app.add_message::<ToClients<HistoryFor<A>>>()
        .add_systems(Update, send_inputs::<A, Tick>)
        .insert_resource(Tick(5));

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
