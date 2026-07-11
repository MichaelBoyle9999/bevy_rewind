//! Tests for the sparse blobby ring buffer (`src/history/sparse_blob_deque.rs`).

#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/comp_b.rs"]
mod comp_b;
#[path = "support/drops.rs"]
mod drops;
#[path = "support/iter_enumerate.rs"]
mod iter_enumerate;
#[path = "support/map_deref.rs"]
mod map_deref;

use comp_a::A;
use comp_b::B;
use drops::{D, DropList, assert_drops};
use iter_enumerate::IterEnumerate;
use map_deref::MapDeref;

use core::mem::MaybeUninit;
use core::num::NonZero;

use bevy::ptr::PtrMut;
use bevy_rewind::history::sparse_blob_deque::SparseBlobDeque;

// Closures are funnelled through these monomorphic helpers so the generic
// `append`/`replace` methods are instantiated once per helper (not once per
// call-site), letting a single instantiation exercise every branch. The value is
// passed as an `Option` and mapped into the closure, so the `Some` (store) and
// `None` (empty slot) paths share one instantiation.

/// Append an `A(v)` value, or an empty (sparse) slot when `v` is `None`.
fn ap(h: &mut SparseBlobDeque, v: Option<u16>) {
    unsafe { h.append(v.map(|v| move |ptr: PtrMut| *ptr.deref_mut::<A>() = A(v))) };
}

/// Append a drop-tracked `D(v)` value, or an empty slot when `v` is `None`.
fn ad(h: &mut SparseBlobDeque, v: Option<u16>, drops: &DropList) {
    unsafe {
        h.append(v.map(|v| {
            move |ptr: PtrMut| {
                ptr.deref_mut::<MaybeUninit<D>>().write(D::new(v, drops));
            }
        }));
    }
}

/// Replace the slot at `i` with `A(v)`. Zero-sized replaces reuse this helper:
/// the closure is never invoked for a zero-sized deque (`items.get_mut` yields
/// `None`), so a single instantiation covers both paths.
fn rep(h: &mut SparseBlobDeque, i: usize, v: u16) {
    unsafe { h.replace(i, |ptr| *ptr.deref_mut::<A>() = A(v)) };
}

#[test]
fn get() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());
    assert_eq!(None, history.get(0).deref::<A>());

    for i in 0..3 {
        if i % 2 == 0 {
            ap(&mut history, Some(i * 5));
        } else {
            ap(&mut history, None);
        }
    }
    ap(&mut history, Some(3));

    for (i, a) in [Some(&A(0)), None, Some(&A(10)), Some(&A(3)), None].iter_enumerate() {
        assert_eq!(a, history.get(i).deref());
    }
}

#[test]
fn append_full() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());

    for i in 0..5 {
        ap(&mut history, Some(i + 1));
    }

    assert_eq!(5, history.len());
    assert_eq!(5, history.stored_items());
    for i in 0..5 {
        assert_eq!(Some(&A(i as u16 + 1)), history.get(i).deref::<A>());
    }

    ap(&mut history, Some(6));
    assert_eq!(5, history.len());
    assert_eq!(5, history.stored_items());
    for i in 0..5 {
        assert_eq!(Some(&A(i as u16 + 2)), history.get(i).deref::<A>());
    }
}

#[test]
fn dense_storage() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(10).unwrap());
    assert_eq!(None, history.get(0).deref::<A>());
    assert_eq!(1, history.items.capacity());

    for _ in 0..5 {
        ap(&mut history, None);
    }

    // None items shouldn't add capacity
    assert_eq!(5, history.len());
    assert_eq!(1, history.items.capacity());

    ap(&mut history, Some(1));
    // We shouldn't need to expand yet
    assert_eq!(1, history.items.len());
    assert_eq!(1, history.items.capacity());

    ap(&mut history, Some(2));
    // Expand to fit just the new item
    assert_eq!(2, history.items.len());
    assert_eq!(2, history.items.capacity());

    for _ in 0..10 {
        ap(&mut history, None);
    }

    // We don't release memory if the items are wrapped out of history
    assert_eq!(0, history.items.len());
    assert_eq!(2, history.items.capacity());

    for i in 0..10 {
        ap(&mut history, Some(i));
    }

    // We should never make it exceed our own capacity
    assert_eq!(10, history.items.len());
    assert_eq!(10, history.items.capacity());
}

#[test]
fn append_get_max_mask() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(64).unwrap());
    assert_eq!(None, history.get(0).deref::<A>());

    for i in 0..(64 + 24) {
        if i % 2 == 0 {
            ap(&mut history, Some(i));
        } else {
            ap(&mut history, None);
        }
    }

    assert_eq!(64, history.len());

    for i in 0..64 {
        let a = history.get(i);
        if i % 2 == 0 {
            assert_eq!(Some(&A(i as u16 + 24)), a.deref());
        } else {
            assert_eq!(None, a.deref::<A>());
        }
    }
}

#[test]
fn append_sparse_wrap_drops_items() {
    let mut history = SparseBlobDeque::from_type::<D>(NonZero::new(5).unwrap());
    let drops = DropList::default();

    for i in 0..6 {
        if i % 2 == 0 {
            ad(&mut history, Some(i), &drops);
        } else {
            // Empty slots go through the D helper too, so the `None` path shares
            // the drop-tracked closure's instantiation.
            ad(&mut history, None, &drops);
        }
    }

    assert_eq!(5, history.len());
    assert_eq!(2, history.stored_items());
    assert_drops(&drops, [0]);

    for i in [0, 2, 4, 5] {
        assert_eq!(None, history.get(i).deref::<D>());
    }
    for i in [1, 3] {
        assert_eq!(Some(i as u16 + 1), history.get(i).deref::<D>().map(|v| v.0));
    }

    drop(history);
    assert_drops(&drops, [0, 2, 4]);
}

#[test]
fn append_dense_wrap_drops_items_full() {
    let mut history = SparseBlobDeque::from_type::<D>(NonZero::new(5).unwrap());
    assert_eq!(None, history.get(0).deref::<D>());
    let drops = DropList::default();

    for i in 0..5 {
        ad(&mut history, Some(i + 1), &drops);
    }

    assert_eq!(5, history.len());
    assert_eq!(5, history.stored_items());
    assert_drops(&drops, []);

    ad(&mut history, Some(6), &drops);
    assert_eq!(5, history.len());
    assert_eq!(5, history.stored_items());
    assert_drops(&drops, [1]);

    drop(history);
    assert_drops(&drops, [1, 2, 3, 4, 5, 6]);
}

#[test]
fn extend_front() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());

    ap(&mut history, Some(1));
    assert_eq!(1, history.len());
    assert_eq!(Some(&A(1)), history.get(0).deref());

    history.extend_front(2);
    assert_eq!(3, history.len());
    for i in 0..2 {
        assert_eq!(None, history.get(i).deref::<A>());
    }
    assert_eq!(Some(&A(1)), history.get(2).deref());

    history.extend_front(7);
    assert_eq!(5, history.len());
    for i in 0..4 {
        assert_eq!(None, history.get(i).deref::<A>());
    }
    assert_eq!(Some(&A(1)), history.get(4).deref());
}

#[test]
fn extend_back() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());

    ap(&mut history, Some(1));
    assert_eq!(1, history.len());

    // Extend the back without needing to remove anything
    history.extend_back(2);
    assert_eq!(3, history.len());
    assert_eq!(Some(&A(1)), history.get(0).deref());
    for i in 1..4 {
        assert_eq!(None, history.get(i).deref::<A>());
    }

    ap(&mut history, Some(2));
    ap(&mut history, Some(3));
    assert_eq!(5, history.len());
    assert_eq!(3, history.stored_items());

    // Wrap items out of history with empty items
    history.extend_back(4);
    eprintln!("{:?}", history);
    assert_eq!(5, history.len());
    assert_eq!(1, history.stored_items());
    assert_eq!(Some(&A(3)), history.get(0).deref());
    for i in 1..6 {
        assert_eq!(None, history.get(i).deref::<A>());
    }

    // Wrap more than full capacity
    history.extend_back(7);
    assert_eq!(5, history.len());
    assert_eq!(0, history.stored_items());
    for i in 0..6 {
        assert_eq!(None, history.get(i).deref::<A>());
    }
}

#[test]
fn trim_back() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());

    for i in 1..=5 {
        ap(&mut history, Some(i));
    }
    assert_eq!(5, history.len());

    history.trim_back(1);
    assert_eq!(4, history.len());
    for (i, v) in (1..=4).iter_enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref());
    }
    assert_eq!(None, history.get(4).deref::<A>());

    history.trim_back(2);
    assert_eq!(2, history.len());
    for (i, v) in (1..=2).iter_enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref());
    }
    assert_eq!(None, history.get(3).deref::<A>());

    history.extend_back(2);
    assert_eq!(4, history.len());
    for (i, v) in (1..=2).iter_enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref());
    }
    for i in 3..5 {
        assert_eq!(None, history.get(i).deref::<A>());
    }

    history.trim_back(1);
    assert_eq!(3, history.len());
    for (i, v) in (1..=2).iter_enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref());
    }

    history.trim_back(6);
    assert_eq!(0, history.len());
    for i in 0..6 {
        assert_eq!(None, history.get(i).deref::<A>());
    }
}

#[test]
fn replace() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());

    for i in 1..=3 {
        ap(&mut history, Some(i));
    }

    assert_eq!(3, history.len());
    assert_eq!(3, history.stored_items());

    rep(&mut history, 1, 5);
    for (i, v) in [1, 5, 3].into_iter().enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref::<A>());
    }

    rep(&mut history, 2, 6);
    for (i, v) in [1, 5, 6].into_iter().enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref::<A>());
    }

    rep(&mut history, 0, 4);
    for (i, v) in (4..=6).enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).deref::<A>());
    }
}

#[test]
fn replace_empty() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());

    for _ in 0..3 {
        ap(&mut history, None);
    }

    assert_eq!(3, history.len());
    assert_eq!(0, history.stored_items());

    rep(&mut history, 1, 2);
    assert_eq!(Some(&A(2)), history.get(1).deref());
    assert_eq!(None, history.get(0).deref::<A>());
    assert_eq!(None, history.get(2).deref::<A>());

    rep(&mut history, 2, 3);
    assert_eq!(Some(&A(3)), history.get(2).deref::<A>());
    assert_eq!(None, history.get(0).deref::<A>());

    rep(&mut history, 0, 1);

    for i in 0..3 {
        assert_eq!(Some(&A(i as u16 + 1)), history.get(i).deref::<A>());
    }
}

#[test]
#[should_panic(expected = "at least 1 and at most 64")]
fn capacity_over_64_panics() {
    let _ = SparseBlobDeque::from_type::<A>(NonZero::new(65).unwrap());
}

#[test]
fn replace_out_of_bounds_is_noop() {
    let mut history = SparseBlobDeque::from_type::<A>(NonZero::new(5).unwrap());
    ap(&mut history, Some(1));

    // Replacing beyond the length does nothing.
    rep(&mut history, 5, 9);

    assert_eq!(1, history.len());
    assert_eq!(Some(&A(1)), history.get(0).deref::<A>());
}

#[test]
fn replace_zero_sized_existing_slot() {
    // A zero-sized type stores no bytes, so `items.get_mut` on an existing
    // (bit-set) slot yields `None`, taking the no-op replace path.
    let mut history = SparseBlobDeque::from_type::<B>(NonZero::new(5).unwrap());
    ap(&mut history, Some(0));
    assert_eq!(1, history.stored_items());

    rep(&mut history, 0, 0);
    assert_eq!(1, history.len());
    assert_eq!(1, history.stored_items());
}
