#[path = "support/entity_input.rs"]
mod entity_input;
mod support;
#[path = "support/hist.rs"]
mod support_hist;
#[path = "support/ramp.rs"]
mod support_ramp;

use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_input::InputHistory;
use entity_input::E;
use support::{A, Tick};
use support_hist::hist;
use support_ramp::Ramp;

#[test]
fn get() {
    let history = hist(10, [A(1), A(2), A(3), A(4), A(5)]);

    for i in 0..5 {
        assert_eq!(Some(A(1 + i)), history.get(Tick(10 + i as u32), false));
    }

    assert_eq!(None, history.get(Tick(9), false));
    assert_eq!(None, history.get(Tick(0), false));
    assert_eq!(None, history.get(Tick(5), false));
    assert_eq!(None, history.get(Tick(15), false));
    assert_eq!(None, history.get(Tick(20), false));
    assert_eq!(None, history.get(Tick(598182), false));
}

#[test]
fn get_repeats() {
    let history = hist(0, [A(1)]);

    for i in 0..5 {
        assert_eq!(Some(A(1)), history.get(Tick(i as u32), true));
    }
    assert_eq!(Some(A(1)), history.get(Tick(6), true));
    assert_eq!(Some(A(1)), history.get(Tick(2309), true));
}

#[test]
fn write() {
    let mut history = InputHistory::<A>::default();

    history.write(Tick(15), A(1));
    assert_eq!(1, history.list.len());
    assert_eq!(RepliconTick::new(15), history.updated_at());

    history.write(Tick(16), A(2));
    assert_eq!(2, history.list.len());
    assert_eq!(RepliconTick::new(16), history.updated_at());

    history.write(Tick(14), A(0));
    assert_eq!(2, history.list.len());
    assert_eq!(RepliconTick::new(16), history.updated_at());

    history.write(Tick(20), A(6));
    assert_eq!(6, history.list.len());
    assert_eq!(RepliconTick::new(20), history.updated_at());

    assert_eq!(hist(15, [A(1), A(2), A(2), A(2), A(2), A(6)]), history);

    history.write(Tick(46), A(10));
    assert_eq!(1, history.list.len());
}

#[test]
fn write_with_gaps_wrap() {
    let mut history = hist(10, (0..10).map(A));
    assert_eq!(10, history.list.len());

    history.write(Tick(25), A(15));
    assert_eq!(10, history.list.len());
    assert_eq!(
        hist(
            16,
            (6..10).map(A).chain((0..5).map(|_| A(9))).chain([A(15)])
        ),
        history
    );
}

#[test]
fn write_gap_equal_to_capacity_salvages_instead_of_clearing() {
    let mut history = InputHistory::<A>::default();
    history.write(Tick(10), A(1));
    history.write(Tick(11), A(2));
    let cap = history.list.capacity() as u32;

    history.write(Tick(11 + cap), A(9));

    assert!(
        history.list.len() > 1,
        "a gap exactly equal to capacity must salvage the tail and gap-fill \
         (leaving a full window), not clear the list down to the lone new entry",
    );
    assert_eq!(RepliconTick::new(11 + cap), history.updated_at());
}

#[test]
fn write_gap_fill_extrapolates_each_missing_tick_forward() {
    let mut history = InputHistory::<Ramp>::default();
    history.write(Tick(5), Ramp(0));
    history.write(Tick(8), Ramp(100));

    assert_eq!(
        hist(5, [Ramp(0), Ramp(1), Ramp(2), Ramp(100)]),
        history,
        "the two skipped ticks (6, 7) must be filled by extrapolating the last \
         known input forward by 1 and 2 ticks — the offset is (gap_tick - last)",
    );
}

#[test]
fn first_tick() {
    let mut history = InputHistory::<A>::default();

    assert_eq!(RepliconTick::new(0), history.updated_at());
    assert_eq!(RepliconTick::new(0), history.first_tick());

    history.write(Tick(15), A(1));
    assert_eq!(RepliconTick::new(15), history.updated_at());
    assert_eq!(RepliconTick::new(15), history.first_tick());

    history.write(Tick(16), A(1));
    assert_eq!(2, history.list.len());
    assert_eq!(RepliconTick::new(15), history.first_tick());

    let history = hist(10, [A(0), A(1), A(2), A(3), A(4), A(5)]);
    assert_eq!(RepliconTick::new(15), history.updated_at());
    assert_eq!(6, history.list.len());
    assert_eq!(RepliconTick::new(10), history.first_tick());
}

// One concrete iterator type so llvm-cov accumulates branch coverage in a single
// monomorphization rather than many partially-covered per-closure instantiations.
#[cfg(feature = "client")]
fn entries(list: impl IntoIterator<Item = (u32, A)>) -> std::vec::IntoIter<(RepliconTick, A)> {
    list.into_iter()
        .map(|(tick, a)| (RepliconTick::new(tick), a))
        .collect::<Vec<_>>()
        .into_iter()
}

#[cfg(feature = "client")]
#[test]
fn replace_section() {
    let original = hist(10, [A(1), A(2), A(3), A(4)]);

    let mut history = original.clone();
    history.list.reserve_exact(6);
    history.replace_section(entries([(13, A(10)), (14, A(11))]));

    let expected = hist(10, [A(1), A(2), A(3), A(10), A(11)]);
    assert_eq!(expected, history);

    let mut history = original.clone();
    history.list.reserve_exact(6);
    history.replace_section(entries([(8, A(10)), (9, A(11)), (10, A(12))]));

    let expected = hist(8, [A(10), A(11), A(12), A(2), A(3), A(4)]);
    assert_eq!(expected, history);

    let mut history = original.clone();
    history.list.reserve_exact(6);
    history.replace_section(entries([(11, A(10)), (12, A(11))]));

    let expected = hist(10, [A(1), A(10), A(11), A(4)]);
    assert_eq!(expected, history);

    let mut history = original.clone();
    history.list.reserve_exact(6);
    history.replace_section(entries([(50, A(10)), (51, A(11))]));

    let expected = hist(50, [A(10), A(11)]);
    assert_eq!(expected, history);
}

#[test]
fn map_entities_remaps_stored_inputs() {
    let mut world = World::new();
    let from = world.spawn_empty().id();
    let to = world.spawn_empty().id();

    let mut history = hist(3, [E(from), E(from)]);
    history.map_entities(&mut (from, to));

    assert_eq!(hist(3, [E(to), E(to)]), history);
}

#[cfg(feature = "client")]
#[test]
fn replace_section_skips_entries_far_in_the_past() {
    let original = hist(30, [A(1), A(2)]);

    let mut history = original.clone();
    history.replace_section(entries([(20, A(9))]));

    assert_eq!(original, history, "an ancient entry must be dropped");
}

#[cfg(feature = "client")]
#[test]
fn replace_section_seeds_empty_history_in_range() {
    let mut history = hist(20, []);
    history.replace_section(entries([(20, A(5))]));

    assert_eq!(hist(20, [A(5)]), history);
}

#[cfg(feature = "client")]
#[test]
fn replace_section_applies_entry_exactly_ten_ticks_behind_head() {
    let mut history = hist(0, (0..15).map(A));
    history.replace_section(entries([(4, A(99))]));

    assert_eq!(
        Some(A(99)),
        history.get(Tick(4), false),
        "an entry exactly ten ticks behind updated_at (14) is not yet too old \
         and must be applied in place, not skipped",
    );
}

#[cfg(feature = "client")]
#[test]
fn replace_section_replaces_the_head_tick_in_place() {
    let mut history = hist(10, [A(1), A(2), A(3)]);
    history.replace_section(entries([(10, A(50))]));

    assert_eq!(
        hist(10, [A(50), A(2), A(3)]),
        history,
        "an entry at exactly first_tick must replace the head in place, not \
         prepend a new out-of-range element ahead of it",
    );
}

#[cfg(feature = "client")]
#[test]
fn replace_section_prepend_extrapolates_each_gap_tick_forward() {
    let mut history = hist(20, [Ramp(0), Ramp(0)]);
    history.replace_section([(RepliconTick::new(17), Ramp(0))].into_iter());

    assert_eq!(
        hist(17, [Ramp(0), Ramp(1), Ramp(2), Ramp(0), Ramp(0)]),
        history,
        "prepending three ticks before first_tick must fill the two gap ticks \
         (18, 19) by extrapolating the entry forward by 1 and 2 — the offset is \
         (first_tick - tick - 1)",
    );
}

#[test]
fn reset_clears_to_default() {
    let mut history = hist(10, [A(1), A(2)]);
    history.reset();

    assert!(history.is_empty());
    assert_eq!(InputHistory::<A>::default(), history);
}
