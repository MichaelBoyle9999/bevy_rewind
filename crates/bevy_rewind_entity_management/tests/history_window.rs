use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_entity_management::outside_history_window;
use proptest::prelude::*;

fn tick(v: u32) -> RepliconTick {
    RepliconTick::new(v)
}

#[test]
fn at_window_edge_is_still_inside() {
    assert!(
        !outside_history_window(tick(100), 10, tick(110)),
        "tick exactly at stamped + history is the last tick still inside the window"
    );
}

#[test]
fn one_past_window_edge_is_outside() {
    assert!(
        outside_history_window(tick(100), 10, tick(111)),
        "the first tick beyond stamped + history is outside the window"
    );
}

#[test]
fn before_window_edge_is_inside() {
    assert!(
        !outside_history_window(tick(100), 10, tick(109)),
        "a tick short of stamped + history is well inside the window"
    );
}

#[test]
fn far_past_window_edge_is_outside() {
    assert!(
        outside_history_window(tick(100), 10, tick(200)),
        "a tick far beyond stamped + history is outside the window"
    );
}

proptest! {
    #[test]
    fn matches_stamped_plus_history_strictly_less_than_tick(
        stamped in 0u32..=10_000,
        history in 0u32..=10_000,
        current in 0u32..=30_000,
    ) {
        let expected = stamped + history < current;
        prop_assert_eq!(
            outside_history_window(tick(stamped), history, tick(current)),
            expected,
            "stamped={} history={} tick={}", stamped, history, current
        );
    }
}
