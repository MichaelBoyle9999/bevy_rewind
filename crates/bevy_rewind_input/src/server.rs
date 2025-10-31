//! Logic specific to server apps

use std::marker::PhantomData;

use crate::{
    HistoryFor, InputAuthority, InputHistory, InputQueue, InputQueueSet, InputTrait, TickSource,
};

use bevy::{ecs::schedule::InternedScheduleLabel, prelude::*};
use bevy_replicon::prelude::*;

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
        app.add_systems(
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
        input_queue.add(*cur_tick, message);
    }
}

fn send_inputs<T: InputTrait, Tick: TickSource>(
    mut messages: MessageWriter<ToClients<HistoryFor<T>>>,
    query: Query<(Entity, &InputQueue<T>)>,
    cur_tick: Res<Tick>,
) {
    let cur_tick = (*cur_tick).into();
    for (entity, queue) in query.iter() {
        if queue.past().any(|(t, _)| *t > cur_tick) || queue.queue().any(|(t, _)| *t < cur_tick) {
            warn_once!(
                "({:?}) Queue has inputs with impossible ticks: {:?}",
                cur_tick.get(),
                queue
            );
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
                future: queue
                    .queue()
                    .take(7)
                    .filter(|(tick, _)| tick.get() >= cur_tick.get())
                    .map(|(tick, t)| ((tick.get() - cur_tick.get()) as u8, t.clone()))
                    .collect(),
            },
        });
    }
}

fn load_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&mut T, &mut InputQueue<T>, Has<InputAuthority>)>,
    tick: Res<Tick>,
) {
    for (mut input, mut input_queue, authority) in query.iter_mut() {
        let found = input_queue.next(*tick);
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
        assert_eq!(
            vec![&(Tick(5).into(), A(2)), &(Tick(6).into(), A(3))],
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

    #[test]
    fn clears_inputs_empty_queue() {
        let mut app = App::new();
        app.add_systems(Update, load_inputs::<A, Tick>)
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

        // All the data was old, and could no longer be repeated
        let e = app.world().entity(e2);
        assert_eq!(A(0), *e.get::<A>().unwrap());
    }
}
