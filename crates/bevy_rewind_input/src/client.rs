//! Logic specific to client apps

use crate::{HistoryFor, InputAuthority, InputHistory, InputQueueSet, InputTrait, TickSource};

use bevy::{ecs::schedule::InternedScheduleLabel, prelude::*};
use bevy_replicon::{client::ClientSystems, prelude::ClientState};
use bevy_rewind::Resimulating;

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
            receive_inputs::<T>
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
        // lags by `CLIENT_TICK_LEAD` and starts a (0,0) feedback loop the first
        // time history is fed back into a body whose live input is non-zero.
        // During resim, capture doesn't run, so history *is* the authoritative
        // source for past ticks and we load normally.
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

fn receive_inputs<T: InputTrait>(
    mut messages: MessageReader<HistoryFor<T>>,
    mut query: Query<&mut InputHistory<T>>,
) {
    for HistoryFor {
        entity,
        tick,
        past,
        future,
    } in messages.read()
    {
        let Ok(mut history) = query.get_mut(*entity) else {
            warn_once!(
                "Received history for entity without InputHistory: {}",
                entity
            );
            continue;
        };
        let mut past_iter = past.iter().peekable();
        while let (Some((rt, t)), until) = (
            past_iter.next(),
            past_iter.peek().map(|(rt, _)| *rt).unwrap_or_default(),
        ) {
            // Expand each item into the inputs it caused
            history.replace_section((until..=*rt).skip(1).rev().filter_map(|rrt| {
                t.repeated((*rt - rrt) as u32)
                    .map(|t| (*tick - rrt as u32, t))
            }));
        }
        history.replace_section(future.iter().map(|(rt, t)| (*tick + *rt as u32, t.clone())));
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
            .add_systems(Update, receive_inputs::<A>);
        let e1 = app.world_mut().spawn(InputHistory::<A>::default()).id();
        let e2 = app.world_mut().spawn(InputHistory::<A>::default()).id();

        app.world_mut().write_message(HistoryFor {
            entity: e1,
            tick: Tick(5).into(),
            past: [(4u8, A(1)), (1, A(2))].into_iter().collect(),
            future: [(0, A(3)), (2, A(4))].into_iter().collect(),
        });

        app.update();

        // The target entity needs to have history written
        let actual = app.world().entity(e1).get::<InputHistory<A>>();
        let expected = hist(1, [A(1), A(1), A(1), A(2), A(3), A(0), A(4)]);
        assert_eq!(Some(&expected), actual);

        // Other entities need to stay untouched
        let actual = app.world().entity(e2).get::<InputHistory<A>>();
        let expected = hist(0, []);
        assert_eq!(Some(&expected), actual);
    }
}
