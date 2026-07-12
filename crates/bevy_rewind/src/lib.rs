pub mod history;
use history::RollbackRegistry;
pub use history::{
    AuthoritativeHistory, ConfirmedInputHorizon, install_confirmed_replication_source,
};

pub mod predicted_resource;
pub use predicted_resource::ResourceHistory;

pub mod load;
use load::{load_and_clear_resource_prediction, reinsert_predicted_resource};

use std::{fmt::Debug, marker::PhantomData};

use bevy::{
    app::RunFixedMainLoop,
    ecs::{
        component::Mutable, intern::Interned, lifecycle::HookContext, schedule::ScheduleLabel,
        system::SystemParam, world::DeferredWorld,
    },
    prelude::*,
};
use bevy_replicon::{
    client::{
        confirm_history::EntityReplicated,
        server_mutate_ticks::{MutateTickReceived, ServerMutateTicks},
    },
    prelude::*,
    shared::{
        replication::{receive_markers::MarkerConfig, track_mutate_messages::TrackAppExt},
        replicon_tick::RepliconTick,
    },
};

pub trait TickSource: Resource + Copy + From<RepliconTick> + Into<RepliconTick> {}

impl<T> TickSource for T where T: Resource + Copy + From<RepliconTick> + Into<RepliconTick> {}

#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct RollbackStoreSet;

#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct RollbackLoadSet;

#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct AddHistorySet;

pub struct RollbackPlugin<Tick: TickSource> {
    pub store_schedule: Interned<dyn ScheduleLabel>,
    pub rollback_schedule: Interned<dyn ScheduleLabel>,
    pub phantom: PhantomData<Tick>,
}

impl<Tick: TickSource> Plugin for RollbackPlugin<Tick> {
    fn build(&self, app: &mut App) {
        fn make_single_threaded(schedule: &mut Schedule) {
            schedule.set_executor_kind(bevy::ecs::schedule::ExecutorKind::SingleThreaded);
        }

        app.register_marker_with::<Predicted>(MarkerConfig {
            priority: 100,
            need_history: true,
        })
        .track_mutate_messages()
        .init_schedule(RollbackSchedule::PreRollback)
        .init_schedule(RollbackSchedule::Rollback)
        .init_schedule(RollbackSchedule::PostRollback)
        .init_schedule(RollbackSchedule::PreResimulation)
        .init_schedule(RollbackSchedule::PostResimulation)
        .init_schedule(RollbackSchedule::BackToPresent)
        .edit_schedule(RollbackSchedule::PreRollback, make_single_threaded)
        .edit_schedule(RollbackSchedule::Rollback, make_single_threaded)
        .edit_schedule(RollbackSchedule::PostRollback, make_single_threaded)
        .edit_schedule(RollbackSchedule::PreResimulation, make_single_threaded)
        .edit_schedule(RollbackSchedule::PostResimulation, make_single_threaded)
        .edit_schedule(RollbackSchedule::BackToPresent, make_single_threaded)
        .configure_sets(
            RollbackSchedule::PreResimulation,
            RollbackLoadSet.run_if(not(resource_exists::<AlreadyLoaded>)),
        )
        .init_resource::<RollbackRegistry>()
        .init_resource::<RollbackFrames>()
        .init_resource::<RollbackTarget>()
        .init_resource::<RequestedRollback>()
        .insert_resource(StoreScheduleLabel(self.store_schedule))
        .insert_resource(SimulationScheduleLabel(self.rollback_schedule))
        .add_plugins(history::HistoryPlugin)
        .add_systems(
            self.store_schedule,
            set_store_tick::<Tick>.before(RollbackStoreSet),
        )
        .add_systems(
            RollbackSchedule::PreRollback,
            set_store_tick::<Tick>.before(RollbackStoreSet),
        )
        .add_systems(
            RunFixedMainLoop,
            (
                calculate_rollback_target::<Tick>,
                trigger_rollback::<Tick>.run_if(rollback_requested),
            )
                .chain()
                .after(RunFixedMainLoopSystems::BeforeFixedMainLoop)
                .before(RunFixedMainLoopSystems::FixedMainLoop),
        );
    }
}

#[derive(Resource, Deref)]
pub struct LoadFrom(pub RepliconTick);

#[derive(Resource, Clone, Copy, Deref)]
pub struct StoreFor(pub RepliconTick);

pub fn set_store_tick<Tick: TickSource>(mut commands: Commands, tick: Option<Res<Tick>>) {
    let Some(tick) = tick else {
        panic!(
            "Tick source ({}) is required but the resource is missing",
            std::any::type_name::<Tick>(),
        );
    };
    commands.insert_resource(StoreFor((*tick).into()));
}

#[derive(Resource, Default, Deref, DerefMut)]
pub struct RequestedRollback(i16);

#[derive(SystemParam)]
struct DivergenceGate<'w, 's> {
    diverged: history::DivergenceQuery<'w, 's>,
    registry: Res<'w, RollbackRegistry>,
    global_confirm: Res<'w, ServerMutateTicks>,
}

fn calculate_rollback_target<Tick: TickSource>(
    mut individual_confirms: MessageReader<EntityReplicated>,
    mut global_confirms: MessageReader<MutateTickReceived>,
    tick: Res<Tick>,
    frames: ResMut<RollbackFrames>,
    mut rollback_target: ResMut<RollbackTarget>,
    mut requested_info: ResMut<RequestedRollback>,
    gate: DivergenceGate,
) {
    let tick = (*tick).into();

    // An eager target (input-divergence in `receive_inputs`) is a proven
    // misprediction and must bypass the state-divergence gate below, whose
    // confirmed-state comparison lags the input by the replication delay.
    let eager_in = **rollback_target;

    for message_tick in individual_confirms
        .read()
        .map(|c| c.tick)
        .chain(global_confirms.read().map(|c| c.tick))
    {
        **rollback_target = rollback_target
            .map(|tick| {
                if tick > message_tick {
                    message_tick
                } else {
                    tick
                }
            })
            .or(Some(message_tick))
    }

    let min = tick.get().saturating_sub(frames.max_frames() as u32 - 2);
    let target = RepliconTick::new(rollback_target.unwrap_or(tick).get().max(min));

    **requested_info = (tick.get() as i64 - target.get() as i64) as i16;
    if target == tick {
        return;
    }

    // Only past targets are gated: a future target has no predicted counterpart to
    // compare, and gating past targets on real divergence avoids a chronic depth-1
    // rollback every tick at zero latency (the global confirm lands one tick late).
    if target.get() < tick.get()
        && eager_in.is_none()
        && !gate.diverged.is_empty()
        && !history::rollback_would_change_state(
            &gate.diverged,
            &gate.registry,
            &gate.global_confirm,
            target,
            tick,
        )
    {
        **rollback_target = None;
        **requested_info = 0;
        return;
    }

    **rollback_target = Some(target);
}

#[derive(Resource, Deref)]
struct SimulationScheduleLabel(Interned<dyn ScheduleLabel>);

#[derive(Resource)]
pub struct AlreadyLoaded;

#[derive(Resource)]
pub struct Resimulating;

fn trigger_rollback<Tick: TickSource>(world: &mut World) {
    let schedule = **world.resource::<SimulationScheduleLabel>();

    *world.resource_mut::<Time>() = world.resource::<Time<Fixed>>().as_generic();

    let real_tick: RepliconTick = (*world.resource::<Tick>()).into();
    // PreRollback must read the target before it is cleared: entity_management's
    // disable_unspawned_during_rollback marks entities spawned after the target.
    let start = (**world.resource::<RollbackTarget>())
        .expect("rollback_requested gates trigger_rollback; target must be Some");

    world.run_schedule(RollbackSchedule::PreRollback);

    **world.resource_mut::<RollbackTarget>() = None;

    world.insert_resource(LoadFrom(RepliconTick::new(start.get().saturating_sub(1))));
    world.insert_resource(Tick::from(start));
    world.run_schedule(RollbackSchedule::Rollback);
    world.run_schedule(RollbackSchedule::PostRollback);

    // A future target runs no resim ticks and must NOT touch Time<Fixed>:
    // discarding fixed-time overstep here steals wall time and compounds into
    // runaway clock drift. See game/tests/clock_drift.rs.
    world.insert_resource(AlreadyLoaded);

    for tick in start.get()..=real_tick.get() {
        let tick = RepliconTick::new(tick);
        world.insert_resource(LoadFrom(RepliconTick::new(tick.get().saturating_sub(1))));
        world.insert_resource(Tick::from(tick));

        world.insert_resource(Resimulating);

        world.run_schedule(RollbackSchedule::PreResimulation);
        world.remove_resource::<AlreadyLoaded>();

        world.run_schedule(schedule);

        world.run_schedule(RollbackSchedule::PostResimulation);

        world.remove_resource::<Resimulating>();
    }

    world.run_schedule(RollbackSchedule::BackToPresent);

    *world.resource_mut::<Time>() = world.resource::<Time<Virtual>>().as_generic();
}

#[derive(Resource, Deref)]
pub struct StoreScheduleLabel(Interned<dyn ScheduleLabel>);

pub trait RollbackApp {
    fn register_predicted_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self;
    fn register_authoritative_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self;
    fn register_authoritative_component_with_tolerance<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        tolerance: fn(&T, &T) -> bool,
    ) -> &mut Self;
    fn register_predicted_resource<T: Resource + Clone + Debug>(&mut self) -> &mut Self;
}

impl RollbackApp for App {
    fn register_predicted_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self {
        let mut registry = self
            .world_mut()
            .remove_resource::<RollbackRegistry>()
            .unwrap();
        registry.register::<T>(self.world_mut());
        self.world_mut().insert_resource(registry);
        self
    }
    fn register_authoritative_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self {
        self.register_predicted_component::<T>();

        self.set_marker_fns::<Predicted, T>(
            history::write_authoritative_history,
            history::remove_authoritative_history,
        )
    }
    fn register_authoritative_component_with_tolerance<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        tolerance: fn(&T, &T) -> bool,
    ) -> &mut Self {
        let mut registry = self
            .world_mut()
            .remove_resource::<RollbackRegistry>()
            .unwrap();
        registry.register_with_tolerance::<T>(self.world_mut(), tolerance);
        self.world_mut().insert_resource(registry);

        self.set_marker_fns::<Predicted, T>(
            history::write_authoritative_history,
            history::remove_authoritative_history,
        )
    }
    fn register_predicted_resource<T: Resource + Clone + Debug>(&mut self) -> &mut Self {
        self.world_mut().init_resource::<ResourceHistory<T>>();

        let store_schedule = **self.world().resource::<StoreScheduleLabel>();
        self.add_systems(
            RollbackSchedule::PreRollback,
            predicted_resource::save_initial::<T>.in_set(RollbackStoreSet),
        )
        .add_systems(
            RollbackSchedule::Rollback,
            load_and_clear_resource_prediction::<T>.in_set(RollbackLoadSet),
        )
        .add_systems(
            RollbackSchedule::PreResimulation,
            reinsert_predicted_resource::<T>.in_set(RollbackLoadSet),
        )
        .add_systems(
            store_schedule,
            predicted_resource::append_history::<T>.in_set(RollbackStoreSet),
        )
    }
}

#[derive(Component, Default)]
#[require(history::PredictedHistory, AuthoritativeHistory)]
#[component(on_remove = remove_histories)]
pub struct Predicted;

fn remove_histories(mut world: DeferredWorld, ctx: HookContext) {
    world
        .commands()
        .entity(ctx.entity)
        .try_remove::<(history::PredictedHistory, AuthoritativeHistory)>();
}

#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TickData<T> {
    Value(T),
    Removed,
    Missing,
}

#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
pub enum RollbackSchedule {
    PreRollback,
    Rollback,
    PostRollback,
    PreResimulation,
    PostResimulation,
    BackToPresent,
}

pub const DEFAULT_ROLLBACK_FRAMES: u8 = 15;

pub const TEST_ROLLBACK_FRAMES: u8 = 5;

#[derive(Resource, Clone, Copy)]
pub struct RollbackFrames(u8);

impl Default for RollbackFrames {
    fn default() -> Self {
        RollbackFrames(DEFAULT_ROLLBACK_FRAMES)
    }
}

impl RollbackFrames {
    pub fn new(frames: u8) -> Self {
        if frames > 60 {
            warn!("Rollback frames cannot exceed 60 frames");
        }
        Self(frames.min(60))
    }

    pub fn max_frames(&self) -> u8 {
        self.0
    }

    pub fn history_size(&self) -> usize {
        self.0 as usize + 2
    }
}

#[derive(Resource, Deref, DerefMut, Default)]
pub struct RollbackTarget(Option<RepliconTick>);

fn rollback_requested(target: Res<RollbackTarget>) -> bool {
    target.is_some()
}
