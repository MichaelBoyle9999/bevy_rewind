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

    assert_eq!(queue.queue.len(), 0);

    let earliest = queue.add(Tick(10), &hist(7, [A(79), A(80)]));
    assert_eq!(Some(RepliconTick::new(7)), earliest);
    assert_eq!(queue.queue.len(), 2);
    assert_eq!(
        ArrayDeque::from([(RepliconTick::new(7), A(79)), (RepliconTick::new(8), A(80)),]),
        queue.queue
    );

    let earliest = queue.add(Tick(10), &hist(9, [A(0), A(1), A(2)]));
    assert_eq!(Some(RepliconTick::new(9)), earliest);
    assert_eq!(queue.queue.len(), 5);

    let earliest = queue.add(Tick(10), &hist(10, [A(29), A(42), A(3)]));
    assert_eq!(None, earliest);
    assert_eq!(queue.queue.len(), 6);

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

    assert_eq!(queue.next(Tick(10)), Some(A(0)));
    assert_eq!(queue.next(Tick(11)), Some(A(0)));
    assert_eq!(queue.next(Tick(15)), Some(A(0)));
    assert_eq!(queue.next(Tick(16)), Some(A(0)));
    assert_eq!(queue.next(Tick(17)), Some(A(7)));
}

#[test]
fn queue_repeat_is_optional() {
    let mut queue = InputQueue::<NoRepeat>::default();
    queue.add(Tick(10), &hist(10, [NoRepeat(0)]));
    queue.add(Tick(10), &hist(17, [NoRepeat(7)]));

    assert_eq!(queue.next(Tick(10)), Some(NoRepeat(0)));
    assert_eq!(queue.next(Tick(11)), None);
    assert_eq!(queue.next(Tick(15)), None);
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
    let mut queue = InputQueue::<A>::default();
    assert_eq!(
        Some(RepliconTick::new(5)),
        queue.add(Tick(10), &hist(5, [A(7), A(7), A(7), A(7), A(7)])),
    );
    assert_eq!(
        None,
        queue.add(Tick(10), &hist(5, [A(7), A(7), A(7), A(7), A(7)])),
    );
}

#[test]
fn queue_reports_only_the_earliest_differing_past_tick_as_novel() {
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(10), &hist(5, [A(1), A(2), A(3)]));
    assert_eq!(
        Some(RepliconTick::new(7)),
        queue.add(Tick(10), &hist(5, [A(1), A(2), A(99)])),
    );
}

#[test]
fn queue_does_not_report_already_consumed_evicted_ticks_as_novel() {
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(1), A(2), A(3), A(4), A(5)]));
    for t in 5..=9 {
        queue.next(Tick(t));
    }
    assert_eq!(
        None,
        queue.add(Tick(15), &hist(5, [A(1), A(2), A(3), A(4), A(5)])),
    );
}

#[test]
fn queue_reports_unseen_past_tick_after_some_consumption_as_novel() {
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(1), A(2)]));
    queue.next(Tick(5));
    queue.next(Tick(6));
    assert_eq!(
        Some(RepliconTick::new(7)),
        queue.add(Tick(10), &hist(7, [A(42)])),
    );
}

#[test]
fn queue_does_not_report_predicted_repeat_as_novel() {
    let mut queue = InputQueue::<A>::default();
    queue.add(Tick(5), &hist(5, [A(1), A(1), A(1)]));
    for t in 5..=7 {
        queue.next(Tick(t));
    }
    assert_eq!(
        None,
        queue.add(Tick(10), &hist(8, [A(1)])),
        "a new past tick equal to the repeated prediction is not a misprediction",
    );
}

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

#[test]
fn queue_drops_late_nonrepeating_input() {
    let mut queue = InputQueue::<NoRepeat>::default();
    queue.add(Tick(10), &hist(10, [NoRepeat(3)]));

    assert_eq!(None, queue.next(Tick(12)));
    assert_eq!(0, queue.past().count(), "nothing was consumed for tick 12");
    assert_eq!(0, queue.queue().count(), "the late entry was still drained");
}

#[test]
fn queue_ignores_empty_history() {
    let mut queue = InputQueue::<A>::default();

    assert_eq!(None, queue.add(Tick(5), &hist(0, [])));
    assert_eq!(0, queue.queue().count());
    assert_eq!(None, queue.received_horizon());
}

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
