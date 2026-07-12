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

#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
pub struct StoreSched;

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

    // RollbackPlugin runs FixedUpdate via `run_schedule`, which panics if the
    // schedule was never registered.
    app.init_schedule(FixedUpdate);

    app.update();
    app
}
