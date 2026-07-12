mod client;

pub use client::{Despawned, EntityManagementPlugin, Unspawned};

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

#[derive(Component, Clone, Copy, Deref, Debug, PartialEq, Eq)]
pub struct SpawnedAt(pub RepliconTick);
