//! Logic specific to server apps

use crate::{
    ConfirmedHorizon, HistoryFor, InputAuthority, InputHistory, InputQueue, InputQueueSet,
    InputTrait, TickSource,
};

use arrayvec::ArrayVec;
use bevy::{ecs::schedule::InternedScheduleLabel, prelude::*};
use bevy_replicon::{prelude::*, shared::replicon_tick::RepliconTick};
use bevy_rewind::{ConfirmedInputHorizon, Resimulating, RollbackTarget};

/// The server half of [`crate::InputQueuePlugin`]: receives, self-feeds,
/// loads, and broadcasts inputs.
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
                        // In case the configured schedule is PreUpdate
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

/// Publish each body's received-input horizon (from its [`InputQueue`]) onto a
/// [`ConfirmedInputHorizon`] component. Once replicated, every observer (including
/// the body's own client) refuses to reconcile the body to authoritative state
/// past it — trusting its own prediction instead of the host's extrapolated guess.
/// Runs server-side before `ServerSystems::Send`, after the tick's inputs have
/// been received (`PreUpdate`) and self-fed (sim schedule). The host's own body
/// self-feeds at the present each tick, so its horizon never lags `ServerTick`
/// and it is never restricted; a client body's horizon lags by the uplink delay,
/// capping reconciliation to its real input.
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

/// The entity a client's inputs are rerouted to. Placed on the client's
/// connection entity; without it, inputs apply to the client entity itself.
/// (A per-input-type variant existed but was never adopted — the
/// vehicle-occupancy design rejected runtime input rerouting — so routing is
/// all-or-nothing.)
#[derive(Component, Deref)]
pub struct InputTarget(Entity);

impl InputTarget {
    /// Reroute all input for this client to the specified entity.
    pub fn all(entity: Entity) -> Self {
        Self(entity)
    }
}

/// Merge each client's shipped `InputHistory` into the targeted body's
/// [`InputQueue`], requesting an eager rollback for novel, unsealed past input.
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
            // Eager rollback: a client input arrived stamped for a tick in our
            // past. Request a rollback to that tick so the resim picks up the
            // newly-merged late input — UNLESS the tick is already sealed. The
            // host runs its present ahead of the confirmed/replicated `ServerTick`
            // (`ConfirmedHorizon`); a tick at or below `ServerTick -
            // SEAL_GRACE_TICKS` has already been simulated and replicated as
            // authoritative, so rolling back to it would rewrite replicated history
            // and re-ship a corrected value to every client. Such a too-late input
            // is discarded (the standard server dejitter policy): it stays merged in
            // the queue at its stamped tick for `received_horizon` bookkeeping, but
            // being below the resim floor it is never consumed. The default sentinel
            // (`u32::MAX`) means "no seal published yet" — e.g. before the host's
            // first fixed step — and leaves the eager path unguarded, preserving the
            // zero-latency depth-1 rollback (its input lands within the grace).
            //
            // Per-system order: this runs in PreUpdate; `calculate_rollback_target`
            // runs later in `RunFixedMainLoop` and both clamps to the rollback
            // window and takes a min over any replicon-confirm-driven target, so
            // writing the raw past tick here composes correctly with state-confirm
            // rollbacks.
            if !sealed(target, *confirmed) {
                **rollback_target = Some(match **rollback_target {
                    Some(prev) => prev.min(target),
                    None => target,
                });
            }
        }
    }
}

/// Whether `target` falls in already-sealed (simulated-and-replicated) territory
/// the host must not roll back to revise: at or below `ConfirmedHorizon -
/// SEAL_GRACE_TICKS`. The default `ConfirmedHorizon` (`u32::MAX`) means no seal has
/// been published, so nothing is sealed.
fn sealed(target: RepliconTick, confirmed: crate::ConfirmedHorizon) -> bool {
    confirmed.0 != u32::MAX && target.get() <= confirmed.0.saturating_sub(crate::SEAL_GRACE_TICKS)
}

/// Record an `InputAuthority` body's live input into its own history and queue,
/// once per real simulation step, before [`load_inputs`] reads the queue. This is
/// the listen-server host's *loopback delivery*: it feeds the exact same
/// [`InputQueue::add`] entry point a remote client's `FromClient` message feeds in
/// [`receive_inputs`], so from here on the host body is indistinguishable from a
/// client body — [`load_inputs`] consumes its tick from the queue (an identity
/// write during forward simulation; the *historical* input during a rollback
/// resim, fixing the host body resimulating with its live present input), and
/// [`send_inputs`] broadcasts it with the same past-redundancy every client body
/// gets (replacing a former special case that sent the live input as a single
/// unprotected future entry).
///
/// Gated off during a rollback resim — the recorded history *is* what the resim
/// replays; rewriting it mid-resim with whatever `T` holds would corrupt it. The
/// self-feed never requests a rollback: the add stamps the current tick, and the
/// re-merged ring's past ticks are value-identical to what is already on file, so
/// `InputQueue::add`'s novelty check returns `None`.
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

/// Broadcast each body's queue contents (consumed-ring redundancy plus pending
/// future inputs) to every client.
pub fn send_inputs<T: InputTrait, Tick: TickSource>(
    mut messages: MessageWriter<ToClients<HistoryFor<T>>>,
    query: Query<(Entity, &InputQueue<T>)>,
    cur_tick: Res<Tick>,
) {
    let cur_tick = (*cur_tick).into();
    for (entity, queue) in query.iter() {
        if queue.past().any(|(t, _)| *t > cur_tick) {
            // Evaluate the diagnostic eagerly: log macros skip argument
            // evaluation when the event is disabled (e.g. no subscriber), and
            // this is a cold invariant-violation path.
            let diagnostic = format!(
                "({:?}) Queue has past inputs with impossible (future) ticks: {queue:?}",
                cur_tick.get(),
            );
            warn_once!("{diagnostic}");
            // Broadcasting would compute a negative past offset; skip the
            // corrupt queue rather than shipping garbage.
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
        // A body whose queue has never been fed (e.g. an authority body before
        // its first stored step) has nothing to say; an empty message would be
        // pure traffic.
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

/// Load each body's input for the current tick from its [`InputQueue`]; see
/// the in-body comments for the authority and confirmed-horizon policies.
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
    // A remote (non-authority) body loads input at `min(sim_tick, confirmed)`: real
    // queued input at/before the confirmed tick (so resim reconstructs the confirmed
    // ticks correctly), and the confirmed input repeated beyond it (the lead window,
    // `confirmed < sim_tick ≤ present`) — so the body EXTRAPOLATES from the confirmed
    // horizon to the present, symmetric with how a client extrapolates the host body,
    // rather than consuming the client's ahead-of-confirmed input it holds future-queued.
    let remote_tick = RepliconTick::new(sim_tick.get().min(confirmed.0));
    for (mut input, mut input_queue, hist, authority) in query.iter_mut() {
        if authority {
            // The host's own body: the exact mirror of the client-side `load_inputs`
            // authority arm. During forward simulation the live input (written by the
            // application's per-tick capture) is authoritative — never overwritten —
            // and the body's own tick is consumed from the self-fed queue purely so
            // `InputQueue::past` carries the redundancy `send_inputs` broadcasts.
            // During a rollback resim the recorded `InputHistory` is the canonical
            // replay source (the self-fed queue was drained by forward consumption),
            // ignoring the confirmed horizon: the host's own input needs no
            // extrapolation, it is confirmed by definition.
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
