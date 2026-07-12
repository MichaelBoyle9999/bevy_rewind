use super::{
    RollbackRegistry,
    component_history::{ComponentHistory, EntityHistory, TickData},
};
use crate::{RollbackFrames, RollbackSchedule, RollbackStoreSet, StoreFor, StoreScheduleLabel};

use std::num::NonZero;

use bevy::{
    ecs::{
        archetype::{ArchetypeGeneration, ArchetypeId},
        component::ComponentId,
    },
    prelude::*,
};

pub struct PredictionStorePlugin;

impl Plugin for PredictionStorePlugin {
    fn build(&self, app: &mut App) {
        let schedule = **app.world().resource::<StoreScheduleLabel>();
        app.init_resource::<ArchetypeCache>()
            .add_systems(schedule, run_store.in_set(RollbackStoreSet))
            .add_systems(
                RollbackSchedule::PreRollback,
                save_initial.in_set(RollbackStoreSet),
            );
    }
}

#[derive(Component, Deref, DerefMut, Default, Debug)]
pub struct PredictedHistory {
    #[deref]
    history: EntityHistory,
    last_archetype: Option<ArchetypeId>,
}

pub fn run_store(world: &mut World) {
    world.resource_scope::<ArchetypeCache, _>(|world, mut cache| {
        world.resource_scope::<RollbackRegistry, _>(|world, registry| {
            update_archetype_cache(world, &mut cache, &registry);

            world.resource_scope::<StoreFor, _>(|world, tick| {
                store_components(world, &cache, &registry, *tick);
            });
        });
    });
}

fn save_initial(world: &mut World) {
    world.resource_scope::<ArchetypeCache, _>(|world, mut cache| {
        world.resource_scope::<RollbackRegistry, _>(|world, registry| {
            update_archetype_cache(world, &mut cache, &registry);

            world.resource_scope::<StoreFor, _>(|world, tick| {
                store_initial(world, &cache, &registry, *tick);
            });
        });
    });
}

#[derive(Resource, Deref, DerefMut)]
pub struct ArchetypeCache {
    generation: ArchetypeGeneration,
    #[deref]
    list: Vec<ArchetypeEntry>,
    no_components: Vec<ArchetypeId>,
}

impl Default for ArchetypeCache {
    fn default() -> Self {
        Self {
            generation: ArchetypeGeneration::initial(),
            list: default(),
            no_components: default(),
        }
    }
}

pub struct ArchetypeEntry {
    id: ArchetypeId,
    predicted: Vec<(ComponentId, usize)>,
}

fn update_archetype_cache(
    world: &mut World,
    cache: &mut ArchetypeCache,
    registry: &RollbackRegistry,
) {
    let predicted_id = world.register_component::<crate::Predicted>();
    let history_id = world.register_component::<PredictedHistory>();

    for archetype in &world.archetypes()[cache.generation..] {
        if !archetype.contains(predicted_id) || !archetype.contains(history_id) {
            continue;
        }

        let mut predicted = Vec::new();

        for &component_id in archetype.components() {
            if let Some(&index) = registry.ids.get(&component_id) {
                predicted.push((component_id, index));
            }
        }

        predicted.sort_by_key(|&(id, _)| id);

        if !predicted.is_empty() {
            cache.list.push(ArchetypeEntry {
                id: archetype.id(),
                predicted,
            });
        } else {
            cache.no_components.push(archetype.id());
        }
    }

    cache.generation = world.archetypes().generation();
}

fn store_components(
    world: &mut World,
    cache: &ArchetypeCache,
    registry: &RollbackRegistry,
    tick: StoreFor,
) {
    let tick = tick.get();
    let hist_size = NonZero::new(
        world
            .get_resource::<RollbackFrames>()
            .copied()
            .unwrap()
            .history_size() as u8,
    )
    .unwrap();

    let world = world.as_unsafe_world_cell();
    let archetypes = world.archetypes();

    for &id in cache.no_components.iter() {
        for entity in archetypes
            .get(id)
            .unwrap()
            .entities()
            .iter()
            .map(|e| e.id())
        {
            let entity_mut = world.get_entity(entity).unwrap();
            // SAFETY: No structural changes here; cached archetypes all contain
            // `PredictedHistory`.
            let mut history = unsafe { entity_mut.get_mut::<PredictedHistory>() }.unwrap();

            if history.last_archetype.is_some() {
                for comp_hist in history.values_mut() {
                    if comp_hist.first_tick() >= tick {
                        continue;
                    }
                    comp_hist.mark_removed(tick);
                }
                history.last_archetype = None;
            }
        }
    }

    for entry in cache.iter() {
        for entity in archetypes
            .get(entry.id)
            .unwrap()
            .entities()
            .iter()
            .map(|e| e.id())
        {
            let entity = world.get_entity(entity).unwrap();
            // SAFETY: No structural changes here; cached archetypes all contain
            // `PredictedHistory`.
            let mut history = unsafe { entity.get_mut::<PredictedHistory>() }.unwrap();

            if let Some(last_archetype) = history.last_archetype
                && last_archetype != entry.id
            {
                for (component_id, comp_hist) in history.iter_mut() {
                    if comp_hist.first_tick() >= tick {
                        continue;
                    }
                    if !entry.predicted.iter().any(|(id, _)| id == component_id) {
                        comp_hist.mark_removed(tick);
                    }
                }
            }
            history.last_archetype = Some(entry.id);

            for &(component_id, registry_index) in entry.predicted.iter() {
                let component = &registry.components[registry_index];

                let history = history
                    .entry(component_id)
                    .or_insert_with(|| ComponentHistory::from_component(component, hist_size));
                // SAFETY: No structural changes here.
                let ptr = unsafe { entity.get_mut_by_id(component_id) }.unwrap();
                if !ptr.is_changed() {
                    continue;
                }
                if let TickData::Value(prev_ptr) = history.get_latest(tick.saturating_sub(1)) {
                    // SAFETY: history and component share the same ComponentId.
                    let equal = unsafe { component.equal(prev_ptr, ptr.as_ref()) };
                    if equal {
                        continue;
                    }
                }
                // SAFETY: history and component share the same ComponentId.
                unsafe { history.write(tick, |dst| component.store(ptr.as_ref(), dst)) };
            }
        }
    }
}

fn store_initial(
    world: &mut World,
    cache: &ArchetypeCache,
    registry: &RollbackRegistry,
    tick: StoreFor,
) {
    let tick = tick.get();
    let hist_size = NonZero::new(
        world
            .get_resource::<RollbackFrames>()
            .copied()
            .unwrap()
            .history_size() as u8,
    )
    .unwrap();

    let world = world.as_unsafe_world_cell();
    let archetypes = world.archetypes();
    // SAFETY: No structural changes here.
    let world = unsafe { world.world_mut() };

    for entry in cache.iter() {
        for entity in archetypes
            .get(entry.id)
            .unwrap()
            .entities()
            .iter()
            .map(|e| e.id())
        {
            let entity = world.as_unsafe_world_cell().get_entity(entity).unwrap();
            // SAFETY: No structural changes here; cached archetypes all contain
            // `PredictedHistory`.
            let mut history = unsafe { entity.get_mut::<PredictedHistory>() }.unwrap();

            if history.last_archetype == Some(entry.id) {
                continue;
            }

            for &(component_id, registry_index) in entry.predicted.iter() {
                if history.contains_key(&component_id) {
                    continue;
                }

                let component = &registry.components[registry_index];
                let mut comp_hist = ComponentHistory::from_component(component, hist_size);

                // SAFETY: No structural changes here.
                let ptr = unsafe { entity.get_by_id(component_id) }.unwrap();
                // SAFETY: history and component share the same ComponentId.
                unsafe { comp_hist.write(tick, |dst| component.store(ptr, dst)) };
                history.insert(component_id, comp_hist);
            }
        }
    }
}
