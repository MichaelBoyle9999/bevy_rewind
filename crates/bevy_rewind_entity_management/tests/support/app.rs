//! Shared app harness for the entity-management test suite.

use std::{marker::PhantomData, time::Duration};

use bevy::{
    ecs::schedule::ScheduleLabel,
    prelude::*,
    state::app::StatesPlugin,
    time::{TimePlugin, TimeUpdateStrategy},
};
use bevy_replicon::{
    client::server_mutate_ticks::{MutateTickReceived, ServerMutateTicks},
    prelude::*,
};
use bevy_rewind::RollbackPlugin;
use bevy_rewind_entity_management::EntityManagementPlugin;

use super::tick::TestTick;

/// The store schedule used by the harness. It never runs as part of
/// `app.update()`; run it manually via `world.run_schedule(StoreSched)` to
/// exercise store-time systems at a controlled tick.
#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
pub struct StoreSched;

/// Build an app with the rollback + entity-management plugins installed and
/// the tick source seeded at `start_tick`.
pub fn init_app(start_tick: u32) -> App {
    let mut app = App::new();
    app.add_plugins((
        StatesPlugin,
        RepliconSharedPlugin::default(),
        RollbackPlugin::<TestTick> {
            store_schedule: StoreSched.intern(),
            rollback_schedule: FixedUpdate.intern(),
            phantom: PhantomData,
        },
        EntityManagementPlugin::<TestTick>::default(),
        TimePlugin,
    ))
    .init_resource::<ServerMutateTicks>()
    .add_message::<bevy_replicon::client::confirm_history::EntityReplicated>()
    .add_message::<MutateTickReceived>()
    .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
        16,
    )))
    .insert_resource(TestTick(start_tick));

    // `RollbackPlugin` runs its simulation schedule (FixedUpdate here) via
    // `run_schedule`, which panics if the schedule was never registered. Calling
    // `add_systems` would init it lazily, but the tests don't need any FixedUpdate
    // systems — so init the empty schedule directly.
    app.init_schedule(FixedUpdate);

    // First update doesn't advance time; pump once so plugin init settles.
    app.update();
    app
}
