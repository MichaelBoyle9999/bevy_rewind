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

/// A per-component "are these close enough to *not* warrant a rollback?"
/// comparator, used only by the divergence gate (`DivergenceScan::entity_diverged`) — as
/// distinct from `equal`, which is exact `PartialEq` and stays the basis of
/// history de-duplication (`predicted.rs`). `cmp` is the user-supplied
/// `fn(&T, &T) -> bool` erased to a bare function pointer; `call` is the
/// monomorphic shim that re-types the two `Ptr`s and invokes it.
#[derive(Clone)]
struct ToleranceCmp {
    cmp: unsafe fn(),
    call: unsafe fn(unsafe fn(), Ptr, Ptr) -> bool,
}

/// Monomorphic shim re-typing two erased `Ptr`s back to `&T` and invoking the
/// erased tolerance comparator. Paired with the `T` it was registered for by
/// [`HistoryComponent::with_tolerance`].
/// SAFETY: `a` and `b` must point to values of type `T`, and `cmp` must be a
/// `fn(&T, &T) -> bool` erased via `with_tolerance::<T>`.
unsafe fn call_tolerance<T>(cmp: unsafe fn(), a: Ptr, b: Ptr) -> bool {
    let cmp = unsafe { std::mem::transmute::<unsafe fn(), fn(&T, &T) -> bool>(cmp) };
    cmp(unsafe { a.deref::<T>() }, unsafe { b.deref::<T>() })
}

impl HistoryComponent {
    /// Get the size of the component
    pub fn size(&self) -> usize {
        self.layout.size()
    }

    /// Get the layout of the component
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Call the component's store function
    ///
    /// # Safety
    /// The types of `src` and `dst` point to MUST match this component's type
    pub unsafe fn store(&self, src: Ptr, dst: PtrMut) {
        unsafe {
            (self.store)(src, dst);
        }
    }

    /// Call the component's equal function
    ///
    /// # Safety
    /// The types of `a` and `b` point to MUST match this component's type
    pub unsafe fn equal(&self, a: Ptr, b: Ptr) -> bool {
        unsafe { (self.equal)(a, b) }
    }

    /// Whether `a` and `b` are close enough to *not* warrant a rollback: the
    /// registered tolerance comparator if one was set, otherwise exact `equal`.
    /// Used by the divergence gate so float noise below the simulation's
    /// non-determinism floor does not trigger a (spurious) correction.
    ///
    /// # Safety
    /// The types of `a` and `b` point to MUST match this component's type
    pub unsafe fn within_tolerance(&self, a: Ptr, b: Ptr) -> bool {
        match self.tolerance {
            Some(ref t) => unsafe { (t.call)(t.cmp, a, b) },
            None => unsafe { (self.equal)(a, b) },
        }
    }

    /// Attach a tolerance comparator for the divergence gate. `cmp` returns `true`
    /// when the two values are close enough that replaying from the authoritative
    /// one would not meaningfully change the present — i.e. the difference is
    /// within the simulation's non-determinism floor and must not trigger a
    /// rollback. Leaves the exact `equal` (history de-dup) untouched.
    pub fn with_tolerance<T>(mut self, cmp: fn(&T, &T) -> bool) -> Self {
        self.tolerance = Some(ToleranceCmp {
            cmp: unsafe { std::mem::transmute::<fn(&T, &T) -> bool, unsafe fn()>(cmp) },
            call: call_tolerance::<T>,
        });
        self
    }

    /// Load a value into uninitialized memory: the authoritative value if
    /// present, otherwise the predicted one, cloned into `dst`.
    ///
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
                // TODO: Rethink this and the write APIs to ensure our usage is sound and doesn't leak memory
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
