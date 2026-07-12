use std::ptr::NonNull;

use bevy::{
    ecs::{component::ComponentId, system::EntityCommand},
    prelude::*,
    ptr::PtrMut,
};

use super::component::HistoryComponent;

#[derive(Clone, Debug)]
pub struct InsertBatch {
    ids: Vec<ComponentId>,
    offsets: Vec<usize>,
    data: Vec<u8>,
}

impl InsertBatch {
    #[expect(
        clippy::new_without_default,
        reason = "no consumer needs Default; adding it would be untested dead code"
    )]
    pub fn new() -> Self {
        Self {
            ids: Vec::with_capacity(128),
            offsets: Vec::with_capacity(128),
            data: Vec::with_capacity(2048),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn push(
        &mut self,
        comp_id: ComponentId,
        comp: &HistoryComponent,
        write_fn: impl FnOnce(PtrMut),
    ) {
        self.ids.push(comp_id);
        if comp.size() == 0 {
            return;
        }

        let align = comp.layout().align();
        let extra_offset = if self.data.len().is_multiple_of(align) {
            0
        } else {
            align - (self.data.len() % align)
        };

        let grow = comp.size() + extra_offset;
        let offset = self.data.len() + extra_offset;

        self.offsets.push(offset);
        self.data.extend((0..grow).map(|_| 0));
        write_fn(unsafe {
            PtrMut::new(NonNull::new_unchecked(
                (&mut self.data[offset..] as *mut [u8]).cast(),
            ))
        });
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.offsets.clear();
        self.data.clear();
    }
}

impl EntityCommand for InsertBatch {
    fn apply(mut self, mut entity: EntityWorldMut) {
        let iter = self.offsets.iter().map(|&offset| {
            let ptr = unsafe {
                PtrMut::new(NonNull::new_unchecked(
                    (&mut self.data[offset..] as *mut [u8]).cast(),
                ))
            };
            unsafe { ptr.promote() }
        });
        unsafe { entity.insert_by_ids(&self.ids, iter) };
    }
}

#[derive(Clone)]
pub struct RemoveBatch {
    ids: Vec<ComponentId>,
}

impl RemoveBatch {
    #[expect(
        clippy::new_without_default,
        reason = "no consumer needs Default; adding it would be untested dead code"
    )]
    pub fn new() -> Self {
        Self {
            ids: Vec::with_capacity(128),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn push(&mut self, comp_id: ComponentId) {
        self.ids.push(comp_id);
    }

    pub fn clear(&mut self) {
        self.ids.clear();
    }
}

impl EntityCommand for RemoveBatch {
    fn apply(self, mut entity: EntityWorldMut) {
        for id in self.ids {
            entity.remove_by_id(id);
        }
    }
}
