use std::{
    alloc::Layout,
    mem::{ManuallyDrop, MaybeUninit},
    num::NonZero,
};

use bevy::ptr::{OwningPtr, Ptr, PtrMut};

#[derive(Clone)]
pub struct HistoryComponent {
    layout: Layout,
    store: unsafe fn(Ptr, PtrMut),
    equal: unsafe fn(Ptr, Ptr) -> bool,
    tolerance: Option<ToleranceCmp>,
    load: unsafe fn(Option<Ptr>, Option<Ptr>, PtrMut),
    drop: Option<unsafe fn(OwningPtr)>,
}

#[derive(Clone)]
struct ToleranceCmp {
    cmp: unsafe fn(),
    call: unsafe fn(unsafe fn(), Ptr, Ptr) -> bool,
}

// SAFETY: `a` and `b` must point to `T`; `cmp` must be a `fn(&T, &T) -> bool`
// erased via `with_tolerance::<T>`.
unsafe fn call_tolerance<T>(cmp: unsafe fn(), a: Ptr, b: Ptr) -> bool {
    let cmp = unsafe { std::mem::transmute::<unsafe fn(), fn(&T, &T) -> bool>(cmp) };
    cmp(unsafe { a.deref::<T>() }, unsafe { b.deref::<T>() })
}

impl HistoryComponent {
    pub fn size(&self) -> usize {
        self.layout.size()
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// # Safety
    /// The types of `src` and `dst` point to MUST match this component's type
    pub unsafe fn store(&self, src: Ptr, dst: PtrMut) {
        unsafe {
            (self.store)(src, dst);
        }
    }

    /// # Safety
    /// The types of `a` and `b` point to MUST match this component's type
    pub unsafe fn equal(&self, a: Ptr, b: Ptr) -> bool {
        unsafe { (self.equal)(a, b) }
    }

    /// # Safety
    /// The types of `a` and `b` point to MUST match this component's type
    pub unsafe fn within_tolerance(&self, a: Ptr, b: Ptr) -> bool {
        match self.tolerance {
            Some(ref t) => unsafe { (t.call)(t.cmp, a, b) },
            None => unsafe { (self.equal)(a, b) },
        }
    }

    pub fn with_tolerance<T>(mut self, cmp: fn(&T, &T) -> bool) -> Self {
        self.tolerance = Some(ToleranceCmp {
            cmp: unsafe { std::mem::transmute::<fn(&T, &T) -> bool, unsafe fn()>(cmp) },
            call: call_tolerance::<T>,
        });
        self
    }

    /// # Safety
    /// - The types `authoritative`, `predicted`, and `dst` point to MUST match this component's type
    /// - At least one of `authoritative` and `predicted` MUST be `Some`
    pub unsafe fn load_to_uninit(
        &self,
        authoritative: Option<Ptr>,
        predicted: Option<Ptr>,
        dst: PtrMut,
    ) {
        unsafe {
            (self.load)(authoritative, predicted, dst);
        }
    }

    pub fn new<T: Clone + PartialEq>() -> Self {
        Self {
            layout: Layout::new::<T>(),
            store: |src, dst| {
                let value = ManuallyDrop::new(unsafe { src.deref::<T>() }.clone());
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        (&value as *const ManuallyDrop<T>).cast(),
                        dst.as_ptr(),
                        size_of::<T>(),
                    );
                }
            },
            equal: |a, b| unsafe { a.deref::<T>() == b.deref::<T>() },
            tolerance: None,
            load: |auth, pred, dst| unsafe {
                dst.deref_mut::<MaybeUninit<T>>()
                    .write(auth.or(pred).unwrap().deref::<T>().clone());
            },
            drop: Some(|ptr| unsafe { ptr.drop_as::<T>() }),
        }
    }
}

impl super::sparse_blob_deque::SparseBlobDeque {
    pub fn from_component(component: &HistoryComponent, size: NonZero<u8>) -> Self {
        // SAFETY: We call this using a valid HistoryComponent
        unsafe { Self::new(component.layout, component.drop, size) }
    }

    pub fn from_type<T: Clone + PartialEq>(size: NonZero<u8>) -> Self {
        Self::from_component(&HistoryComponent::new::<T>(), size)
    }
}
