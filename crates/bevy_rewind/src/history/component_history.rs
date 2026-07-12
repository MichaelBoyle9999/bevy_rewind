use super::component::HistoryComponent;
use super::sparse_blob_deque::SparseBlobDeque;

use std::num::NonZero;

use bevy::{
    ecs::component::ComponentId,
    platform::collections::HashMap,
    prelude::{Deref, DerefMut},
    ptr::{Ptr, PtrMut},
};

#[derive(Default, Deref, DerefMut, Debug)]
pub struct EntityHistory {
    components: HashMap<ComponentId, ComponentHistory>,
}

pub struct ComponentHistory {
    pub removed_mask: u64,
    pub list: SparseBlobDeque,
    pub last_tick: u32,
}

impl core::fmt::Debug for ComponentHistory {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ComponentHistory")
            .field("last_tick", &self.last_tick)
            .field(
                "removed_mask",
                &format!("{:01$b}", self.removed_mask, self.list.len()),
            )
            .field("list", &self.list)
            .finish()
    }
}

#[derive(Debug)]
pub enum TickData<T> {
    Value(T),
    Removed,
    Missing,
}

impl<T: PartialEq> PartialEq for TickData<T> {
    fn eq(&self, other: &Self) -> bool {
        use TickData::*;
        match self {
            Value(t) => match other {
                Value(other) => t == other,
                _ => false,
            },
            Removed => {
                matches!(other, Removed)
            }
            Missing => {
                matches!(other, Missing)
            }
        }
    }
}

impl<T: Eq> Eq for TickData<T> {}

impl<T> TickData<T> {
    pub fn value(self) -> Option<T> {
        match self {
            TickData::Value(t) => Some(t),
            _ => None,
        }
    }
}

impl<T: Clone> TickData<&T> {
    pub fn cloned(&self) -> TickData<T> {
        use TickData::*;
        match *self {
            Value(t) => Value(t.clone()),
            Removed => Removed,
            Missing => Missing,
        }
    }
}

impl<T> TickData<T> {
    pub fn map<O>(&self, f: impl Fn(&T) -> O) -> TickData<O> {
        match self {
            TickData::Value(t) => TickData::Value(f(t)),
            TickData::Removed => TickData::Removed,
            TickData::Missing => TickData::Missing,
        }
    }
}

impl ComponentHistory {
    pub fn from_component(component: &HistoryComponent, size: NonZero<u8>) -> Self {
        Self {
            removed_mask: 0,
            list: SparseBlobDeque::from_component(component, size),
            last_tick: 0,
        }
    }

    pub fn from_type<T: Clone + PartialEq>(size: NonZero<u8>) -> Self {
        Self {
            removed_mask: 0,
            list: SparseBlobDeque::from_type::<T>(size),
            last_tick: 0,
        }
    }

    #[expect(
        clippy::len_without_is_empty,
        reason = "no consumer needs is_empty; adding it would be untested dead code"
    )]
    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn stored_items(&self) -> usize {
        self.list.stored_items()
    }

    pub fn first_tick(&self) -> u32 {
        self.last_tick.saturating_sub(
            63u32.saturating_sub((self.removed_mask | self.list.mask()).leading_zeros()),
        )
    }

    pub fn get<'a>(&'a self, tick: u32) -> TickData<Ptr<'a>> {
        if tick > self.last_tick {
            return TickData::Missing;
        }
        let ago = (self.last_tick - tick) as usize;
        if ago >= self.len() {
            return TickData::Missing;
        }
        let index = self.len() - 1 - ago;
        let index_bit = 1 << ago as u64;
        if self.removed_mask & index_bit != 0 {
            return TickData::Removed;
        }

        match self.list.get(index) {
            Some(ptr) => TickData::Value(ptr),
            None => TickData::Missing,
        }
    }

    pub fn get_latest<'a>(&'a self, tick: u32) -> TickData<Ptr<'a>> {
        let ago = self.last_tick.saturating_sub(tick) as usize;
        if ago >= self.len() {
            return TickData::Missing;
        }

        let search_mask = !((1 << ago as u64) - 1);
        let removed_ago = (self.removed_mask & search_mask).trailing_zeros();
        let item_ago = (self.list.mask() & search_mask).trailing_zeros();
        let len = self.list.len() as u32;
        if removed_ago > len && item_ago > len {
            return TickData::Missing;
        }
        if removed_ago <= item_ago {
            return TickData::Removed;
        }

        let index = self.len() - 1 - item_ago as usize;

        // SAFETY: reaching here means `item_ago` indexes a set bit in
        // `self.list.mask()`, so slot `index` is occupied and `get` is `Some`.
        TickData::Value(unsafe { self.list.get(index).unwrap_unchecked() })
    }

    pub fn empty_after(&self, tick: u32) -> u32 {
        if self.list.is_empty() {
            return 0;
        }
        if tick >= self.last_tick {
            return 64;
        }

        let ago = ((self.last_tick - tick) as usize).min(self.len().saturating_sub(1));
        let search_mask = (1 << (ago as u64)) - 1;

        let empty = (self.list.mask() | self.removed_mask) & search_mask;
        empty.leading_zeros() - (64u32.saturating_sub(ago as u32))
    }

    /// # Safety
    /// - The value written in `write_fn` MUST match the type this history was made for
    /// - `write_fn` MUST write to the [`PtrMut`], or the value will be uninitialized
    pub unsafe fn write(&mut self, tick: u32, write_fn: impl FnOnce(PtrMut)) {
        let mut write_fn = Some(write_fn);
        // SAFETY: `write_dyn` invokes the closure at most once, so `take()` always
        // yields `Some` here.
        unsafe { self.write_dyn(tick, &mut |ptr| (write_fn.take().unwrap_unchecked())(ptr)) }
    }

    /// # Safety
    /// Same contract as [`write`](Self::write): `write_fn` must write a value of
    /// this history's type to the [`PtrMut`].
    unsafe fn write_dyn(&mut self, tick: u32, write_fn: &mut dyn FnMut(PtrMut)) {
        self.fill_gaps(tick);

        if !self.list.is_empty() && tick <= self.last_tick {
            let ago = (self.last_tick - tick) as usize;
            if ago >= self.list.capacity() {
                return;
            }
            if ago >= self.list.len() {
                self.list.extend_front(ago - (self.list.len() - 1));
            }

            let index = self.len() - 1 - ago;
            unsafe { self.list.replace(index, &mut *write_fn) };
            return;
        }

        if self.list.capacity() == self.list.len() {
            self.trim_front();
        }

        self.removed_mask = self.removed_mask.wrapping_shl(1);
        unsafe { self.list.append(Some(&mut *write_fn)) }
        self.last_tick = tick;
    }

    pub fn mark_removed(&mut self, tick: u32) {
        if !self.list.is_empty() && tick <= self.last_tick {
            let ago = (self.last_tick - tick) as usize;
            if ago >= self.list.capacity() {
                return;
            }
            if ago >= self.list.len() {
                self.list.extend_front(ago - (self.list.len() - 1));
            }

            self.removed_mask |= 1 << ago;

            return;
        }

        self.fill_gaps(tick);

        if self.list.capacity() == self.list.len() {
            self.trim_front();
        }

        self.removed_mask = self.removed_mask.wrapping_shl(1) | 1;
        unsafe { self.list.append(None::<fn(PtrMut)>) };
        self.last_tick = tick;
    }

    fn fill_gaps(&mut self, tick: u32) {
        if self.list.is_empty() || tick <= self.last_tick + 1 {
            return;
        }

        let gap = tick - 1 - self.last_tick;

        if gap as usize >= self.list.capacity() {
            if self.list.stored_items() == 0 && self.removed_mask == 0 {
                self.list
                    .extend_back((gap as usize).min(self.list.capacity()));
                self.last_tick += gap;
                return;
            }

            let newest_item = self.list.mask().trailing_zeros();
            let newest_remove = self.removed_mask.trailing_zeros();
            let newest_bit = newest_item.min(newest_remove);
            if newest_bit != 0 {
                let bits_to_swap = (1 << newest_bit) | 1;

                if newest_item < newest_remove {
                    *self.list.mask_mut() ^= bits_to_swap;
                } else {
                    self.removed_mask = 1;
                }
            }

            let cap_mask = if self.list.capacity() < 64 {
                (1 << self.list.capacity()) - 1
            } else {
                u64::MAX
            };
            let n = self.list.capacity() - 1;
            self.list.extend_back(n);
            self.removed_mask = self.removed_mask.wrapping_shl(n as u32) & cap_mask;

            self.last_tick += gap;
            return;
        }

        if self.list.len() + gap as usize > self.list.capacity() {
            let new_first = self.list.len() + gap as usize - self.list.capacity();
            let retained = self.list.len() - new_first;
            let search_mask = 1 << (retained - 1);
            let has_value =
                (self.removed_mask & search_mask) | (self.list.mask() & search_mask) != 0;

            if !has_value {
                let item_ago = (self.list.mask().wrapping_shr(retained as u32)).trailing_zeros();
                let removed_ago =
                    (self.removed_mask.wrapping_shr(retained as u32)).trailing_zeros();
                if item_ago < 64 || removed_ago < 64 {
                    let to_move = item_ago.min(removed_ago) + 1;
                    let bits_to_swap = 1 << (retained - 1) | 1 << (retained - 1 + to_move as usize);

                    if item_ago < removed_ago {
                        *self.list.mask_mut() ^= bits_to_swap;
                    } else {
                        self.removed_mask ^= bits_to_swap;
                    }
                }
            }
        }

        self.removed_mask = self.removed_mask.wrapping_shl(gap);
        self.list.extend_back(gap as usize);
        self.last_tick += gap;
    }

    fn trim_front(&mut self) {
        let search_mask = 1 << (self.list.len() - 2);
        let has_value = (self.removed_mask & search_mask) | (self.list.mask() & search_mask) != 0;

        if !has_value {
            let retained = self.list.len() - 1;
            let bits_to_swap = 0b11 << (retained - 1);
            if self.list.mask() & (search_mask << 1) != 0 {
                *self.list.mask_mut() ^= bits_to_swap;
            } else if self.removed_mask & (search_mask << 1) != 0 {
                self.removed_mask ^= bits_to_swap;
            }
        }
    }

    pub fn clean(&mut self, retain_until: u32) {
        if retain_until >= self.last_tick {
            return;
        }

        let to_drop = self.last_tick - retain_until;
        if to_drop >= self.len() as u32 {
            self.list.clear();
            self.last_tick = retain_until;
            return;
        }
        self.removed_mask = self.removed_mask.wrapping_shr(to_drop);
        self.list.trim_back(to_drop as usize);
        self.last_tick -= to_drop;
    }

    pub fn keep_first_item(&mut self) {
        if self.list.stored_items() == 0 {
            return;
        }

        let zeros = self.list.mask().leading_zeros();
        let ago = 63 - zeros;
        self.clean(self.last_tick.saturating_sub(ago));
    }
}
