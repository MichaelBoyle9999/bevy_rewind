//! Tests for `InputQueue` (src/queue.rs).

#![cfg(feature = "server")]

mod support;
#[path = "support/hist.rs"]
mod support_hist;

use arraydeque::ArrayDeque;
use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_input::{InputQueue, InputTrait};
use serde::{Deserialize, Serialize};
use support::{A, Tick};
use support_hist::hist;

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
        ArrayDeque::from([(RepliconTick::new(7), A(79)), (RepliconTick::new(8), A(80)),]),
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

/// A new past tick whose value EQUALS what input-repeat already predicted for
/// it is NOT a misprediction — the body was extrapolated with exactly that
/// input, so resimulating would reproduce the same state. Reporting it novel
/// is what made the host roll back every tick of a steady walk (each tick
/// delivers a new-but-unchanged input). Divergence must be measured against
/// the repeated prediction, not "is this a new tick".
#[test]
fn queue_does_not_report_predicted_repeat_as_novel() {
    let mut queue = InputQueue::<A>::default();
    // Walk: ticks 5,6,7 = A(1); consume through 7 so `past.back()` = (7, A(1)).
    queue.add(Tick(5), &hist(5, [A(1), A(1), A(1)]));
    for t in 5..=7 {
        queue.next(Tick(t));
    }
    // A new past tick 8 arrives carrying the SAME A(1) the repeat already
    // predicted for tick 8. No misprediction ⇒ no rollback.
    assert_eq!(
        None,
        queue.add(Tick(10), &hist(8, [A(1)])),
        "a new past tick equal to the repeated prediction is not a misprediction",
    );
}

/// The contrast: a new past tick whose value DIFFERS from the repeated
/// prediction (e.g. the client's "stop" landing while the host extrapolated
/// the walk) is a real misprediction and must request a rollback to it.
#[test]
fn queue_reports_diverging_new_past_tick_as_novel() {
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(1), A(1), A(1)]));
    for t in 5..=7 {
        queue.next(Tick(t));
    }
    assert_eq!(
        Some(RepliconTick::new(8)),
        queue.add(Tick(10), &hist(8, [A(9)])),
        "a new past tick differing from the repeated prediction is a misprediction",
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

/// A late (already-passed) non-repeating input is consumed off the queue but
/// produces nothing: it cannot be repeated forward to the requested tick, and
/// nothing may be recorded into the consumed ring for it.
#[test]
fn queue_drops_late_nonrepeating_input() {
    let mut queue = InputQueue::<NoRepeat>::default();
    queue.add(Tick(10), &hist(10, [NoRepeat(3)]));

    assert_eq!(None, queue.next(Tick(12)));
    assert_eq!(0, queue.past().count(), "nothing was consumed for tick 12");
    assert_eq!(0, queue.queue().count(), "the late entry was still drained");
}

/// An empty history carries no information: merging it is a no-op that
/// reports nothing novel and leaves the received horizon untouched.
#[test]
fn queue_ignores_empty_history() {
    let mut queue = InputQueue::<A>::default();

    assert_eq!(None, queue.add(Tick(5), &hist(0, [])));
    assert_eq!(0, queue.queue().count());
    assert_eq!(None, queue.received_horizon());
}

/// A late repeating input is consumed off the queue and repeated forward to
/// the requested tick, entering the consumed ring at the tick it stood in for.
#[test]
fn queue_repeats_late_input_forward() {
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(3)]));

    assert_eq!(Some(A(3)), queue.next(Tick(7)));
    assert_eq!(
        vec![&(Tick(7).into(), A(3))],
        queue.past().collect::<Vec<_>>(),
    );
}
