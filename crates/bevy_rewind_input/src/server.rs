//! Logic specific to server apps

use std::marker::PhantomData;

use crate::{
    ConfirmedHorizon, HistoryFor, InputAuthority, InputHistory, InputQueue, InputQueueSet,
    InputTrait, TickSource,
};

use arrayvec::ArrayVec;
use bevy::{ecs::schedule::InternedScheduleLabel, prelude::*};
use bevy_replicon::{prelude::*, shared::replicon_tick::RepliconTick};
use bevy_rewind::RollbackTarget;

pub(super) struct InputQueueServerPlugin<T: InputTrait, Tick: TickSource> {
    schedule: InternedScheduleLabel,
    phantom: std::marker::PhantomData<(T, Tick)>,
}

impl<T: InputTrait, Tick: TickSource> InputQueueServerPlugin<T, Tick> {
    #[cfg(feature = "server")]
    pub fn new(schedule: InternedScheduleLabel) -> Self {
        Self {
            schedule,
            phantom: std::marker::PhantomData::<(T, Tick)>,
        }
    }
}

impl<T: InputTrait, Tick: TickSource> Plugin for InputQueueServerPlugin<T, Tick> {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConfirmedHorizon>().add_systems(
            PreUpdate,
            receive_inputs::<T, Tick>
                .run_if(in_state(ServerState::Running))
                .after(ServerSystems::Receive)
                .in_set(InputQueueSet::Network),
        )
        .add_systems(
            self.schedule,
            load_inputs::<T, Tick>
                .run_if(in_state(ServerState::Running))
                .in_set(InputQueueSet::Load)
                // In case the configured schedule is PreUpdate
                .after(InputQueueSet::Network),
        )
        .add_systems(
            PostUpdate,
            send_inputs::<T, Tick>
                .run_if(in_state(ServerState::Running))
                .before(ServerSystems::Send)
                .in_set(InputQueueSet::Network),
        );
    }
}

/// The entity to redirect the input to, use () as T to route all inputs, or an InputType
/// to route only that type. If both are specified, the InputType one takes precedence
#[derive(Component, Deref)]
pub struct InputTarget<T = ()>(#[deref] Entity, PhantomData<T>);

impl InputTarget<()> {
    /// Reroute all input for this client to the specified entity.
    /// If a specific-variant is also on this same entity, it will take precedence.
    pub fn all(entity: Entity) -> Self {
        Self(entity, PhantomData)
    }
}

impl<T> InputTarget<T> {
    /// Reroute input for this specific type to the specified entity.
    /// Takes precedence over [`InputTarget::all`] if both are present.
    pub fn specific(entity: Entity) -> Self {
        Self(entity, PhantomData)
    }
}

fn receive_inputs<T: InputTrait, Tick: TickSource>(
    input_target: Query<AnyOf<(&InputTarget<T>, &InputTarget)>>,
    mut messages: MessageReader<FromClient<InputHistory<T>>>,
    mut query: Query<&mut InputQueue<T>>,
    cur_tick: Res<Tick>,
    mut rollback_target: ResMut<RollbackTarget>,
) {
    for FromClient { client_id, message } in messages.read() {
        let Some(client_entity) = client_id.entity() else {
            continue;
        };
        let entity = input_target
            .get(client_entity)
            .map(|(specific, all)| specific.map(|e| **e).unwrap_or(**all.unwrap()))
            .unwrap_or(client_entity);
        let Ok(mut input_queue) = query.get_mut(entity) else {
            continue;
        };
        if let Some(target) = input_queue.add(*cur_tick, message) {
            // Eager rollback: a client input arrived stamped for a tick in our
            // past. Request a rollback to that tick so the resim picks up the
            // newly-merged late input. Per-system order: this runs in PreUpdate;
            // `calculate_rollback_target` runs later in `RunFixedMainLoop` and
            // both clamps to the rollback window and takes a min over any
            // replicon-confirm-driven target, so writing the raw past tick here
            // composes correctly with state-confirm rollbacks.
            **rollback_target = Some(match **rollback_target {
                Some(prev) => prev.min(target),
                None => target,
            });
        }
    }
}

fn send_inputs<T: InputTrait, Tick: TickSource>(
    mut messages: MessageWriter<ToClients<HistoryFor<T>>>,
    query: Query<(Entity, &InputQueue<T>, Option<&T>, Has<InputAuthority>)>,
    cur_tick: Res<Tick>,
) {
    let cur_tick = (*cur_tick).into();
    for (entity, queue, current, authority) in query.iter() {
        if queue.past().any(|(t, _)| *t > cur_tick) {
            warn_once!(
                "({:?}) Queue has past inputs with impossible (future) ticks: {:?}",
                cur_tick.get(),
                queue
            );
        }
        let mut future: ArrayVec<(u8, T), 7> = queue
            .queue()
            .take(7)
            .filter(|(tick, _)| tick.get() >= cur_tick.get())
            .map(|(tick, t)| ((tick.get() - cur_tick.get()) as u8, t.clone()))
            .collect();
        // A listen-server host drives its own (`InputAuthority`) body from live input
        // that never enters the queue: the host never sends itself a `FromClient`
        // message, so `receive_inputs` never feeds this body and the queue stays empty.
        // Broadcast its current input directly so clients can replay the host's movement
        // instead of seeing a frozen body. Guarded on an empty future so a body whose
        // input *does* arrive via the queue (a remote client's) is untouched.
        if authority && future.is_empty() {
            if let Some(input) = current {
                future.push((0, input.clone()));
            }
        }
        messages.write(ToClients {
            mode: SendMode::Broadcast,
            message: HistoryFor {
                entity,
                tick: cur_tick,
                past: queue
                    .past()
                    .map(|(tick, t)| ((cur_tick.get() - tick.get()) as u8, t.clone()))
                    .collect(),
                future,
            },
        });
    }
}

fn load_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&mut T, &mut InputQueue<T>, Has<InputAuthority>)>,
    tick: Res<Tick>,
    confirmed: Res<ConfirmedHorizon>,
) {
    let sim_tick: RepliconTick = (*tick).into();
    // A remote (non-authority) body loads input at `min(sim_tick, confirmed)`: real
    // queued input at/before the confirmed tick (so resim reconstructs the confirmed
    // ticks correctly), and the confirmed input repeated beyond it (the lead window,
    // `confirmed < sim_tick ≤ present`) — so the body EXTRAPOLATES from the confirmed
    // horizon to the present, symmetric with how a client extrapolates the host body,
    // rather than consuming the client's ahead-of-confirmed input it holds future-queued.
    // The host's own (authority) body runs from live input (empty queue), so it loads at
    // the present unchanged.
    let remote_tick = RepliconTick::new(sim_tick.get().min(confirmed.0));
    for (mut input, mut input_queue, authority) in query.iter_mut() {
        let at = if authority { sim_tick } else { remote_tick };
        let found = input_queue.next(at);
        if found.is_none() && authority {
            continue;
        }
        *input = found.unwrap_or_default();
    }
}

#[cfg(test)]
mod tests {
    use bevy::{ecs::schedule::ScheduleLabel, state::app::StatesPlugin};

    use super::*;
    use crate::tests::*;

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
            .insert_resource(Tick(5));

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Client(e),
            message: hist(5, [A(1), A(2), A(3)]),
        });
        app.update();

        assert_eq!(None, **app.world().resource::<RollbackTarget>());
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
    /// body ignores the horizon and loads at the present. This is what makes the
    /// host's render of a remote body extrapolate symmetrically with the client's.
    #[test]
    fn remote_body_loads_at_confirmed_horizon_authority_loads_at_present() {
        let mut app = App::new();
        app.add_systems(Update, load_inputs::<A, Tick>)
            .insert_resource(ConfirmedHorizon(5))
            .insert_resource(Tick(7));

        // Remote body: queue holds ticks 5 (A(10), confirmed), 6, 7 (A(30), present).
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(7), &hist(5, [A(10), A(20), A(30)]));
        let remote = app.world_mut().spawn((A(0), queue)).id();

        // Authority body: same queue, but it loads at the present regardless.
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(7), &hist(5, [A(10), A(20), A(30)]));
        let own = app.world_mut().spawn((A(0), queue, InputAuthority)).id();

        app.update();

        // Remote clamps to `min(present 7, confirmed 5) = 5` → tick 5's input.
        assert_eq!(A(10), *app.world().entity(remote).get::<A>().unwrap());
        // Authority loads the present tick 7's input.
        assert_eq!(A(30), *app.world().entity(own).get::<A>().unwrap());
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
}
