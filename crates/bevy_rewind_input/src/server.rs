use crate::{
    ConfirmedHorizon, HistoryFor, InputAuthority, InputHistory, InputQueue, InputQueueSet,
    InputTrait, TickSource,
};

use arrayvec::ArrayVec;
use bevy::{ecs::schedule::InternedScheduleLabel, prelude::*};
use bevy_replicon::{prelude::*, shared::replicon_tick::RepliconTick};
use bevy_rewind::{ConfirmedInputHorizon, Resimulating, RollbackTarget};

pub struct InputQueueServerPlugin<T: InputTrait, Tick: TickSource> {
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
        app.init_resource::<ConfirmedHorizon>()
            .add_systems(
                PreUpdate,
                receive_inputs::<T, Tick>
                    .run_if(in_state(ServerState::Running))
                    .after(ServerSystems::Receive)
                    .in_set(InputQueueSet::Network),
            )
            .add_systems(
                self.schedule,
                (
                    store_authority_inputs::<T, Tick>.before(InputQueueSet::Load),
                    load_inputs::<T, Tick>
                        .in_set(InputQueueSet::Load)
                        .after(InputQueueSet::Network),
                )
                    .run_if(in_state(ServerState::Running)),
            )
            .add_systems(
                PostUpdate,
                (publish_confirmed_input_horizon::<T>, send_inputs::<T, Tick>)
                    .run_if(in_state(ServerState::Running))
                    .before(ServerSystems::Send)
                    .in_set(InputQueueSet::Network),
            );
    }
}

fn publish_confirmed_input_horizon<T: InputTrait>(
    mut commands: Commands,
    query: Query<(Entity, &InputQueue<T>, Option<&ConfirmedInputHorizon>)>,
) {
    for (entity, queue, existing) in &query {
        let Some(horizon) = queue.received_horizon() else {
            continue;
        };
        let horizon = horizon.get();
        if existing.map(|c| c.0) != Some(horizon) {
            commands
                .entity(entity)
                .insert(ConfirmedInputHorizon(horizon));
        }
    }
}

#[derive(Component, Deref)]
pub struct InputTarget(Entity);

impl InputTarget {
    pub fn all(entity: Entity) -> Self {
        Self(entity)
    }
}

pub fn receive_inputs<T: InputTrait, Tick: TickSource>(
    input_target: Query<&InputTarget>,
    mut messages: MessageReader<FromClient<InputHistory<T>>>,
    mut query: Query<&mut InputQueue<T>>,
    cur_tick: Res<Tick>,
    confirmed: Res<crate::ConfirmedHorizon>,
    mut rollback_target: ResMut<RollbackTarget>,
) {
    for FromClient { client_id, message } in messages.read() {
        let Some(client_entity) = client_id.entity() else {
            continue;
        };
        let entity = input_target
            .get(client_entity)
            .map(|target| **target)
            .unwrap_or(client_entity);
        let Ok(mut input_queue) = query.get_mut(entity) else {
            continue;
        };
        if let Some(target) = input_queue.add(*cur_tick, message) {
            if !sealed(target, *confirmed) {
                **rollback_target = Some(match **rollback_target {
                    Some(prev) => prev.min(target),
                    None => target,
                });
            }
        }
    }
}

fn sealed(target: RepliconTick, confirmed: crate::ConfirmedHorizon) -> bool {
    confirmed.0 != u32::MAX && target.get() <= confirmed.0.saturating_sub(crate::SEAL_GRACE_TICKS)
}

pub fn store_authority_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(&mut InputHistory<T>, &mut InputQueue<T>, &T), With<InputAuthority>>,
    tick: Res<Tick>,
    resimulating: Option<Res<Resimulating>>,
) {
    if resimulating.is_some() {
        return;
    }
    for (mut hist, mut queue, input) in query.iter_mut() {
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
        queue.add(*tick, &hist);
    }
}

pub fn send_inputs<T: InputTrait, Tick: TickSource>(
    mut messages: MessageWriter<ToClients<HistoryFor<T>>>,
    query: Query<(Entity, &InputQueue<T>)>,
    cur_tick: Res<Tick>,
) {
    let cur_tick = (*cur_tick).into();
    for (entity, queue) in query.iter() {
        if queue.past().any(|(t, _)| *t > cur_tick) {
            let diagnostic = format!(
                "({:?}) Queue has past inputs with impossible (future) ticks: {queue:?}",
                cur_tick.get(),
            );
            warn_once!("{diagnostic}");
            continue;
        }
        let future: ArrayVec<(u8, T), 7> = queue
            .queue()
            .take(7)
            .filter(|(tick, _)| tick.get() >= cur_tick.get())
            .map(|(tick, t)| ((tick.get() - cur_tick.get()) as u8, t.clone()))
            .collect();
        let past: ArrayVec<(u8, T), 3> = queue
            .past()
            .map(|(tick, t)| ((cur_tick.get() - tick.get()) as u8, t.clone()))
            .collect();
        if past.is_empty() && future.is_empty() {
            continue;
        }
        messages.write(ToClients {
            mode: SendMode::Broadcast,
            message: HistoryFor {
                entity,
                tick: cur_tick,
                past,
                future,
            },
        });
    }
}

pub fn load_inputs<T: InputTrait, Tick: TickSource>(
    mut query: Query<(
        &mut T,
        &mut InputQueue<T>,
        Option<&InputHistory<T>>,
        Has<InputAuthority>,
    )>,
    tick: Res<Tick>,
    confirmed: Res<ConfirmedHorizon>,
    resimulating: Option<Res<Resimulating>>,
) {
    let in_resim = resimulating.is_some();
    let sim_tick: RepliconTick = (*tick).into();
    let remote_tick = RepliconTick::new(sim_tick.get().min(confirmed.0));
    for (mut input, mut input_queue, hist, authority) in query.iter_mut() {
        if authority {
            if in_resim {
                if let Some(historical) = hist.and_then(|h| h.get(sim_tick, false)) {
                    *input = historical;
                }
            } else {
                input_queue.next(sim_tick);
            }
            continue;
        }
        *input = input_queue.next(remote_tick).unwrap_or_default();
    }
}
