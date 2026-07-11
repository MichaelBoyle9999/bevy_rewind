#![deny(clippy::std_instead_of_alloc)]
#![deny(clippy::std_instead_of_core)]

extern crate alloc;
use alloc::alloc::{Layout, alloc, dealloc};
use core::{num::NonZero, ptr::NonNull};

use bevy::ptr::{OwningPtr, Ptr, PtrMut};

/// A blobby ring buffer with support for gaps
pub struct BlobDeque {
    /// The memory layout of each item
    layout: Layout,
    /// Capacity in items, not bytes
    pub capacity: u8,
    /// The length in items, not bytes
    pub len: u8,
    /// The start of the ringbuffer in items, not bytes
    pub start: u8,
    /// The ring buffer's data
    pub data: NonNull<u8>,
    /// The function to drop items, if any
    drop: Option<unsafe fn(OwningPtr<'_>)>,
}

impl core::fmt::Debug for BlobDeque {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // Writing to a `String` is infallible, so build the item list eagerly.
        let mut items = String::new();
        items.push('[');

        let size = self.layout.size();
        for i in 0..(self.len as usize) {
            if i != 0 {
                items.push_str(", ");
            }
            if size == 0 {
                items.push('-');
            } else {
                items.push_str("0x");
                let ptr = self.get(i).unwrap();
                for offset in 0..size {
                    let byte = unsafe { ptr.byte_add(offset).as_ptr().read() };
                    items.push_str(&format!("{byte:02x}"));
                }
            }
        }

        items.push(']');

        f.debug_struct("BlobDeque")
            .field("capacity", &self.capacity)
            .field("len", &self.len)
            .field("start", &self.start)
            .field("items", &items)
            .finish()
    }
}

unsafe impl Send for BlobDeque {}
unsafe impl Sync for BlobDeque {}

impl BlobDeque {
    pub fn new(layout: Layout, drop: Option<unsafe fn(OwningPtr<'_>)>, size: NonZero<u8>) -> Self {
        if layout.size() == 0 {
            let align = NonZero::<usize>::new(layout.align()).expect("alignment must be > 0");
            Self {
                layout,
                capacity: size.get(),
                len: 0,
                start: 0,
                data: NonNull::without_provenance(align),
                drop,
            }
        } else {
            let data = alloc_items(&layout, size.get() as usize);
            Self {
                layout,
                capacity: size.get(),
                len: 0,
                start: 0,
                data,
                drop,
            }
        }
    }

    /// Get the length of the `BlobDeque`
    #[expect(
        clippy::len_without_is_empty,
        reason = "no consumer needs is_empty; adding it would be untested dead code"
    )]
    pub fn len(&self) -> usize {
        self.len as usize
    }

    /// Get the capacity of the `BlobDeque`
    pub fn capacity(&self) -> usize {
        self.capacity as usize
    }

    pub fn drop(&self) -> Option<unsafe fn(OwningPtr<'_>)> {
        self.drop
    }

    pub fn get<'a>(&'a self, index: usize) -> Option<Ptr<'a>> {
        if (self.len as usize) < index + 1 {
            return None;
        }
        let size = self.layout.size();
        if size == 0 {
            return Some(unsafe { Ptr::new(self.data) });
        }
        let offset = self.get_offset(index);
        Some(unsafe { Ptr::new(self.data).byte_add(offset) })
    }

    pub fn get_mut<'a>(&'a mut self, index: usize) -> Option<PtrMut<'a>> {
        let size = self.layout.size();
        if size == 0 || (self.len as usize) < index + 1 {
            // size 0 cannot be mutated
            return None;
        }
        let offset = self.get_offset(index);
        Some(unsafe { PtrMut::new(self.data).byte_add(offset) })
    }

    fn get_offset(&self, index: usize) -> usize {
        ((self.start as usize + index) % self.capacity as usize) * self.layout.size()
    }

    pub fn drop_front(&mut self) {
        if self.len == 0 {
            return;
        }

        if self.layout.size() != 0 {
            self.drop
                .inspect(|f| unsafe { f(self.get_mut(0).unwrap_unchecked().promote()) });
            self.start = (self.start + 1) % self.capacity;
        }
        self.len -= 1;
    }

    pub fn drop_back(&mut self) {
        if self.len == 0 {
            return;
        }

        if self.layout.size() != 0 {
            self.drop.inspect(|f| unsafe {
                f(self.get_mut(self.len() - 1).unwrap_unchecked().promote());
            });
        }
        self.len -= 1;
    }

    /// Append a value to the back, dropping the front if at capacity
    ///
    /// # Safety
    /// - The value written in `write_fn` MUST match the type the `BlobDeque` was made for
    /// - `write_fn` MUST write to the [`PtrMut`], or the value will be uninitialized
    pub unsafe fn append<'a>(&mut self, write_fn: impl FnOnce(PtrMut<'a>)) {
        if let Some(ptr) = unsafe { self.new_ptr() } {
            write_fn(ptr);
        }
    }

    unsafe fn new_ptr<'a>(&mut self) -> Option<PtrMut<'a>> {
        if self.layout.size() == 0 {
            self.len = (self.len + 1).min(self.capacity);
            return None;
        }
        if self.len == self.capacity {
            self.drop
                .inspect(|f| unsafe { f(self.get_mut(0).unwrap_unchecked().promote()) });
            self.len -= 1;
            self.start = (self.start + 1) % self.capacity;
        }
        let offset = self.get_offset(self.len as usize);

        self.len += 1;
        Some(unsafe { PtrMut::new(self.data).byte_add(offset) })
    }

    /// Insert a value at `at`, shifting later items back
    ///
    /// # Safety
    /// - The value written in `write_fn` MUST match the type the `BlobDeque` was made for
    /// - `write_fn` MUST write to the [`PtrMut`], or the value will be uninitialized
    // TODO: Return capacity error instead of Option
    #[must_use]
    pub unsafe fn insert<'a>(
        &mut self,
        at: usize,
        write_fn: impl FnOnce(PtrMut<'a>),
    ) -> Option<()> {
        if let Some(maybe_ptr) = unsafe { self.new_ptr_at(at) } {
            if let Some(ptr) = maybe_ptr {
                write_fn(ptr);
            }
            Some(())
        } else {
            None
        }
    }

    unsafe fn new_ptr_at<'a>(&mut self, at: usize) -> Option<Option<PtrMut<'a>>> {
        if self.len == self.capacity || at > self.len() {
            return None;
        }

        let size = self.layout.size();
        if size == 0 {
            self.len = (self.len + 1).min(self.capacity);
            return Some(None);
        }

        if at == self.len() {
            // No op
        } else if at == 0 {
            if self.start == 0 {
                self.start = self.capacity - 1;
            } else {
                self.start -= 1;
            }
        } else {
            if self.capacity - self.len < self.start {
                // Shift the wrapped part of the buffer forward by one item
                let first_half = (self.capacity - self.start) as usize;

                let raw_pos = at.saturating_sub(first_half);
                let to_move = self.len() - first_half - raw_pos;
                unsafe {
                    core::ptr::copy(
                        self.data.byte_add(raw_pos * size).as_ptr(),
                        self.data.byte_add((raw_pos + 1) * size).as_ptr(),
                        to_move * size,
                    );
                }
            }

            if self.start as usize + at < self.capacity() {
                if self.start != 0 && self.capacity - self.len <= self.start {
                    // Move the item at the end to the front of the memory (before start)
                    unsafe {
                        core::ptr::copy_nonoverlapping(
                            self.data.byte_add((self.capacity() - 1) * size).as_ptr(),
                            self.data.as_ptr(),
                            size,
                        );
                    }
                }

                let move_start = self.start as usize + at;
                let to_move = self.capacity() - move_start - 1;

                if to_move != 0 {
                    unsafe {
                        core::ptr::copy(
                            self.data.byte_add(move_start * size).as_ptr(),
                            self.data.byte_add((move_start + 1) * size).as_ptr(),
                            to_move * size,
                        );
                    }
                }
            }
        }

        let offset = self.get_offset(at);

        self.len += 1;
        Some(Some(unsafe { PtrMut::new(self.data).byte_add(offset) }))
    }

    pub fn resize(&mut self, capacity: NonZero<u8>) {
        let capacity = capacity.get();
        if capacity == self.capacity {
            return;
        }

        let size = self.layout.size();
        let lost = self.len.saturating_sub(capacity);

        if size == 0 {
            self.len = self.len.min(capacity);
            self.capacity = capacity;
            return;
        }

        if lost > 0 {
            if let Some(drop) = self.drop {
                for i in 0..lost {
                    let item = unsafe { self.get_mut(i as usize).unwrap_unchecked().promote() };
                    unsafe { drop(item) };
                }
            }
            self.len -= lost;
            self.start += lost;
        }

        let new_data = alloc_items(&self.layout, capacity as usize);

        let start = self.start;
        let overflow = start.saturating_sub(self.capacity);
        let first_part = self.capacity.saturating_sub(start).min(capacity);
        if first_part > 0 {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.data.byte_add(start as usize * size).as_ptr(),
                    new_data.as_ptr(),
                    first_part as usize * size,
                );
            }
        }
        if self.start != 0 && capacity > first_part && self.len > first_part {
            let l = capacity.min(self.len) - first_part;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    self.data.byte_add(overflow as usize * size).as_ptr(),
                    new_data.byte_add(first_part as usize * size).as_ptr(),
                    l as usize * size,
                );
            }
        }

        let layout = array_layout(&self.layout, self.capacity as usize).unwrap();
        unsafe { dealloc(self.data.as_ptr(), layout) };

        self.data = new_data;
        self.capacity = capacity;
        self.start = 0;
    }

    pub fn clear(&mut self) {
        if self.layout.size() == 0 {
            self.len = 0;
            return;
        }

        if let Some(drop) = self.drop {
            for i in 0..self.len {
                let item = unsafe { self.get_mut(i as usize).unwrap_unchecked().promote() };
                unsafe { drop(item) };
            }
        }
        self.len = 0;
        self.start = 0;
    }
}

impl Drop for BlobDeque {
    fn drop(&mut self) {
        self.clear();

        if self.layout.size() > 0 {
            let layout = array_layout(&self.layout, self.capacity as usize).unwrap();
            unsafe { dealloc(self.data.as_ptr(), layout) };
        }

        self.capacity = 0;
    }
}

fn alloc_items(layout: &Layout, size: usize) -> NonNull<u8> {
    let array_layout = array_layout(layout, size).unwrap();
    let data = unsafe { alloc(array_layout) };
    NonNull::new(data).expect("BlobDeque allocation failed")
}

/// From <https://doc.rust-lang.org/beta/src/core/alloc/layout.rs.html>
pub fn array_layout(layout: &Layout, n: usize) -> Option<Layout> {
    let (array_layout, offset) = repeat_layout(layout, n)?;
    debug_assert_eq!(layout.size(), offset);
    Some(array_layout)
}

// TODO: replace with `Layout::repeat` if/when it stabilizes
/// From <https://doc.rust-lang.org/beta/src/core/alloc/layout.rs.html>
fn repeat_layout(layout: &Layout, n: usize) -> Option<(Layout, usize)> {
    // This cannot overflow. Quoting from the invariant of Layout:
    // > `size`, when rounded up to the nearest multiple of `align`,
    // > must not overflow (i.e., the rounded value must be less than
    // > `usize::MAX`)
    let padded_size = layout.size() + padding_needed_for(layout, layout.align());
    let alloc_size = padded_size.checked_mul(n)?;

    // SAFETY: self.align is already known to be valid and alloc_size has been
    // padded already.
    unsafe {
        Some((
            Layout::from_size_align_unchecked(alloc_size, layout.align()),
            padded_size,
        ))
    }
}

/// From <https://doc.rust-lang.org/beta/src/core/alloc/layout.rs.html>
const fn padding_needed_for(layout: &Layout, align: usize) -> usize {
    let len = layout.size();

    // Rounded up value is:
    //   len_rounded_up = (len + align - 1) & !(align - 1);
    // and then we return the padding difference: `len_rounded_up - len`.
    //
    // We use modular arithmetic throughout:
    //
    // 1. align is guaranteed to be > 0, so align - 1 is always
    //    valid.
    //
    // 2. `len + align - 1` can overflow by at most `align - 1`,
    //    so the &-mask with `!(align - 1)` will ensure that in the
    //    case of overflow, `len_rounded_up` will itself be 0.
    //    Thus the returned padding, when added to `len`, yields 0,
    //    which trivially satisfies the alignment `align`.
    //
    // (Of course, attempts to allocate blocks of memory whose
    // size and padding overflow in the above manner should cause
    // the allocator to yield an error anyway.)

    let len_rounded_up = len.wrapping_add(align).wrapping_sub(1) & !align.wrapping_sub(1);
    len_rounded_up.wrapping_sub(len)
}
