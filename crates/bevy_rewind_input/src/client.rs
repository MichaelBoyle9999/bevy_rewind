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

/// Record an `InputAuthority` body's live input into its `InputHistory`, once
/// per real simulation step.
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

        // Clone, never `mem::take`: the live component is a *snapshot* the
        // application's capture rewrites each tick, and some fields carry
        // state across ticks (a wrapping press counter; a steer angle the
        // capture slews from its previous value). Zeroing the live input here
        // planted a phantom `T::default()` the tick after any field the
        // capture only writes conditionally — e.g. a press counter reverting
        // to 0 one tick after a press, which edge-detecting consumers saw as
        // a second, spurious press. The server-side twin
        // (`store_authority_inputs`) has always cloned.
        hist.write(*tick, input.clone());
    }
}

/// Load each body's input for the current tick from its `InputHistory`.
pub fn load_inputs<T: InputTrait, Tick: TickSource>(
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

/// Ship the local `InputAuthority` bodies' recorded histories to the server.
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

/// Merge the server's broadcast input histories into remote bodies, requesting
/// a rollback on genuine misprediction; see the in-body comments.
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
