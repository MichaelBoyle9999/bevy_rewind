#[path = "support/entity_input.rs"]
mod entity_input;
mod support;
#[path = "support/hist.rs"]
mod support_hist;

use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_input::InputHistory;
use entity_input::E;
use support::{A, Tick};
use support_hist::hist;

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

#[test]
fn reset_clears_to_default() {
    let mut history = hist(10, [A(1), A(2)]);
    history.reset();

    assert!(history.is_empty());
    assert_eq!(InputHistory::<A>::default(), history);
}
