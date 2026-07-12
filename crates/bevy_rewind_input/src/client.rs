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

pub fn store_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&mut InputHistory<T>, &T), With<InputAuthority>>,
    tick: Res<Tick>,
) {
    for (mut hist, input) in query.iter_mut() {
        match hist.updated_at().partial_cmp(&(*tick).into()).unwrap() {
            std::cmp::Ordering::Greater => {
                hist.reset();
            }
            std::cmp::Ordering::Equal => {
                continue;
            }
            std::cmp::Ordering::Less => {}
        };

        hist.write(*tick, input.clone());
    }
}

pub fn load_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&InputHistory<T>, &mut T, Has<InputAuthority>)>,
    tick: Res<Tick>,
    resimulating: Option<Res<Resimulating>>,
) {
    let in_resim = resimulating.is_some();
    for (hist, mut input, authority) in query.iter_mut() {
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

pub fn send_input_messages<T: InputTrait>(
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

pub fn receive_inputs<T: InputTrait, Tick: TickSource>(
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

        if authority {
            continue;
        }

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
