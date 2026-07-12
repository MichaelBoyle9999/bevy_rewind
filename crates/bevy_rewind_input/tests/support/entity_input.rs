use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_rewind_input::InputTrait;
use serde::{Deserialize, Serialize};

#[derive(Component, Clone, Serialize, Deserialize, Debug, PartialEq, TypePath)]
pub struct E(pub Entity);

impl Default for E {
    fn default() -> Self {
        Self(Entity::PLACEHOLDER)
    }
}

impl InputTrait for E {
    fn repeats() -> bool {
        true
    }
}

impl MapEntities for E {
    fn map_entities<M: EntityMapper>(&mut self, mapper: &mut M) {
        self.0 = mapper.get_mapped(self.0);
    }
}
