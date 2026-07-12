#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/comp_b.rs"]
mod comp_b;
#[path = "support/comp_c.rs"]
mod comp_c;

use comp_a::A;
use comp_c::C;

use std::mem::ManuallyDrop;

use bevy_rewind::history::batch::InsertBatch;
use bevy_rewind::history::component::HistoryComponent;

use bevy::{ecs::component::ComponentId, ecs::system::EntityCommand, prelude::*};

#[derive(Component, Clone, PartialEq, Eq, Debug)]
struct Byte(u8);

fn push_bytes(batch: &mut InsertBatch, id: ComponentId, comp: &HistoryComponent, bytes: &[u8]) {
    batch.push(id, comp, |ptr| unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr(), bytes.len());
    });
}

fn push_val<T: Clone + PartialEq>(batch: &mut InsertBatch, id: ComponentId, v: T) {
    let comp = HistoryComponent::new::<T>();
    let v = ManuallyDrop::new(v);
    let bytes = unsafe {
        std::slice::from_raw_parts((&v as *const ManuallyDrop<T>).cast::<u8>(), size_of::<T>())
    };
    push_bytes(batch, id, &comp, bytes);
}

#[test]
fn insert_minimal_archetype_moves() {
    let mut world = World::new();

    let comp_a = world.register_component::<A>();
    let comp_c = world.register_component::<C>();

    let mut batch = InsertBatch::new();
    push_val(&mut batch, comp_a, A(5));
    push_val(&mut batch, comp_c, C(12, 2));

    let e1 = world.spawn_empty().id();
    world.flush();

    let archetypes_before = world.archetypes().len();
    let e = world.entity_mut(e1);
    assert_eq!(None, e.get::<A>());
    assert_eq!(None, e.get::<C>());

    batch.apply(e);
    world.flush();

    let e = world.entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
    assert_eq!(Some(&C(12, 2)), e.get::<C>());
    let archetypes_after = world.archetypes().len();
    assert_eq!(archetypes_before + 1, archetypes_after);
}

#[test]
fn push_zero_sized_skips_data() {
    let mut world = World::new();
    let comp_b = world.register_component::<comp_b::B>();

    let mut batch = InsertBatch::new();
    assert!(batch.is_empty());

    push_val(&mut batch, comp_b, comp_b::B);
    assert!(!batch.is_empty());
}

#[test]
fn push_unaligned_component_pads() {
    let mut world = World::new();
    let comp_byte = world.register_component::<Byte>();
    let comp_a = world.register_component::<A>();

    let mut batch = InsertBatch::new();
    push_val(&mut batch, comp_byte, Byte(9));
    push_val(&mut batch, comp_a, A(7));

    let e1 = world.spawn_empty().id();
    world.flush();
    batch.apply(world.entity_mut(e1));
    world.flush();

    let e = world.entity(e1);
    assert_eq!(Some(&Byte(9)), e.get::<Byte>());
    assert_eq!(Some(&A(7)), e.get::<A>());
}
