//! Tests for the blobby ring buffer (`src/history/blob_deque.rs`).

#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/comp_b.rs"]
mod comp_b;
#[path = "support/comp_c.rs"]
mod comp_c;
#[path = "support/drops.rs"]
mod drops;
#[path = "support/map_deref.rs"]
mod map_deref;

use comp_a::A;
use comp_b::B;
use comp_c::C;
use drops::{D, DropList, assert_drops};
use map_deref::MapDeref;

use std::alloc::Layout;
use std::mem::MaybeUninit;
use std::num::NonZero;

use bevy::ptr::PtrMut;
use bevy_rewind::history::blob_deque::{BlobDeque, array_layout};

trait MapDerefMut<'a> {
    fn deref<T>(self) -> Option<&'a mut T>;
}

// WARN: This function is actually unsafe, but not marked as such to avoid cluttering the tests
// DO NOT USE THIS OUTSIDE OF TESTS!
impl<'a> MapDerefMut<'a> for Option<PtrMut<'a>> {
    fn deref<T>(self) -> Option<&'a mut T> {
        self.map(|v| unsafe { v.deref_mut::<T>() })
    }
}

// Closures are funnelled through these monomorphic helpers so the generic
// `append`/`insert` methods are instantiated once per helper, letting a single
// instantiation exercise every branch.

fn ba(h: &mut BlobDeque, v: u16) {
    unsafe { h.append(|ptr| *ptr.deref_mut::<A>() = A(v)) };
}

fn bc(h: &mut BlobDeque, x: u8, y: u16) {
    unsafe { h.append(|ptr| *ptr.deref_mut::<C>() = C(x, y)) };
}

fn ba_d(h: &mut BlobDeque, v: u16, drops: &DropList) {
    unsafe {
        h.append(|ptr| {
            ptr.deref_mut::<MaybeUninit<D>>().write(D::new(v, drops));
        });
    };
}

// Zero-sized appends/inserts funnel through the same value closures: the closure
// is never invoked for a zero-sized deque (`new_ptr` yields `None`), so a single
// instantiation covers both the value and zero-sized paths.
fn bi(h: &mut BlobDeque, at: usize, v: u16) -> Option<()> {
    unsafe { h.insert(at, |ptr| *ptr.deref_mut::<A>() = A(v)) }
}

#[test]
fn get_in_bounds() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    for i in 1..=5 {
        ba(&mut history, i);
    }

    for i in 0..5 {
        assert_eq!(Some(&A(i as u16 + 1)), history.get(i).deref::<A>());
    }
}

#[test]
fn get_out_of_bounds() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    for i in 1..=4 {
        ba(&mut history, i);
    }

    // Out of bounds, within capacity
    assert_eq!(None, history.get(4).deref::<A>());
    // Out of bounds and out of capacity
    assert_eq!(None, history.get(5).deref::<A>());
}

#[test]
fn get_mut_in_bounds() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    for i in 1..=5 {
        ba(&mut history, i);
    }

    for i in 0..5 {
        assert_eq!(Some(&mut A(i as u16 + 1)), history.get_mut(i).deref::<A>());
    }
}

#[test]
fn get_mut_out_of_bounds() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    for i in 1..=4 {
        ba(&mut history, i);
    }

    // Out of bounds, within capacity
    assert_eq!(None, history.get_mut(4).deref::<A>());
    // Out of bounds and out of capacity
    assert_eq!(None, history.get_mut(5).deref::<A>());
}

#[test]
fn get_mut_zst_is_none() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(5).unwrap());

    for _ in 1..=5 {
        ba(&mut history, 0);
    }

    for i in 0..=6 {
        assert_eq!(None, history.get_mut(i).deref::<B>());
    }
}

#[test]
fn append_get_sized() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    assert_eq!(None, unsafe { history.get(0).map(|v| v.deref::<A>()) });

    ba(&mut history, 1);
    assert_eq!(Some(&A(1)), unsafe { history.get(0).map(|v| v.deref()) });
    assert_eq!(None, unsafe { history.get(1).map(|v| v.deref::<A>()) });

    ba(&mut history, 2);
    assert_eq!(Some(&A(1)), unsafe { history.get(0).map(|v| v.deref()) });
    assert_eq!(Some(&A(2)), unsafe { history.get(1).map(|v| v.deref()) });
    assert_eq!(None, unsafe { history.get(2).map(|v| v.deref::<A>()) });
}

#[test]
fn append_get_zst() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(5).unwrap());

    assert_eq!(None, history.get(0).map(|v| unsafe { v.deref::<B>() }));
    assert_eq!(None, history.get(1).map(|v| unsafe { v.deref::<B>() }));

    ba(&mut history, 0);
    assert_eq!(Some(&B), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(1).map(|v| unsafe { v.deref::<B>() }));

    ba(&mut history, 0);
    assert_eq!(Some(&B), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&B), history.get(1).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(2).map(|v| unsafe { v.deref::<B>() }));
}

#[test]
fn append_get_alignment() {
    let mut history = BlobDeque::new(Layout::new::<C>(), None, NonZero::new(5).unwrap());

    assert_eq!(None, history.get(0).map(|v| unsafe { v.deref::<C>() }));
    assert_eq!(None, history.get(1).map(|v| unsafe { v.deref::<C>() }));

    bc(&mut history, 1, 2);
    assert_eq!(Some(&C(1, 2)), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(1).map(|v| unsafe { v.deref::<C>() }));

    bc(&mut history, 4, 3);
    assert_eq!(Some(&C(1, 2)), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&C(4, 3)), history.get(1).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(2).map(|v| unsafe { v.deref::<C>() }));
}

#[test]
fn wraps() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(3).unwrap());

    for i in 1..=5 {
        // Write 1, 2, 3, 4, 5
        ba(&mut history, i);
    }
    // Only 3, 4, 5 should be in the list
    assert_eq!(3, history.len);
    assert_eq!(3, history.capacity);
    assert_eq!(Some(&A(3)), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&A(4)), history.get(1).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&A(5)), history.get(2).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(3).map(|v| unsafe { v.deref::<A>() }));
}

#[test]
fn wraps_zst() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(3).unwrap());

    for _ in 1..=20 {
        ba(&mut history, 0);
    }
    // Only 3 values should be in the history
    assert_eq!(3, history.len);
    assert_eq!(3, history.capacity);
    assert_eq!(Some(&B), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&B), history.get(1).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&B), history.get(2).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(3).map(|v| unsafe { v.deref::<A>() }));
}

#[test]
fn wraps_many_times() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(3).unwrap());

    for i in 0..100 {
        ba(&mut history, i);
    }
    assert_eq!(Some(&A(97)), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&A(98)), history.get(1).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&A(99)), history.get(2).map(|v| unsafe { v.deref() }));
    assert_eq!(None, history.get(3).map(|v| unsafe { v.deref::<A>() }));
}

#[test]
fn insert_trivial() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    // Add the item to the back
    bi(&mut history, 0, 2).unwrap();
    // Add the item to the front
    bi(&mut history, 0, 1).unwrap();
    // Add the item to the back, but this time the list isn't empty
    bi(&mut history, 2, 3).unwrap();

    assert_eq!(3, history.len());
    for i in 0..3 {
        assert_eq!(
            Some(&A(i as u16 + 1)),
            history.get(i).map(|v| unsafe { v.deref() })
        );
    }
    assert_eq!(None, history.get(3).map(|v| unsafe { v.deref::<A>() }));
}

#[test]
fn insert_errors() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());

    ba(&mut history, 1);

    // Not connected to current items
    let res = bi(&mut history, 2, 0);
    assert!(res.is_none());

    for _ in 0..4 {
        ba(&mut history, 1);
    }

    // No capacity
    let res = bi(&mut history, 0, 0);
    assert!(res.is_none());
}

#[test]
fn insert_moves() {
    // Check both at wrapping capacity and some space over
    for cap in 7..=8 {
        // Check at all start positions to make sure we hit every move condition
        for start in 0..cap {
            insert_move_with_start(start, cap);
        }
    }
}

fn insert_move_with_start(start: u8, cap: u8) {
    let case_str = format!("Case: start {}, cap {}", start, cap);
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(cap).unwrap());

    history.start = start;
    for i in [1, 2, 3, 5, 6, 7] {
        ba(&mut history, i);
    }

    // Insert an item in between
    bi(&mut history, 3, 4).unwrap();

    assert_eq!(7, history.len(), "{}", case_str);
    for i in 0..7 {
        assert_eq!(
            Some(&A(i as u16 + 1)),
            history.get(i).map(|v| unsafe { v.deref() }),
            "{}",
            case_str
        );
    }
    assert_eq!(
        None,
        history.get(7).map(|v| unsafe { v.deref::<A>() }),
        "{}",
        case_str
    );
}

#[test]
fn shrink() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());
    let old_ptr = history.data;

    // We write enough values so we can test items getting removed after shrinking
    for i in 1..=5 {
        ba(&mut history, i);
    }
    assert_eq!(5, history.len);
    assert_eq!(5, history.capacity);
    assert_eq!(Some(&A(1)), history.get(0).map(|v| unsafe { v.deref() }));

    history.resize(NonZero::new(3).unwrap());

    assert_ne!(old_ptr, history.data);
    assert_eq!(3, history.len);
    assert_eq!(3, history.capacity);
    assert_eq!(0, history.start);

    // We should only have the last 3 values
    for (i, v) in (3..=5).enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).map(|v| unsafe { v.deref() }));
    }
}

#[test]
fn shrink_wrapped() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(5).unwrap());
    let old_ptr = history.data;

    // We write 7 values to a history of 5 items, so it's wrapped in such a way
    // that shrinking it down to 3 items needs to copy from both sides
    for i in 1..=7 {
        ba(&mut history, i);
    }

    assert_eq!(5, history.len);
    assert_eq!(5, history.capacity);
    assert_eq!(2, history.start);
    assert_eq!(Some(&A(3)), history.get(0).map(|v| unsafe { v.deref() }));

    history.resize(NonZero::new(3).unwrap());

    assert_ne!(old_ptr, history.data);
    assert_eq!(3, history.len);
    assert_eq!(3, history.capacity);
    assert_eq!(0, history.start);

    // We should only have the last 3 values
    for (i, v) in (5..=7).enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).map(|v| unsafe { v.deref() }));
    }
}

#[test]
fn resize_same_size() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(3).unwrap());
    let old_ptr = history.data;

    history.resize(NonZero::new(3).unwrap());

    assert_eq!(old_ptr, history.data);
}

#[test]
fn grow() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(3).unwrap());
    let old_ptr = history.data;

    // We fully fill up our history
    for i in 1..=3 {
        ba(&mut history, i);
    }
    assert_eq!(3, history.len);
    assert_eq!(3, history.capacity);
    assert_eq!(Some(&A(1)), history.get(0).map(|v| unsafe { v.deref() }));

    history.resize(NonZero::new(5).unwrap());

    assert_ne!(old_ptr, history.data);
    assert_eq!(3, history.len);
    assert_eq!(5, history.capacity);
    assert_eq!(0, history.start);

    // We should be able to write more values
    for i in 4..=5 {
        ba(&mut history, i);
    }

    assert_eq!(5, history.len);
    assert_eq!(0, history.start);

    for (i, v) in (1..=5).enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).map(|v| unsafe { v.deref() }));
    }
}

fn d_hist(size: u8) -> BlobDeque {
    BlobDeque::new(
        Layout::new::<D>(),
        Some(|ptr| unsafe { ptr.drop_as::<D>() }),
        NonZero::new(size).unwrap(),
    )
}

#[test]
fn drop_history() {
    drop_history_with_start(0);
}

#[test]
fn drop_history_offset() {
    for i in 1..=4 {
        drop_history_with_start(i);
    }
}

fn drop_history_with_start(start: u8) {
    let drops = DropList::default();
    let mut history = d_hist(5);
    history.start = start;

    for i in 1..=5 {
        ba_d(&mut history, i, &drops);
    }
    assert_eq!(5, history.len);
    assert_drops(&drops, []);

    drop(history);

    assert_drops(&drops, [1, 2, 3, 4, 5]);
}

#[test]
fn shrink_drop() {
    let drops = DropList::default();
    let mut history = d_hist(5);

    for i in 1..=5 {
        ba_d(&mut history, i, &drops);
    }
    assert_eq!(5, history.len);
    assert_drops(&drops, []);

    history.resize(NonZero::new(3).unwrap());

    assert_eq!(3, history.len);
    assert_drops(&drops, [1, 2]);

    drop(history);

    assert_drops(&drops, [1, 2, 3, 4, 5]);
}

#[test]
fn wrap_drop() {
    let drops = DropList::default();
    let mut history = d_hist(5);

    for i in 1..=5 {
        ba_d(&mut history, i, &drops);
    }
    assert_eq!(5, history.len);
    assert_drops(&drops, []);

    for i in 6..=9 {
        ba_d(&mut history, i, &drops);
    }
    assert_eq!(5, history.len);
    assert_drops(&drops, [1, 2, 3, 4]);

    drop(history);

    assert_drops(&drops, [1, 2, 3, 4, 5, 6, 7, 8, 9]);
}

#[test]
fn drop_front() {
    let drops = DropList::default();
    let mut history = d_hist(5);

    for i in 1..=5 {
        ba_d(&mut history, i, &drops);
    }
    assert_eq!(5, history.len);
    assert_drops(&drops, []);

    history.drop_front();

    assert_eq!(4, history.len);
    assert_drops(&drops, [1]);

    history.drop_front();

    assert_eq!(3, history.len);
    assert_drops(&drops, [1, 2]);

    drop(history);
    assert_drops(&drops, [1, 2, 3, 4, 5]);
}

#[test]
fn drop_front_small_or_empty() {
    let drops = DropList::default();
    let mut history = d_hist(5);

    ba_d(&mut history, 1, &drops);
    assert_eq!(1, history.len);
    assert_drops(&drops, []);

    history.drop_front();

    assert_eq!(0, history.len);
    assert_drops(&drops, [1]);

    history.drop_front();
    assert_drops(&drops, [1]);

    drop(history);
    assert_drops(&drops, [1]);
}

#[test]
fn drop_back() {
    let drops = DropList::default();
    let mut history = d_hist(5);

    for i in 1..=5 {
        ba_d(&mut history, i, &drops);
    }
    assert_eq!(5, history.len);
    assert_drops(&drops, []);

    history.drop_back();

    assert_eq!(4, history.len);
    assert_drops(&drops, [5]);

    history.drop_back();

    assert_eq!(3, history.len);
    assert_drops(&drops, [5, 4]);

    drop(history);
    assert_drops(&drops, [5, 4, 1, 2, 3]);
}

#[test]
fn drop_back_small_or_empty() {
    let drops = DropList::default();
    let mut history = d_hist(5);

    ba_d(&mut history, 1, &drops);
    assert_eq!(1, history.len);
    assert_drops(&drops, []);

    history.drop_back();

    assert_eq!(0, history.len);
    assert_drops(&drops, [1]);

    history.drop_back();
    assert_drops(&drops, [1]);

    drop(history);
    assert_drops(&drops, [1]);
}

#[test]
fn drop_front_zst() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(3).unwrap());

    for _ in 0..3 {
        ba(&mut history, 0);
    }
    assert_eq!(3, history.len);

    history.drop_front();

    // ZST fronts don't advance the start; only the length shrinks
    assert_eq!(2, history.len);
    assert_eq!(0, history.start);
}

#[test]
fn drop_back_zst() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(3).unwrap());

    for _ in 0..3 {
        ba(&mut history, 0);
    }
    assert_eq!(3, history.len);

    history.drop_back();

    assert_eq!(2, history.len);
}

#[test]
fn insert_zst() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(3).unwrap());

    bi(&mut history, 0, 0).unwrap();
    bi(&mut history, 1, 0).unwrap();

    assert_eq!(2, history.len);
    assert_eq!(Some(&B), history.get(0).map(|v| unsafe { v.deref() }));
    assert_eq!(Some(&B), history.get(1).map(|v| unsafe { v.deref() }));
}

#[test]
fn insert_at_front_wrapped() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(4).unwrap());

    // Wrap the buffer so start != 0, then make room at the front
    for i in 1..=5 {
        ba(&mut history, i);
    }
    assert_eq!(1, history.start);
    history.drop_front();
    history.drop_front();
    assert_eq!(3, history.start);
    assert_eq!(2, history.len);

    // Insert at the front with a non-zero start: the start steps back
    bi(&mut history, 0, 3).unwrap();

    assert_eq!(2, history.start);
    assert_eq!(3, history.len);
    for (i, v) in (3..=5).enumerate() {
        assert_eq!(Some(&A(v)), history.get(i).map(|v| unsafe { v.deref() }));
    }
}

#[test]
fn resize_zst() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(5).unwrap());

    for _ in 0..4 {
        ba(&mut history, 0);
    }
    assert_eq!(4, history.len);

    history.resize(NonZero::new(2).unwrap());

    assert_eq!(2, history.len);
    assert_eq!(2, history.capacity);

    history.resize(NonZero::new(6).unwrap());

    assert_eq!(2, history.len);
    assert_eq!(6, history.capacity);
}

#[test]
fn shrink_start_past_new_capacity() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(4).unwrap());

    // Wrap so that start = 2, then shrink to 1: the lost items push the start
    // past the old capacity, so the copy comes entirely from the wrapped part.
    for i in 1..=6 {
        ba(&mut history, i);
    }
    assert_eq!(2, history.start);
    assert_eq!(4, history.len);

    history.resize(NonZero::new(1).unwrap());

    assert_eq!(1, history.len);
    assert_eq!(1, history.capacity);
    assert_eq!(0, history.start);
    assert_eq!(Some(&A(6)), history.get(0).map(|v| unsafe { v.deref() }));
}

#[test]
fn debug_empty() {
    let history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(3).unwrap());

    let repr = format!("{history:?}");
    assert!(repr.contains("capacity: 3"), "{repr}");
    assert!(repr.contains("len: 0"), "{repr}");
    assert!(repr.contains("\"[]\""), "{repr}");
}

#[test]
fn debug_items_hex() {
    let mut history = BlobDeque::new(Layout::new::<A>(), None, NonZero::new(3).unwrap());

    ba(&mut history, 1);
    ba(&mut history, 2);

    // A is a native-endian u16; format both byte orders to stay portable
    let (one, two) = if cfg!(target_endian = "little") {
        ("0x0100", "0x0200")
    } else {
        ("0x0001", "0x0002")
    };
    let repr = format!("{history:?}");
    assert!(repr.contains(&format!("\"[{one}, {two}]\"")), "{repr}");
}

#[test]
fn debug_zst_items() {
    let mut history = BlobDeque::new(Layout::new::<B>(), None, NonZero::new(3).unwrap());

    ba(&mut history, 0);
    ba(&mut history, 0);

    let repr = format!("{history:?}");
    assert!(repr.contains("\"[-, -]\""), "{repr}");
}

#[test]
fn array_layout_overflow_is_none() {
    // Three maximum-size items overflow `usize`, so no layout exists
    let huge = Layout::from_size_align(isize::MAX as usize - 7, 8).unwrap();
    assert!(array_layout(&huge, 3).is_none());
}

#[test]
fn array_layout_multiplies() {
    let layout = array_layout(&Layout::new::<A>(), 4).unwrap();
    assert_eq!(4 * size_of::<A>(), layout.size());
    assert_eq!(align_of::<A>(), layout.align());
}
