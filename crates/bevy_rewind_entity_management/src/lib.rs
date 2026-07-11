//! A crate handling entity management in a way that plays nice with rollback.
//!
//! The plugin is symmetric across client and server (both sides run rollbacks,
//! so both need the spawn-rollback machinery). The despawn flow that converts a
//! server-replicated despawn into a `Despawned`-then-disabled retention is also
//! shared, so the same plugin compiles and runs on either side.

mod client;

pub use client::{Despawned, EntityManagementPlugin, Unspawned};

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

/// The tick at which a [`bevy_rewind::Predicted`] entity was first spawned. Stamped
/// automatically by an observer the [`EntityManagementPlugin`] registers, reading
/// from the configured `TickSource`. The observer no-ops during a `Resimulating`
/// resim so the original spawn tick survives rollback-driven re-spawns of the same
/// entity. Once set, it is preserved for the life of the entity.
#[derive(Component, Clone, Copy, Deref, Debug, PartialEq, Eq)]
pub struct SpawnedAt(pub RepliconTick);
