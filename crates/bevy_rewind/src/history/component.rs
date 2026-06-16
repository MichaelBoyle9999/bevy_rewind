use std::{
    alloc::Layout,
    mem::{ManuallyDrop, MaybeUninit},
    num::NonZero,
};

use bevy::{
    prelude::*,
    ptr::{OwningPtr, Ptr, PtrMut},
};

#[derive(Clone)]
pub struct HistoryComponent {
    layout: Layout,
    store: unsafe fn(Ptr, PtrMut),
    equal: unsafe fn(Ptr, Ptr) -> bool,
    tolerance: Option<ToleranceCmp>,
    call_load: CallLoad,
    load: unsafe fn(),
    drop: Option<unsafe fn(OwningPtr)>,
}

/// A per-component "are these close enough to *not* warrant a rollback?"
/// comparator, used only by the divergence gate (`history::entity_diverged`) — as
/// distinct from `equal`, which is exact `PartialEq` and stays the basis of
/// history de-duplication (`predicted.rs`). `cmp` is the user-supplied
/// `fn(&T, &T) -> bool` erased to a bare function pointer; `call` is the
/// monomorphic shim that re-types the two `Ptr`s and invokes it. Mirrors the
/// `load`/`call_load` erasure pattern.
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

pub type LoadFn<T> = fn(Option<&T>, Option<&T>, ExistingOrUninit<T>, Commands, entity: Entity);
type CallLoad =
    unsafe fn(unsafe fn(), Option<Ptr>, Option<Ptr>, ErasedExistingOrUninit, Commands, Entity);

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
    /// SAFETY: The types of `src` and `dst` point to MUST match this component's type
    pub unsafe fn store(&self, src: Ptr, dst: PtrMut) {
        unsafe {
            (self.store)(src, dst);
        }
    }

    /// Call the component's equal function
    /// SAFETY: The types of `a` and `b` point to MUST match this component's type
    pub unsafe fn equal(&self, a: Ptr, b: Ptr) -> bool {
        unsafe { (self.equal)(a, b) }
    }

    /// Whether `a` and `b` are close enough to *not* warrant a rollback: the
    /// registered tolerance comparator if one was set, otherwise exact `equal`.
    /// Used by the divergence gate so float noise below the simulation's
    /// non-determinism floor does not trigger a (spurious) correction.
    /// SAFETY: The types of `a` and `b` point to MUST match this component's type
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

    /// Call the component's load function targeting uninitialized memory
    /// SAFETY: The types of `authoritative`, `predicted`, and `dst` point to MUST match this component's type
    pub unsafe fn load_to_uninit(
        &self,
        authoritative: Option<Ptr>,
        predicted: Option<Ptr>,
        dst: PtrMut,
        commands: Commands,
        entity: Entity,
    ) {
        unsafe {
            (self.call_load)(
                self.load,
                authoritative,
                predicted,
                ErasedExistingOrUninit::Uninit(dst),
                commands,
                entity,
            );
        }
    }

    /// Call the component's load function targeting an existing value
    /// SAFETY: The types of `authoritative`, `predicted`, and `dst` point to MUST match this component's type
    // TODO:
    #[allow(dead_code)]
    pub unsafe fn load_to_component(
        &self,
        authoritative: Option<Ptr>,
        predicted: Option<Ptr>,
        dst: PtrMut,
        commands: Commands,
        entity: Entity,
    ) {
        unsafe {
            (self.call_load)(
                self.load,
                authoritative,
                predicted,
                ErasedExistingOrUninit::Existing(dst),
                commands,
                entity,
            );
        }
    }

    pub fn new<T: Clone + PartialEq>() -> Self {
        Self::new_internal::<T>(
            |_, auth: Option<Ptr>, pred, dst, _, _| unsafe {
                dst.deref::<T>()
                    .write(auth.or(pred).unwrap().deref::<T>().clone());
            },
            || {},
        )
    }

    pub fn with_load<T: Clone + PartialEq>(load_fn: LoadFn<T>) -> Self {
        Self::new_internal::<T>(
            |load, auth, pred, dst, commands, entity| {
                let load = unsafe { std::mem::transmute::<unsafe fn(), LoadFn<T>>(load) };
                (load)(
                    auth.map(|v| unsafe { v.deref::<T>() }),
                    pred.map(|v| unsafe { v.deref::<T>() }),
                    unsafe { dst.deref::<T>() },
                    commands,
                    entity,
                );
            },
            unsafe { std::mem::transmute::<LoadFn<T>, unsafe fn()>(load_fn) },
        )
    }

    fn new_internal<T: Clone + PartialEq>(call_load: CallLoad, load: unsafe fn()) -> Self {
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
            call_load,
            load,
            drop: Some(|ptr| unsafe { ptr.drop_as::<T>() }),
        }
    }
}

impl super::sparse_blob_deque::SparseBlobDeque {
    pub(super) fn from_component(component: &HistoryComponent, size: NonZero<u8>) -> Self {
        // SAFETY: We call this using a valid HistoryComponent
        unsafe { Self::new(component.layout, component.drop, size) }
    }

    pub(super) fn from_type<T: Clone + PartialEq>(size: NonZero<u8>) -> Self {
        Self::from_component(&HistoryComponent::new::<T>(), size)
    }
}

pub enum ErasedExistingOrUninit<'a> {
    // TODO:
    #[allow(dead_code)]
    Existing(PtrMut<'a>),
    Uninit(PtrMut<'a>),
}

impl<'a> ErasedExistingOrUninit<'a> {
    unsafe fn deref<T>(self) -> ExistingOrUninit<'a, T> {
        use ErasedExistingOrUninit::*;
        match self {
            Existing(v) => ExistingOrUninit::Existing(unsafe { v.deref_mut::<T>() }),
            Uninit(v) => ExistingOrUninit::Uninit(unsafe { v.deref_mut::<MaybeUninit<T>>() }),
        }
    }
}

/// An existing component, or an uninitialized pointer to one
pub enum ExistingOrUninit<'a, T> {
    /// An existing value
    Existing(&'a mut T),
    /// An uninitialized value
    Uninit(&'a mut MaybeUninit<T>),
}

impl<'a, T> ExistingOrUninit<'a, T> {
    /// Write the provided value
    pub fn write(self, t: T) {
        use ExistingOrUninit::*;
        match self {
            Existing(v) => *v = t,
            Uninit(v) => {
                v.write(t);
            }
        }
    }
}
