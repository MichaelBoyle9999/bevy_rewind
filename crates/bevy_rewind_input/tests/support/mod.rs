use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_input::InputTrait;
use serde::{Deserialize, Serialize};

#[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug, Default)]
pub struct Tick(pub u32);

impl From<RepliconTick> for Tick {
    fn from(value: RepliconTick) -> Self {
        Self(value.get())
    }
}

impl From<Tick> for RepliconTick {
    fn from(value: Tick) -> Self {
        RepliconTick::new(value.0)
    }
}

#[derive(Component, Clone, Default, Serialize, Deserialize, Debug, PartialEq, TypePath)]
pub struct A(pub u8);

impl InputTrait for A {
    fn repeats() -> bool {
        true
    }
}

impl MapEntities for A {
    fn map_entities<M: EntityMapper>(&mut self, _: &mut M) {}
}
