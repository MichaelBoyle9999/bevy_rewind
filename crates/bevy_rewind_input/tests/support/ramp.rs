use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_rewind_input::InputTrait;
use serde::{Deserialize, Serialize};

// An input whose repeat extrapolation depends on how many ticks have elapsed
// since it was last known. Unlike `A`, its `repeated` reads the `since` offset,
// so a test using it observes the exact offset the code under test passes in —
// which distinguishes a forward offset (`t - anchor`) from any mutated form.
#[derive(Component, Clone, Default, Serialize, Deserialize, Debug, PartialEq, TypePath)]
pub struct Ramp(pub u32);

impl InputTrait for Ramp {
    fn repeats() -> bool {
        true
    }

    fn repeated(&self, since: u32) -> Option<Self> {
        Some(Ramp(self.0 + since))
    }
}

impl MapEntities for Ramp {
    fn map_entities<M: EntityMapper>(&mut self, _: &mut M) {}
}
