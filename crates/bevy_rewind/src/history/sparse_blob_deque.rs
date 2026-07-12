#![deny(clippy::std_instead_of_alloc)]
#![deny(clippy::std_instead_of_core)]

use super::blob_deque::BlobDeque;

extern crate alloc;
use alloc::alloc::Layout;
use core::num::NonZero;

use bevy::ptr::{OwningPtr, Ptr, PtrMut};

pub struct SparseBlobDeque {
    mask: u64,
    len: u8,
    capacity: u8,
    pub items: BlobDeque,
}

impl core::fmt::Debug for SparseBlobDeque {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SparseBlobDeque")
            .field("capacity", &self.capacity)
            .field("len", &self.len)
            .field("mask", &format!("{:01$b}", self.mask, self.len as usize))
            .field("items", &self.items)
            .finish()
    }
}

impl SparseBlobDeque {
    /// SAFETY: The layout and drop function MUST match the type this collection will be used for
    pub(super) unsafe fn new(
        layout: Layout,
        drop: Option<unsafe fn(OwningPtr<'_>)>,
        cap: NonZero<u8>,
    ) -> Self {
        let capacity = cap.get();
        if !(1..=64).contains(&capacity) {
            panic!("SparseBlobDeque capacity MUST be at least 1 and at most 64");
        }
        Self {
            mask: 0,
            len: 0,
            capacity,
            items: BlobDeque::new(layout, drop, unsafe { NonZero::new_unchecked(1) }),
        }
    }

    pub fn len(&self) -> usize {
        self.len as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn capacity(&self) -> usize {
        self.capacity as usize
    }

    pub fn stored_items(&self) -> usize {
        self.items.len()
    }

    // The least significant bit is the back of the collection.
    pub fn mask(&self) -> u64 {
        self.mask
    }

    pub fn mask_mut(&mut self) -> &mut u64 {
        &mut self.mask
    }

    pub fn get<'a>(&'a self, index: usize) -> Option<Ptr<'a>> {
        if index >= self.len as usize {
            return None;
        }
        let index_bit = 1 << (self.len as u64 - 1 - index as u64);
        if self.mask & index_bit == 0 {
            return None;
        }
        let search_mask = !(index_bit - 1);
        let item_index = (self.mask & search_mask).count_ones() - 1;
        self.items.get(item_index as usize)
    }

    /// # Safety
    /// - The value written in `write_fn` MUST match the type this collection was made for
    /// - `write_fn` MUST write to the [`PtrMut`], or the value will be uninitialized
    pub unsafe fn append<'a>(&mut self, write_fn: Option<impl FnOnce(PtrMut<'a>)>) {
        if self.len == self.capacity {
            let index_bit = 1 << (self.len - 1);
            if self.mask & index_bit != 0 {
                self.items.drop_front();
            }
            self.mask &= !index_bit;
            self.len -= 1;
        }

        self.mask = self.mask.wrapping_shl(1);
        if let Some(write_fn) = write_fn {
            if self.items.capacity() == self.items.len() && self.items.capacity() != self.capacity()
            {
                let new_cap = unsafe { NonZero::new_unchecked(self.items.capacity() as u8 + 1) };
                self.items.resize(new_cap);
            }
            unsafe { self.items.append(write_fn) };
            self.mask |= 1;
        }
        self.len += 1;
    }

    pub fn extend_front(&mut self, n: usize) {
        self.len += (n as u8).min(self.capacity - self.len);
    }

    pub fn extend_back(&mut self, n: usize) {
        if n >= self.capacity() {
            self.items.clear();
            self.mask = 0;
            self.len = self.capacity;
            return;
        }

        let search_mask = ((1u64 << n) - 1).wrapping_shl(self.capacity as u32 - n as u32);
        let ones = (self.mask & search_mask).count_ones();
        for _ in 0..ones {
            self.items.drop_front();
        }

        self.mask = (self.mask & !search_mask).wrapping_shl(n as u32);
        self.len = (self.len + n as u8).min(self.capacity);
    }

    pub fn trim_back(&mut self, n: usize) {
        if n >= self.len() {
            self.clear();
            return;
        }

        let search_mask = (1 << n) - 1;
        let items_to_drop = (self.mask & search_mask).count_ones();
        for _ in 0..items_to_drop {
            self.items.drop_back();
        }
        self.mask = self.mask.wrapping_shr(n as u32);
        self.len -= n as u8;
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.mask = 0;
        self.len = 0;
    }

    /// # Safety
    /// - The value written in `write_fn` MUST match the type this collection was made for
    /// - `write_fn` MUST write to the [`PtrMut`], or the value will be uninitialized
    pub unsafe fn replace(&mut self, index: usize, write_fn: impl FnOnce(PtrMut)) {
        if index >= self.len() {
            return;
        }

        let index_bit = 1 << (self.len as u64 - 1 - index as u64);
        let search_mask = !(index_bit - 1);
        let ones = (self.mask & search_mask).count_ones();
        if self.mask & index_bit != 0 {
            let drop_fn = self.items.drop();
            if let Some(mut ptr) = self.items.get_mut(ones as usize - 1) {
                drop_fn.inspect(|f| unsafe { f(ptr.reborrow().promote()) });
                write_fn(ptr);
            }
            return;
        }

        if self.items.len() == self.items.capacity() {
            self.items
                .resize(unsafe { NonZero::new_unchecked(self.items.capacity() as u8 + 1) });
        }

        if (self.mask & !search_mask) == 0 {
            self.mask |= index_bit;
            unsafe { self.items.append(write_fn) };
            return;
        }

        self.mask |= index_bit;
        unsafe { self.items.insert(ones as usize, write_fn).unwrap() };
    }
}
