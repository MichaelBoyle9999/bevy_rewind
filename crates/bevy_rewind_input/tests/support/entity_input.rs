//! An entity-carrying test input, for observing `MapEntities` behaviour.

use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_rewind_input::InputTrait;
use serde::{Deserialize, Serialize};

/// A repeating input that carries an [`Entity`], so entity mapping is
/// observable (the plain `A` fixture's `map_entities` is a no-op).
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
