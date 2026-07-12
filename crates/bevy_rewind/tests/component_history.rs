#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/tick_data_deref.rs"]
mod tick_data_deref;

use comp_a::A;
use tick_data_deref::TickDataDeref;

use bevy::ptr::PtrMut;

use bevy_rewind::history::component::HistoryComponent;
use bevy_rewind::history::component_history::{ComponentHistory, TickData::*};

use std::num::NonZero;

// Funnelled through one helper so `ComponentHistory::write` is instantiated once
// (per-monomorphisation coverage gate).
fn wr(h: &mut ComponentHistory, tick: u32, v: u16) {
    unsafe { h.write(tick, |ptr| *ptr.deref_mut::<A>() = A(v)) };
}

#[test]
fn append() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 0, 1);
    assert_eq!(1, history.len());
    wr(&mut history, 1, 2);
    assert_eq!(2, history.len());
    wr(&mut history, 2, 3);
    assert_eq!(3, history.len());

    assert_eq!(Value(&A(1)), history.get(0).deref());
    assert_eq!(Value(&A(2)), history.get(1).deref());
    assert_eq!(Value(&A(3)), history.get(2).deref());
    assert_eq!(Missing, history.get(3).deref::<A>());
}

#[test]
fn get_latest() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 0, 1);
    wr(&mut history, 4, 2);
    assert_eq!(5, history.len());

    for i in 0..=3 {
        assert_eq!(Value(&A(1)), history.get_latest(i).deref());
    }

    history.mark_removed(1);
    for i in 1..=3 {
        assert_eq!(Removed, history.get_latest(i).deref::<A>());
    }
}

#[test]
fn start_non_zero_tick() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 25, 1);
    assert_eq!(1, history.len());
    assert_eq!(25, history.last_tick);

    assert_eq!(Missing, history.get(24).deref::<A>());
    assert_eq!(Value(&A(1)), history.get(25).deref::<A>());
    assert_eq!(Missing, history.get(26).deref::<A>());
}

#[test]
fn repeated_tick() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 0, 1);
    wr(&mut history, 1, 2);
    assert_eq!(2, history.len());

    wr(&mut history, 1, 4);
    assert_eq!(2, history.len());
    wr(&mut history, 0, 3);
    assert_eq!(2, history.len());

    assert_eq!(Value(&A(3)), history.get(0).deref());
    assert_eq!(Value(&A(4)), history.get(1).deref());
    assert_eq!(Missing, history.get(2).deref::<A>());
}

#[test]
fn gaps() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 0, 1);
    wr(&mut history, 2, 2);

    assert_eq!(3, history.len());
    assert_eq!(2, history.stored_items());

    assert_eq!(Value(&A(1)), history.get(0).deref());
    assert_eq!(Missing, history.get(1).deref::<A>());
    assert_eq!(Value(&A(2)), history.get(2).deref());
    assert_eq!(Missing, history.get(3).deref::<A>());
}

#[test]
fn wrap_retains_first_value() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 0, 1);
    wr(&mut history, 4, 2);
    wr(&mut history, 6, 3);

    assert_eq!(5, history.len());
    assert_eq!(3, history.stored_items());
    assert_eq!(Value(&A(1)), history.get(2).deref());
    assert_eq!(Value(&A(2)), history.get(4).deref());
    assert_eq!(Value(&A(3)), history.get(6).deref());
    for i in [1, 3, 5] {
        assert_eq!(Missing, history.get(i).deref::<A>());
    }
}

#[test]
fn wrap_with_removed() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    history.mark_removed(0);
    wr(&mut history, 5, 1);

    assert_eq!(5, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(Removed, history.get(1).deref::<A>());
    assert_eq!(Value(&A(1)), history.get(5).deref());
    for i in [0, 2, 3, 4, 6] {
        assert_eq!(Missing, history.get(i).deref::<A>());
    }
}

#[test]
fn wrap_more_than_capacity() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(20).unwrap());
    assert_eq!(0, history.len());

    history.mark_removed(0);
    wr(&mut history, 81, 1);

    assert_eq!(20, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(Removed, history.get(62).deref::<A>());
    assert_eq!(Value(&A(1)), history.get(81).deref());

    history.mark_removed(120);
    assert_eq!(Value(&A(1)), history.get(101).deref());
    assert_eq!(Removed, history.get(120).deref::<A>());
}

#[test]
fn out_of_order() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());
    assert_eq!(0, history.len());

    wr(&mut history, 2, 3);
    wr(&mut history, 1, 2);
    wr(&mut history, 3, 4);
    wr(&mut history, 0, 1);
    assert_eq!(4, history.len());

    assert_eq!(Value(&A(1)), history.get(0).deref());
    assert_eq!(Value(&A(2)), history.get(1).deref());
    assert_eq!(Value(&A(3)), history.get(2).deref());
    assert_eq!(Value(&A(4)), history.get(3).deref());
    assert_eq!(Missing, history.get(4).deref::<A>());
}

#[test]
fn clean() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());

    wr(&mut history, 0, 1);
    history.mark_removed(2);
    wr(&mut history, 3, 2);
    assert_eq!(4, history.len());
    assert_eq!(2, history.stored_items());

    history.clean(3);
    assert_eq!(4, history.len());
    assert_eq!(2, history.stored_items());

    history.clean(2);
    assert_eq!(3, history.len());
    assert_eq!(1, history.stored_items());

    assert_eq!(Value(&A(1)), history.get(0).deref());
    assert_eq!(Missing, history.get(1).deref::<A>());
    assert_eq!(Removed, history.get(2).deref::<A>());
    assert_eq!(Missing, history.get(3).deref::<A>());

    history.clean(0);
    assert_eq!(1, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(0, history.removed_mask);

    assert_eq!(Value(&A(1)), history.get(0).deref());
    for i in 1..=3 {
        assert_eq!(Missing, history.get(i).deref::<A>());
    }

    for i in 5..=9 {
        wr(&mut history, i, i as u16);
    }
    assert_eq!(5, history.len());
    assert_eq!(5, history.stored_items());

    history.clean(4);
    assert_eq!(0, history.len());
    assert_eq!(0, history.stored_items());
}

#[test]
fn keep_first_item() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(5).unwrap());

    unsafe { history.list.append(None::<fn(PtrMut)>) };
    assert_eq!(1, history.len());

    history.keep_first_item();
    assert_eq!(1, history.len());

    history.mark_removed(1);
    assert_eq!(2, history.len());

    history.keep_first_item();
    assert_eq!(2, history.len());

    wr(&mut history, 2, 1);
    history.mark_removed(3);
    wr(&mut history, 4, 2);
    assert_eq!(5, history.len());

    history.keep_first_item();
    assert_eq!(3, history.len());

    assert_eq!(Missing, history.get(0).deref::<A>());
    assert_eq!(Removed, history.get(1).deref::<A>());
    assert_eq!(Value(&A(1)), history.get(2).deref());
}

#[test]
fn empty_after() {
    let a = HistoryComponent::new::<A>();
    let mut history = ComponentHistory::from_component(&a, NonZero::new(64).unwrap());
    assert_eq!(0, history.len());
    assert_eq!(0, history.empty_after(0));

    unsafe { history.list.append(None::<fn(PtrMut)>) };
    assert_eq!(64, history.empty_after(0));
    assert_eq!(64, history.empty_after(1));

    wr(&mut history, 3, 1);
    assert_eq!(2, history.empty_after(0));
    assert_eq!(1, history.empty_after(1));
    assert_eq!(0, history.empty_after(2));
    for i in 3..=4 {
        assert_eq!(64, history.empty_after(i));
    }

    wr(&mut history, 20, 2);
    assert_eq!(1, history.empty_after(1));
    assert_eq!(16, history.empty_after(3));
    assert_eq!(1, history.empty_after(18));
    assert_eq!(0, history.empty_after(19));
    for i in 20..=21 {
        assert_eq!(64, history.empty_after(i));
    }

    history.mark_removed(25);
    assert_eq!(3, history.empty_after(21));
    assert_eq!(1, history.empty_after(23));
    assert_eq!(0, history.empty_after(24));
    for i in 25..=26 {
        assert_eq!(64, history.empty_after(i));
    }

    wr(&mut history, 64, 3);
    assert_eq!(37, history.empty_after(26));
    assert_eq!(1, history.empty_after(62));
    assert_eq!(0, history.empty_after(63));
    for i in 64..=65 {
        assert_eq!(64, history.empty_after(i));
    }

    assert_eq!(1, history.empty_after(0));
    assert_eq!(1, history.empty_after(1));
}

fn a_hist(size: u8) -> ComponentHistory {
    ComponentHistory::from_component(&HistoryComponent::new::<A>(), NonZero::new(size).unwrap())
}

#[test]
fn tick_data_eq_across_variants() {
    use bevy_rewind::history::component_history::TickData;

    assert_ne!(TickData::Value(A(1)), TickData::Removed);
    assert_ne!(TickData::Value(A(1)), TickData::Missing);
    assert_ne!(TickData::Removed, TickData::Value(A(1)));
    assert_ne!(TickData::Missing, TickData::Value(A(1)));
    assert_ne!(TickData::Missing, TickData::<A>::Removed);
    assert_ne!(TickData::Removed, TickData::<A>::Missing);
    assert_eq!(TickData::<A>::Missing, TickData::<A>::Missing);
    assert_eq!(TickData::<A>::Removed, TickData::<A>::Removed);
    assert_eq!(TickData::Value(A(1)), TickData::Value(A(1)));
    assert_ne!(TickData::Value(A(1)), TickData::Value(A(2)));
}

#[test]
fn debug_shows_history_state() {
    let mut history = a_hist(5);
    wr(&mut history, 3, 1);
    history.mark_removed(4);

    let repr = format!("{history:?}");
    assert!(repr.contains("last_tick: 4"), "{repr}");
    assert!(repr.contains("removed_mask"), "{repr}");
}

#[test]
fn get_latest_all_missing() {
    let mut history = a_hist(5);

    unsafe { history.list.append(None::<fn(PtrMut)>) };
    assert_eq!(1, history.len());

    assert_eq!(Missing, history.get_latest(0).deref::<A>());
}

#[test]
fn gap_over_capacity_with_only_missing() {
    let mut history = a_hist(5);

    unsafe { history.list.append(None::<fn(PtrMut)>) };
    assert_eq!(1, history.len());

    wr(&mut history, 10, 1);

    assert_eq!(5, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(Value(&A(1)), history.get(10).deref());
    for i in 6..=9 {
        assert_eq!(Missing, history.get(i).deref::<A>());
    }
}

#[test]
fn gap_over_capacity_moves_trailing_value_to_back() {
    let mut history = a_hist(5);

    wr(&mut history, 0, 1);
    unsafe { history.list.append(None::<fn(PtrMut)>) };
    history.last_tick = 1;
    assert_eq!(2, history.len());

    wr(&mut history, 100, 2);

    assert_eq!(5, history.len());
    assert_eq!(2, history.stored_items());
    assert_eq!(Value(&A(2)), history.get(100).deref());
    assert_eq!(Value(&A(1)), history.get_latest(99).deref::<A>());
}

#[test]
fn gap_over_capacity_moves_trailing_removed_to_back() {
    let mut history = a_hist(5);

    history.mark_removed(0);
    unsafe { history.list.append(None::<fn(PtrMut)>) };
    history.removed_mask = 0b10;
    history.last_tick = 1;
    assert_eq!(2, history.len());

    wr(&mut history, 100, 2);

    assert_eq!(5, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(Value(&A(2)), history.get(100).deref());
    assert_eq!(Removed, history.get_latest(99).deref::<A>());
}

#[test]
fn gap_over_capacity_full_mask_width() {
    let mut history = a_hist(64);

    wr(&mut history, 0, 1);
    wr(&mut history, 100, 2);

    assert_eq!(64, history.len());
    assert_eq!(2, history.stored_items());
    assert_eq!(Value(&A(2)), history.get(100).deref());
    assert_eq!(Value(&A(1)), history.get_latest(99).deref::<A>());
}

#[test]
fn gap_within_capacity_moves_boundary_removed() {
    let mut history = a_hist(5);

    history.mark_removed(0);
    unsafe { history.list.append(None::<fn(PtrMut)>) };
    history.removed_mask = 0b10;
    history.last_tick = 1;

    wr(&mut history, 6, 1);

    assert_eq!(5, history.len());
    assert_eq!(Value(&A(1)), history.get(6).deref());
    assert_eq!(Removed, history.get_latest(5).deref::<A>());
}

#[test]
fn mark_removed_beyond_capacity_is_ignored() {
    let mut history = a_hist(5);
    wr(&mut history, 10, 1);

    history.mark_removed(2);

    assert_eq!(1, history.len());
    assert_eq!(Value(&A(1)), history.get(10).deref());
}

#[test]
fn mark_removed_extends_front_for_old_tick() {
    let mut history = a_hist(5);
    wr(&mut history, 8, 1);
    wr(&mut history, 9, 2);
    wr(&mut history, 10, 3);
    assert_eq!(3, history.len());

    history.mark_removed(6);

    assert_eq!(5, history.len());
    assert_eq!(Removed, history.get(6).deref::<A>());
}

#[test]
fn gap_overflow_boundary_already_has_value() {
    let mut history = a_hist(5);
    for i in 0..=3 {
        wr(&mut history, i, i as u16 + 1);
    }

    wr(&mut history, 6, 9);

    assert_eq!(5, history.len());
    assert_eq!(Value(&A(9)), history.get(6).deref());
}

#[test]
fn gap_within_capacity_nothing_to_retain() {
    let mut history = a_hist(5);

    unsafe { history.list.append(None::<fn(PtrMut)>) };
    unsafe { history.list.append(None::<fn(PtrMut)>) };
    history.last_tick = 1;

    wr(&mut history, 6, 1);

    assert_eq!(5, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(Value(&A(1)), history.get(6).deref());
    for i in 2..=5 {
        assert_eq!(Missing, history.get(i).deref::<A>());
    }
}
