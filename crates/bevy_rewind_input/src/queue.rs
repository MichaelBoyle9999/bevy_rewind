use crate::{InputHistory, InputTrait};

use arraydeque::{ArrayDeque, Wrapping};
use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

/// A queue containing inputs
#[derive(Component, Debug)]
pub struct InputQueue<T: InputTrait> {
    /// Ring of already-consumed inputs, broadcast as loss redundancy.
    pub past: ArrayDeque<(RepliconTick, T), 3, Wrapping>,
    /// Pending inputs keyed by tick, consumed by [`Self::next`].
    pub queue: ArrayDeque<(RepliconTick, T), 30>,
    /// Highest tick for which this body's *real* (received, not extrapolated)
    /// input has ever been merged in via [`Self::add`]. This is the body's
    /// confirmed-input horizon: past it the simulator only has the last input
    /// *repeated forward* (a guess), so authoritative state there must not be
    /// asserted. `None` until the first input arrives.
    received_horizon: Option<RepliconTick>,
}

impl<T: InputTrait> Default for InputQueue<T> {
    fn default() -> Self {
        Self {
            past: ArrayDeque::new(),
            queue: ArrayDeque::new(),
            received_horizon: None,
        }
    }
}

impl<T: InputTrait> InputQueue<T> {
    /// Iterate over the already-consumed inputs (the redundancy ring).
    pub fn past(&self) -> impl Iterator<Item = &(RepliconTick, T)> {
        self.past.iter()
    }

    /// Iterate over the pending (not yet consumed) inputs.
    pub fn queue(&self) -> impl Iterator<Item = &(RepliconTick, T)> {
        self.queue.iter()
    }

    /// The highest tick for which real input has been received — the body's
    /// confirmed-input horizon. Authoritative state beyond this tick is
    /// extrapolation (a repeated guess) and must not be confirmed.
    pub fn received_horizon(&self) -> Option<RepliconTick> {
        self.received_horizon
    }

    /// Merge an incoming input history into the queue. Past-tick history
    /// (entries stamped for ticks `< cur_tick`) is accepted, not discarded:
    /// late inputs are placed into the queue at their stamped tick so that
    /// the eager-rollback path can resimulate from the earliest changed tick.
    /// Returns the earliest past tick whose value is *novel* — i.e. it
    /// differs from what was already on file for that tick, or it lands in
    /// a gap past anything previously consumed. A history that re-sends the
    /// same values for the same past ticks returns `None`, so no rollback is
    /// requested.
    ///
    /// Why this matters: at zero network latency the client still ships its
    /// full `InputHistory` ring (20 ticks) every tick, and system-pipeline
    /// delay means every message arrives stamped at `< cur_tick`. Without the
    /// novelty check the server would request a fresh rollback every tick,
    /// every tick, even when no input has actually changed — the chronic
    /// 13-tick floor-clamp the zero-latency smoothness tests caught.
    ///
    /// Conflict policy: on tick overlap, the incoming history overrides any
    /// existing queue entry — the newest message has the most-recent client
    /// state for that tick. Capacity overrun keeps the newest ticks.
    pub fn add(
        &mut self,
        tick: impl Into<RepliconTick>,
        history: &InputHistory<T>,
    ) -> Option<RepliconTick> {
        if history.is_empty() {
            return None;
        }
        let cur_tick = tick.into();
        let history_first = history.first_tick();
        let history_last = history.updated_at();

        // Record the body's confirmed-input horizon: the highest tick we have ever
        // been told real input for. Everything the simulator runs past this is the
        // last input repeated forward (a guess), which must not be confirmed.
        self.received_horizon = Some(match self.received_horizon {
            Some(prev) if prev >= history_last => prev,
            _ => history_last,
        });

        // Highest tick we've ever called `next()` for. Anything <= this is
        // either already consumed or was skipped as late (and either way the
        // simulator has moved past it). If `past` is empty (no `next()` yet),
        // any past-tick info is potentially novel.
        let highest_consumed = self.past.back().map(|(t, _)| t.get());

        let mut earliest_novel: Option<RepliconTick> = None;
        for (i, new_val) in history.iter().enumerate() {
            let t = RepliconTick::new(history_first.get() + i as u32);
            if t >= cur_tick {
                break;
            }
            let existing: Option<&T> = self
                .past
                .iter()
                .find(|(pt, _)| *pt == t)
                .map(|(_, v)| v)
                .or_else(|| self.queue.iter().find(|(qt, _)| *qt == t).map(|(_, v)| v));
            let is_novel = match existing {
                Some(v) => v != new_val,
                None => match highest_consumed {
                    // Nothing consumed yet: no prediction baseline, so any past
                    // input is potentially new information.
                    None => true,
                    // An unseen tick the simulator has already run past is a real
                    // misprediction only if its value differs from what we
                    // PREDICTED there — the last consumed input repeated forward
                    // (input-repeat is how the body is extrapolated). A new tick
                    // whose value matches that repeat is steady-state delivery,
                    // not a correction, so it must not trigger a rollback. (A tick
                    // <= highest_consumed is either still in `past` — handled by
                    // the `Some` arm above — or older than anything we've run.)
                    Some(hc) => {
                        t.get() > hc && {
                            let predicted = self
                                .past
                                .back()
                                .and_then(|(bt, bv)| bv.repeated(t.get() - bt.get()))
                                .unwrap_or_default();
                            predicted != *new_val
                        }
                    }
                },
            };
            if is_novel {
                earliest_novel = Some(t);
                break;
            }
        }

        let existing: Vec<(RepliconTick, T)> = self
            .queue
            .drain(..)
            // Drop existing slots overlapping the history's range — history wins on conflict.
            .filter(|(t, _)| *t < history_first || *t > history_last)
            .collect();

        let mut combined: Vec<(RepliconTick, T)> =
            Vec::with_capacity(existing.len() + history.iter().count());
        combined.extend(existing);
        combined.extend(
            history
                .iter()
                .enumerate()
                .map(|(i, t)| (history_first + i as u32, t.clone())),
        );
        combined.sort_by_key(|(t, _)| t.get());

        let cap = self.queue.capacity();
        let skip = combined.len().saturating_sub(cap);
        for entry in combined.into_iter().skip(skip) {
            let _ = self.queue.push_back(entry);
        }

        earliest_novel
    }

    /// Consume the input for `tick`: the exact queued input when present,
    /// otherwise the newest late (or last consumed) input repeated forward.
    pub fn next(&mut self, tick: impl Into<RepliconTick>) -> Option<T> {
        let tick = tick.into();
        // Pop every entry at or below `tick`; the queue is tick-sorted, so the
        // last popped entry is either the exact input for `tick` or the newest
        // late input the simulator has now moved past.
        let mut newest = None;
        while self.queue.front().is_some_and(|(t, _)| *t <= tick) {
            newest = self.queue.pop_front();
        }
        // An exact hit is consumed as-is; a late input stands in by repeating
        // forward to `tick`.
        let hit = newest.and_then(|(from_tick, t)| {
            if from_tick == tick {
                Some(t)
            } else {
                t.repeated(tick - from_tick)
            }
        });
        if let Some(input) = hit {
            self.past.push_back((tick, input.clone()));
            return Some(input);
        }
        self.past
            .back()
            .and_then(|(from_tick, t)| t.repeated(tick - *from_tick))
    }
}
