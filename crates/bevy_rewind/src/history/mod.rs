// Data types
pub mod blob_deque;
pub mod sparse_blob_deque;

// Shared history types
pub mod component;
pub mod component_history;

// Specific history types
pub mod authoritative;
pub use authoritative::AuthoritativeHistory;
pub mod predicted;
pub use predicted::PredictedHistory;

pub mod batch;
pub mod confirmed;
pub use confirmed::{ConfirmedInputHorizon, install_confirmed_replication_source};
pub mod load;
pub(crate) use load::{DivergenceQuery, rollback_would_change_state};

use bevy::{ecs::component::ComponentId, platform::collections::HashMap, prelude::*};
use component::HistoryComponent;

// TODO: Add some extra safeguards to check types and reduce places to duplicate them

pub struct HistoryPlugin;

impl Plugin for HistoryPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            load::HistoryLoadPlugin,
            predicted::PredictionStorePlugin,
            authoritative::AuthoriativeCleanupPlugin,
        ));
    }
}

pub(crate) use authoritative::{remove_authoritative_history, write_authoritative_history};

#[derive(Resource, Default)]
pub struct RollbackRegistry {
    pub ids: HashMap<ComponentId, usize>,
    pub components: Vec<HistoryComponent>,
}

impl RollbackRegistry {
    pub fn register<T: Component + Clone + PartialEq>(&mut self, world: &mut World) {
        let id = world.register_component::<T>();
        self.ids.insert(id, self.components.len());
        self.components.push(HistoryComponent::new::<T>());
    }

    /// Register a component whose divergence gate uses `tolerance` instead of
    /// exact equality: a confirmed-vs-predicted difference within tolerance does
    /// not warrant a rollback. History de-duplication still uses exact `PartialEq`.
    pub fn register_with_tolerance<T: Component + Clone + PartialEq>(
        &mut self,
        world: &mut World,
        tolerance: fn(&T, &T) -> bool,
    ) {
        let id = world.register_component::<T>();
        self.ids.insert(id, self.components.len());
        self.components
            .push(HistoryComponent::new::<T>().with_tolerance::<T>(tolerance));
    }
}
