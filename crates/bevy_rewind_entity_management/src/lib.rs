//! A crate handling entity management in a way that plays nice with rollback.
//!
//! The plugin is symmetric across client and server (both sides run rollbacks,
//! so both need the spawn-rollback machinery). The despawn flow that converts a
//! server-replicated despawn into a `Despawned`-then-disabled retention is also
//! shared and gated at runtime on `world_has_authority` rather than at compile
//! time, so the same plugin compiles and runs on either side.

mod client;

pub use client::{Despawned, EntityManagementPlugin, Unspawned};

use std::marker::PhantomData;

use bevy::{
    ecs::system::SystemParam,
    platform::collections::{HashMap, HashSet},
    prelude::*,
};
use bevy_replicon::shared::replicon_tick::RepliconTick;

/// The tick at which a [`bevy_rewind::Predicted`] entity was first spawned. Stamped
/// automatically by an observer the [`EntityManagementPlugin`] registers, reading
/// from the configured `TickSource`. The observer no-ops during a `Resimulating`
/// resim so the original spawn tick survives rollback-driven re-spawns of the same
/// entity (e.g. via `reuse_spawn` on the client). Once set, it is preserved for the
/// life of the entity.
#[derive(Component, Clone, Copy, Deref, Debug, PartialEq, Eq)]
pub struct SpawnedAt(pub RepliconTick);

/// A plugin adding handling of entity reuse for a specific [`SpawnReason`]
pub struct SpawnPlugin<Reason: SpawnReason>(PhantomData<Reason>);

impl<Reason: SpawnReason> Default for SpawnPlugin<Reason> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Reason: SpawnReason> SpawnPlugin<Reason> {
    /// Construct a `SpawnPlugin` for the specified [`SpawnReason`]
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

#[derive(Resource, Deref, DerefMut, Default)]
struct ToRemove(HashSet<Entity>);

/// A system param used to track spawned entities
#[derive(SystemParam)]
pub struct Spawned<'w, Reason: SpawnReason> {
    entities: ResMut<'w, SpawnedEntities<Reason>>,
    to_remove: Res<'w, ToRemove>,
    authority: Option<Res<'w, State<bevy_replicon::prelude::ClientState>>>,
}

#[derive(Debug)]
struct SpawnedEntity {
    id: Entity,
    last_spawned: RepliconTick,
}

#[derive(Resource, Debug)]
struct SpawnedEntities<Reason: SpawnReason>(HashMap<Reason, SpawnedEntity>);

impl<Reason: SpawnReason> Default for SpawnedEntities<Reason> {
    fn default() -> Self {
        Self(HashMap::default())
    }
}

/// A trait for spawn reasons, which are used to reuse entities during rollback
pub trait SpawnReason:
    PartialEq + Eq + std::hash::Hash + std::fmt::Debug + Sync + Send + 'static
{
    /// Get the tick for this spawn reason
    fn tick(&self) -> RepliconTick;
}

/// An extension trait for [`Commands`] for rollback-friendly entity management
pub trait EntityManagementCommands {
    /// Spawn an entity, reusing entities on client if matching
    fn reuse_spawn<Reason: SpawnReason>(
        &mut self,
        spawned: &Spawned<Reason>,
        reason: Reason,
        bundle: impl Bundle,
    ) -> Entity;

    /// Register an entity, causing later spawns to reuse this entity
    fn register_reuse<Reason: SpawnReason>(
        &mut self,
        spawned: &Spawned<Reason>,
        reason: Reason,
        entity: Entity,
    );

    /// Disable an entity if doing rollback, otherwise despawn it
    fn disable_or_despawn(&mut self, entity: Entity);
}

/// An extension trait for [`EntityWorldMut`] for rollback-friendly entity management
pub trait EntityManagementEntityWorldMut {
    /// Disable an entity if doing rollback, otherwise despawn it
    fn disable_or_despawn(self);
}

/// An extension trait for [`World`] for rollback-friendly entity management
pub trait EntityManagementWorld {
    /// Spawn an entity, reusing entities on client if matching
    fn reuse_spawn<'a, Reason: SpawnReason>(
        &'a mut self,
        spawn: Reason,
        bundle: impl Bundle,
    ) -> EntityWorldMut<'a>;

    /// Register an entity, causing later spawns to reuse this entity
    fn register_reuse<Reason: SpawnReason>(&mut self, reason: Reason, entity: Entity);

    /// Disable an entity if doing rollback, otherwise despawn it
    fn disable_or_despawn(&mut self, entity: Entity);
}

/// An extension trait for [`DeferredWorld`](bevy::ecs::world::DeferredWorld) for rollback-friendly
/// entity management
pub trait EntityManagementDeferredWorld {
    /// Register an entity, causing later spawns to reuse this entity
    fn register_reuse<Reason: SpawnReason>(&mut self, reason: Reason, entity: Entity);
}
