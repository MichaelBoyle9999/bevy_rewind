use crate::InputTrait;

use std::collections::VecDeque;

use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use serde::{Deserialize, Serialize};

/// Capacity of the per-body input history ring. Must cover at least the
/// configured rollback window (`bevy_rewind::RollbackFrames`) plus the
/// application's tick lead, otherwise a wide rollback will resim with
/// `T::default()` for ticks outside the ring — which would silently lose
/// movement on a remote body. Public so consumers can put a compile-time
/// canary against it; see `game/src/networking/components.rs`.
///
/// Sized so the *seal delay* (the lead, how far the host runs its present ahead
/// of the confirmed tick — `MAX_LEAD_TICKS = INPUT_HISTORY_CAPACITY −
/// DEFAULT_ROLLBACK_FRAMES`) covers a realistic worst-case ping. At 60 Hz a 10-tick
/// one-way lead absorbs ≈167 ms each way (≈333 ms RTT), so a client's input still
/// reaches the host's present in time to be applied in the unsealed lead window
/// rather than landing below the sealed `ServerTick` (which would force the host to
/// revise already-replicated authoritative state — the asymmetric move→stop
/// overshoot). Bumped from 20 (a 5-tick lead, which clamped above ≈183 ms RTT) so
/// "weird geography" pings of 160 ms+ stay in the healthy regime. The lead/ring
/// sizes are flagged for the on-device feel pass.
pub const INPUT_HISTORY_CAPACITY: usize = 25;

/// The input history for an input. Used when sending data to the server, also useful for rollback
#[derive(Message, Component, Clone, TypePath, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: for<'de2> serde::Deserialize<'de2>"))]
pub struct InputHistory<T: InputTrait> {
    /// The inputs, newest last; the last entry is the input at [`Self::updated_at`].
    // TODO: ArrayDeque?
    pub list: VecDeque<T>,
    /// The tick of the newest input in [`Self::list`].
    pub updated_at: RepliconTick,
}

impl<T: InputTrait> Default for InputHistory<T> {
    fn default() -> Self {
        Self {
            list: std::collections::VecDeque::with_capacity(INPUT_HISTORY_CAPACITY),
            updated_at: default(),
        }
    }
}

impl<T: InputTrait> MapEntities for InputHistory<T> {
    fn map_entities<M: EntityMapper>(&mut self, mapper: &mut M) {
        for t in self.list.iter_mut() {
            t.map_entities(mapper);
        }
    }
}

impl<T: InputTrait> InputHistory<T> {
    /// Returns true is the history is empty
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Iterate over all inputs in the history
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.list.iter()
    }

    /// Get the tick the queue was last updated at
    pub fn updated_at(&self) -> RepliconTick {
        self.updated_at
    }

    /// Get the tick for the first input in the history
    pub fn first_tick(&self) -> RepliconTick {
        RepliconTick::new(
            self.updated_at
                .get()
                .saturating_sub(self.list.len().saturating_sub(1) as u32),
        )
    }

    /// Get the input for the specified tick, if it exists
    pub fn get(&self, tick: impl Into<RepliconTick>, repeat: bool) -> Option<T> {
        let tick = tick.into();
        if (tick > self.updated_at() && !(repeat && T::repeats())) || tick < self.first_tick() {
            return None;
        }
        if tick > self.updated_at() {
            return self
                .list
                .back()
                .and_then(|t| t.repeated(tick - self.updated_at()));
        }
        let index = tick - self.first_tick();
        self.list.get(index as usize).cloned()
    }

    /// Write an input to the history. A write whose tick is more than one ahead
    /// of `updated_at` (a lost message, or a sender whose present skipped a tick
    /// on a lead slew) fills the gap by *repeating* the last known input — the
    /// same extrapolation [`Self::get`] applies beyond `updated_at` — so a gap
    /// tick replays as "kept doing what they were doing". Falling to
    /// `T::default()` instead planted a spurious zero input (a walking body
    /// halted and snapped its facing to default for one tick); non-repeating
    /// inputs still fill with the default, matching their `repeated() == None`
    /// contract.
    pub fn write(&mut self, tick: impl Into<RepliconTick>, value: T) {
        let tick = tick.into();
        if tick <= self.updated_at {
            warn!("Writing past values to history!");
            return;
        }

        if !self.list.is_empty() && tick > self.updated_at + 1 {
            if tick - self.updated_at > self.list.capacity() as u32 {
                self.list.clear();
            } else {
                while tick - self.first_tick() > self.list.capacity() as u32 {
                    self.list.pop_front();
                }
                let last = self.list.back().cloned();
                let updated_at = self.updated_at.get();
                self.list
                    .extend((updated_at..tick.get()).skip(1).map(|gap_tick| {
                        last.as_ref()
                            .and_then(|input| input.repeated(gap_tick - updated_at))
                            .unwrap_or_default()
                    }));
            }
        }

        if self.list.len() == self.list.capacity() {
            self.list.pop_front();
        }
        self.updated_at = tick;
        self.list.push_back(value);
    }

    /// Overwrite (or extend/front-fill) the history with the given `(tick, input)`
    /// entries; see the in-body comments for the gap-filling policy.
    #[cfg(feature = "client")]
    pub fn replace_section(&mut self, iter: impl Iterator<Item = (RepliconTick, T)>) {
        for (tick, t) in iter {
            // TODO: Better capacity system
            if tick + 10 < self.updated_at {
                continue;
            } else if tick > self.updated_at {
                self.write(tick, t.clone());
            } else if tick < self.first_tick() {
                // Front-fill the gap between this past value and the existing
                // window by repeating it forward — the same extrapolation `write`
                // applies to a trailing gap — rather than planting defaults.
                while tick + 1 < self.first_tick() {
                    let gap = self.first_tick() - tick - 1;
                    self.list.push_front(t.repeated(gap).unwrap_or_default());
                }
                self.list.push_front(t.clone());
            } else if self.list.is_empty() {
                // Degenerate default history (`updated_at == 0`, empty list): the
                // tick lands "in range" only because `first_tick()` collapses to
                // `updated_at`. Seed the slot instead of indexing an empty deque.
                self.updated_at = tick;
                self.list.push_back(t.clone());
            } else {
                let index = tick - self.first_tick();
                self.list[index as usize] = t.clone();
            }
        }
    }

    /// Reset the input history to an empty state
    pub fn reset(&mut self) {
        self.updated_at = default();
        self.list.clear();
    }
}
