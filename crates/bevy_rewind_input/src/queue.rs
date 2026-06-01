use crate::{InputHistory, InputTrait};

use arraydeque::{ArrayDeque, Wrapping};
use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

/// A queue containing inputs
#[derive(Component, Debug)]
pub struct InputQueue<T: InputTrait> {
    past: ArrayDeque<(RepliconTick, T), 3, Wrapping>,
    queue: ArrayDeque<(RepliconTick, T), 30>,
}

impl<T: InputTrait> Default for InputQueue<T> {
    fn default() -> Self {
        Self {
            past: ArrayDeque::new(),
            queue: ArrayDeque::new(),
        }
    }
}

impl<T: InputTrait> InputQueue<T> {
    pub(crate) fn past(&self) -> impl Iterator<Item = &(RepliconTick, T)> {
        self.past.iter()
    }

    pub(crate) fn queue(&self) -> impl Iterator<Item = &(RepliconTick, T)> {
        self.queue.iter()
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
    pub(crate) fn add(
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
                None => highest_consumed.is_none_or(|hc| t.get() > hc),
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

    pub(crate) fn next(&mut self, tick: impl Into<RepliconTick>) -> Option<T> {
        let tick = tick.into();
        let mut newest_late = None;
        while !self.queue.is_empty() && self.queue[0].0 < tick {
            newest_late = self.queue.pop_front();
        }
        if self.queue.is_empty() || self.queue[0].0 != tick {
            if let Some((from_tick, t)) = newest_late {
                if let Some(input) = t.repeated(tick - from_tick) {
                    self.past.push_back((tick, input.clone()));
                    return Some(input);
                }
            }
            return self
                .past
                .back()
                .and_then(|(from_tick, t)| t.repeated(tick - *from_tick));
        }

        let (tick, t) = self.queue.pop_front()?;
        self.past.push_back((tick, t.clone()));
        Some(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::*;

    use bevy::ecs::entity::MapEntities;
    use serde::{Deserialize, Serialize};

    #[derive(Component, Clone, Default, Serialize, Deserialize, Debug, PartialEq, TypePath)]
    pub struct NoRepeat(u8);

    impl InputTrait for NoRepeat {
        fn repeats() -> bool {
            false
        }
    }

    impl MapEntities for NoRepeat {
        fn map_entities<M: EntityMapper>(&mut self, _: &mut M) {}
    }

    #[test]
    fn queue_accepts_past_inputs_and_reports_earliest() {
        let mut queue = InputQueue::<A>::default();

        // List starts empty
        assert_eq!(queue.queue.len(), 0);

        // A history entirely in the past is now accepted (was dropped under the
        // legacy "queue holds only future ticks" design). The caller uses the
        // returned earliest past tick to request a rollback.
        let earliest = queue.add(Tick(10), &hist(7, [A(79), A(80)]));
        assert_eq!(Some(RepliconTick::new(7)), earliest);
        assert_eq!(queue.queue.len(), 2);
        assert_eq!(
            ArrayDeque::from([
                (RepliconTick::new(7), A(79)),
                (RepliconTick::new(8), A(80)),
            ]),
            queue.queue
        );

        // History straddling cur_tick: entries 9 (past), 10, 11 (future) are
        // merged. Earliest past is the only past tick this message brought.
        let earliest = queue.add(Tick(10), &hist(9, [A(0), A(1), A(2)]));
        assert_eq!(Some(RepliconTick::new(9)), earliest);
        assert_eq!(queue.queue.len(), 5);

        // Conflict policy: history wins on overlapping ticks. Ticks 10 and 11
        // already present, but they get overwritten with the latest values.
        let earliest = queue.add(Tick(10), &hist(10, [A(29), A(42), A(3)]));
        assert_eq!(None, earliest);
        assert_eq!(queue.queue.len(), 6);

        // Disjoint future range merges cleanly with no past write.
        let earliest = queue.add(Tick(10), &hist(15, [A(6), A(7)]));
        assert_eq!(None, earliest);
        assert_eq!(queue.queue.len(), 8);

        assert_eq!(
            ArrayDeque::from([
                (RepliconTick::new(7), A(79)),
                (RepliconTick::new(8), A(80)),
                (RepliconTick::new(9), A(0)),
                (RepliconTick::new(10), A(29)),
                (RepliconTick::new(11), A(42)),
                (RepliconTick::new(12), A(3)),
                (RepliconTick::new(15), A(6)),
                (RepliconTick::new(16), A(7)),
            ]),
            queue.queue
        );
    }

    #[test]
    fn queue_doesnt_overflow() {
        let mut queue = InputQueue::<A>::default();

        queue.add(Tick(10), &hist(7, (0..100).map(A)));
        assert_eq!(queue.queue.len(), 30);
    }

    #[test]
    fn queue_repeats_actions_when_none_available() {
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(10), &hist(10, [A(0)]));
        queue.add(Tick(10), &hist(17, [A(7)]));

        // We get the actual input
        assert_eq!(queue.next(Tick(10)), Some(A(0)));
        // There is no input, but the last one should still repeat
        assert_eq!(queue.next(Tick(11)), Some(A(0)));
        // Still repeating
        assert_eq!(queue.next(Tick(15)), Some(A(0)));
        // Repeating indefinitely (formerly capped at 5 ticks; the cap drove
        // jitter-induced default()-fallback bugs in client prediction).
        assert_eq!(queue.next(Tick(16)), Some(A(0)));
        // And now we should get the next input
        assert_eq!(queue.next(Tick(17)), Some(A(7)));
    }

    #[test]
    fn queue_repeat_is_optional() {
        let mut queue = InputQueue::<NoRepeat>::default();
        queue.add(Tick(10), &hist(10, [NoRepeat(0)]));
        queue.add(Tick(10), &hist(17, [NoRepeat(7)]));

        // We get the actual input
        assert_eq!(queue.next(Tick(10)), Some(NoRepeat(0)));
        // There is no input, and we shouldn't repeat
        assert_eq!(queue.next(Tick(11)), None);
        // Still no repeating
        assert_eq!(queue.next(Tick(15)), None);
        // And now we should get the next input
        assert_eq!(queue.next(Tick(17)), Some(NoRepeat(7)));
    }

    #[test]
    fn queue_skips_old_values() {
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(9), &hist(9, [A(0), A(1), A(2)]));

        assert_eq!(queue.next(Tick(10)), Some(A(1)));
    }

    #[test]
    fn queue_does_not_report_same_value_resends_as_novel() {
        // First add: queue empty, A(7) at tick 5 is novel — earliest past tick.
        let mut queue = InputQueue::<A>::default();
        assert_eq!(
            Some(RepliconTick::new(5)),
            queue.add(Tick(10), &hist(5, [A(7), A(7), A(7), A(7), A(7)])),
        );
        // Second add: same value for same ticks. No rollback should be requested.
        assert_eq!(
            None,
            queue.add(Tick(10), &hist(5, [A(7), A(7), A(7), A(7), A(7)])),
        );
    }

    #[test]
    fn queue_reports_only_the_earliest_differing_past_tick_as_novel() {
        // Seed the queue with [(5, A(1)), (6, A(2)), (7, A(3))].
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(10), &hist(5, [A(1), A(2), A(3)]));
        // Resend with ticks 5 and 6 unchanged but tick 7's value flipped.
        // The earliest novel past tick is 7, not 5.
        assert_eq!(
            Some(RepliconTick::new(7)),
            queue.add(Tick(10), &hist(5, [A(1), A(2), A(99)])),
        );
    }

    #[test]
    fn queue_does_not_report_already_consumed_evicted_ticks_as_novel() {
        // Seed and consume forward through tick 9 so `past` advances to 9.
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(5), &hist(5, [A(1), A(2), A(3), A(4), A(5)]));
        // Consume through tick 9. Past ring (cap 3) ends up holding ticks 7..9.
        for t in 5..=9 {
            queue.next(Tick(t));
        }
        // History stamped from tick 5 still includes ticks 5 and 6 — those have
        // been evicted from `past` and are older than the highest consumed tick,
        // so the simulator has moved past them. Not novel.
        assert_eq!(
            None,
            queue.add(Tick(15), &hist(5, [A(1), A(2), A(3), A(4), A(5)])),
        );
    }

    #[test]
    fn queue_reports_unseen_past_tick_after_some_consumption_as_novel() {
        // Seed [(5, A(1)), (6, A(2))], consume to tick 6 → past holds (5, _) and (6, _).
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(5), &hist(5, [A(1), A(2)]));
        queue.next(Tick(5));
        queue.next(Tick(6));
        // Now an incoming message backfills tick 7 (between highest_consumed=6
        // and cur_tick=10) with a brand new value. That's novel — we never saw
        // tick 7 before.
        assert_eq!(
            Some(RepliconTick::new(7)),
            queue.add(Tick(10), &hist(7, [A(42)])),
        );
    }

    #[test]
    fn queue_tracks_past_inputs() {
        let mut queue = InputQueue::<A>::default();
        queue.add(Tick(9), &hist(9, [A(0), A(1), A(2)]));
        queue.add(Tick(9), &hist(13, [A(4)]));

        assert_eq!(queue.next(Tick(10)), Some(A(1)));
        assert_eq!(queue.past.len(), 1);
        assert_eq!(queue.next(Tick(11)), Some(A(2)));
        assert_eq!(queue.past.len(), 2);

        // Repeated inputs don't need to get written
        assert_eq!(queue.next(Tick(12)), Some(A(2)));
        assert_eq!(queue.past.len(), 2);

        assert_eq!(queue.next(Tick(13)), Some(A(4)));
        assert_eq!(queue.past.len(), 3);

        assert_eq!(
            ArrayDeque::from([
                (RepliconTick::new(10), A(1)),
                (RepliconTick::new(11), A(2)),
                (RepliconTick::new(13), A(4))
            ]),
            queue.past
        );
    }
}
