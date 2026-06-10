//! Logic specific to client apps

use crate::{HistoryFor, InputAuthority, InputHistory, InputQueueSet, InputTrait, TickSource};

use bevy::{ecs::schedule::InternedScheduleLabel, prelude::*};
use bevy_replicon::{
    client::ClientSystems, prelude::ClientState, shared::replicon_tick::RepliconTick,
};
use bevy_rewind::{Resimulating, RollbackTarget};

pub(super) struct InputQueueClientPlugin<T: InputTrait, Tick: TickSource> {
    schedule: InternedScheduleLabel,
    phantom: std::marker::PhantomData<(T, Tick)>,
}

impl<T: InputTrait, Tick: TickSource> InputQueueClientPlugin<T, Tick> {
    #[cfg(feature = "client")]
    pub fn new(schedule: InternedScheduleLabel) -> Self {
        Self {
            schedule,
            phantom: std::marker::PhantomData::<(T, Tick)>,
        }
    }
}

impl<T: InputTrait, Tick: TickSource> Plugin for InputQueueClientPlugin<T, Tick> {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PreUpdate,
            receive_inputs::<T, Tick>
                .run_if(in_state(ClientState::Connected))
                .after(ClientSystems::Receive)
                .in_set(InputQueueSet::Network),
        )
        .add_systems(
            self.schedule,
            load_inputs::<T, Tick>
                .in_set(InputQueueSet::Load)
                .run_if(in_state(ClientState::Connected)),
        )
        .add_systems(
            FixedPostUpdate,
            store_inputs::<T, Tick>
                .in_set(InputQueueSet::Clean)
                .run_if(in_state(ClientState::Connected)),
        )
        .add_systems(
            PostUpdate,
            send_input_messages::<T>
                .run_if(in_state(ClientState::Connected))
                .before(ClientSystems::Send)
                .in_set(InputQueueSet::Network),
        );
    }
}

fn store_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&mut InputHistory<T>, &mut T), With<InputAuthority>>,
    tick: Res<Tick>,
) {
    for (mut hist, mut input) in query.iter_mut() {
        match hist.updated_at().partial_cmp(&(*tick).into()).unwrap() {
            std::cmp::Ordering::Greater => {
                hist.reset();
            }
            std::cmp::Ordering::Equal => {
                continue;
            }
            std::cmp::Ordering::Less => {}
        };

        let taken = std::mem::take(&mut *input);
        hist.write(*tick, taken);
    }
}

fn load_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&InputHistory<T>, &mut T, Has<InputAuthority>)>,
    tick: Res<Tick>,
    resimulating: Option<Res<Resimulating>>,
) {
    let in_resim = resimulating.is_some();
    for (hist, mut input, authority) in query.iter_mut() {
        // During forward simulation, the local-authority body's input is the
        // value the application's per-tick capture wrote at the start of this
        // fixed step. Loading from history here would overwrite it with the
        // server's round-tripped view of this client's earlier inputs, which
        // lags by one network round-trip and starts a (0,0) feedback loop the
        // first time history is fed back into a body whose live input is
        // non-zero. During resim, capture doesn't run, so history *is* the
        // authoritative source for past ticks and we load normally.
        if authority && !in_resim {
            continue;
        }
        let i = hist.get(*tick, !authority);
        if i.is_none() && authority {
            continue;
        }
        *input = i.unwrap_or_default();
    }
}

fn send_input_messages<T: InputTrait>(
    hist: Query<&InputHistory<T>, With<InputAuthority>>,
    mut messages: MessageWriter<InputHistory<T>>,
) {
    for hist in hist.iter() {
        if hist.is_empty() {
            continue;
        }
        messages.write(hist.clone());
    }
}

fn receive_inputs<T: InputTrait, Tick: TickSource>(
    mut messages: MessageReader<HistoryFor<T>>,
    mut query: Query<(&mut InputHistory<T>, Has<InputAuthority>)>,
    cur_tick: Res<Tick>,
    mut rollback_target: ResMut<RollbackTarget>,
) {
    let cur_tick: RepliconTick = (*cur_tick).into();
    for HistoryFor {
        entity,
        tick,
        past,
        future,
    } in messages.read()
    {
        let Ok((mut history, authority)) = query.get_mut(*entity) else {
            warn_once!(
                "Received history for entity without InputHistory: {}",
                entity
            );
            continue;
        };

        // The server broadcasts every body's history, including this client's own —
        // and for the input-owning client that echo is a *stale round trip*, not new
        // information: it reflects only the inputs the server had consumed when it
        // sent, lagging the live history by the full RTT. Writing it would
        // retroactively zero real recorded inputs (the server repeats its last
        // consumed input over ticks it hasn't received yet), trigger a bogus
        // misprediction rollback to the zeroed tick, resim the body from rest, and
        // ship the corrupted history back to the server — whose "newest message
        // wins" merge then erases the genuine inputs from its queue and freezes the
        // body authoritatively. Local recorded input is authoritative for an
        // `InputAuthority` body; never accept the echo.
        if authority {
            continue;
        }

        // Resim a remote body only on a genuine *misprediction* — the client-side
        // mirror of the server's `InputQueue::add` divergence check. We extrapolate
        // a remote body by repeating its last known input; a broadcast that fills a
        // past tick we already ran with exactly that repeated value is steady-state
        // delivery, not a correction, and must not roll back (otherwise a remote
        // body whose present runs ahead of the confirmed stream would roll back
        // every tick). So snapshot the *predicted* value (`get(.., true)` — the
        // repeat) at every past tick this message can touch, apply the message,
        // then find the earliest tick whose predicted value actually changed; that
        // is where a resim must replay from. The window spans
        // `[tick - max_past_offset, cur_tick)`: `past` reaches back from the stamp,
        // and a `future` entry is only in our past once our present has run beyond
        // it.
        let max_past_offset = past.iter().map(|(rt, _)| *rt as u32).max().unwrap_or(0);
        let before: Vec<(u32, Option<T>)> = (tick.get().saturating_sub(max_past_offset)
            ..cur_tick.get())
            .map(|t| (t, history.get(RepliconTick::new(t), true)))
            .collect();

        let mut past_iter = past.iter().peekable();
        while let (Some((rt, t)), until) = (
            past_iter.next(),
            past_iter.peek().map(|(rt, _)| *rt).unwrap_or_default(),
        ) {
            // Expand each item into the inputs it caused, including the item's
            // own tick (`rrt == rt`). The range deliberately overlaps the next
            // item's tick (`rrt == until`): that slot is first written as this
            // item's repeat and then overwritten with the next item's exact
            // value, which is what lets an offset-0 entry — the only coverage of
            // the stamp tick in a past-only message, e.g. a listen-server host
            // body's broadcast — land rather than being skipped.
            history.replace_section((until..=*rt).rev().filter_map(|rrt| {
                t.repeated((*rt - rrt) as u32)
                    .map(|t| (*tick - rrt as u32, t))
            }));
        }
        history.replace_section(future.iter().map(|(rt, t)| (*tick + *rt as u32, t.clone())));

        if let Some((t, _)) = before
            .into_iter()
            .find(|(t, b)| history.get(RepliconTick::new(*t), true) != *b)
        {
            let target = RepliconTick::new(t);
            **rollback_target = Some(match **rollback_target {
                Some(prev) => prev.min(target),
                None => target,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

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
}
