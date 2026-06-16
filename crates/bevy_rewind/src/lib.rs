//! A crate for generic rollback handling in bevy

mod history;
pub use history::{AuthoritativeHistory, ExistingOrUninit, install_confirmed_replication_source};
use history::{LoadFn, RollbackRegistry};

mod predicted_resource;
pub use predicted_resource::ResourceHistory;

mod load;
use load::{load_and_clear_resource_prediction, reinsert_predicted_resource};

use std::{fmt::Debug, marker::PhantomData};

use bevy::{
    app::RunFixedMainLoop,
    ecs::{
        component::Mutable, intern::Interned, lifecycle::HookContext, schedule::ScheduleLabel,
        world::DeferredWorld,
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

/// The source of the current simulation tick
pub trait TickSource: Resource + Copy + From<RepliconTick> + Into<RepliconTick> {}

impl<T> TickSource for T where T: Resource + Copy + From<RepliconTick> + Into<RepliconTick> {}

/// A set in which systems storing state run
#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct RollbackStoreSet;

/// A set in which systems loading from history run
#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct RollbackLoadSet;

/// A set in which histories are added
#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub struct AddHistorySet;

/// A plugin that adds rollback logic to an app
pub struct RollbackPlugin<Tick: TickSource> {
    /// The schedule in which state is stored, all systems storing state are placed in
    /// the [`RollbackStoreSet`]. This usually runs at the end of your simulation.
    pub store_schedule: Interned<dyn ScheduleLabel>,
    /// The schedule that is executed for a rollback, this is either your simulation or a
    /// schedule that executes your simulation along with some extra stuff before and after it.
    pub rollback_schedule: Interned<dyn ScheduleLabel>,
    /// phantom nonsense
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
        // Init schedules
        .init_schedule(RollbackSchedule::PreRollback)
        .init_schedule(RollbackSchedule::Rollback)
        .init_schedule(RollbackSchedule::PostRollback)
        .init_schedule(RollbackSchedule::PreResimulation)
        .init_schedule(RollbackSchedule::PostResimulation)
        .init_schedule(RollbackSchedule::BackToPresent)
        // Since all our schedules probably won't run many systems
        // the single threaded executor should be faster
        .edit_schedule(RollbackSchedule::PreRollback, make_single_threaded)
        .edit_schedule(RollbackSchedule::Rollback, make_single_threaded)
        .edit_schedule(RollbackSchedule::PostRollback, make_single_threaded)
        .edit_schedule(RollbackSchedule::PreResimulation, make_single_threaded)
        .edit_schedule(RollbackSchedule::PostResimulation, make_single_threaded)
        .edit_schedule(RollbackSchedule::BackToPresent, make_single_threaded)
        // Configure run condition for PreResimulation on the first frame
        .configure_sets(
            RollbackSchedule::PreResimulation,
            RollbackLoadSet.run_if(not(resource_exists::<AlreadyLoaded>)),
        )
        // Init resources
        .init_resource::<RollbackRegistry>()
        .init_resource::<RollbackFrames>()
        .init_resource::<RollbackTarget>()
        .init_resource::<RequestedRollback>()
        // Store configured schedules
        .insert_resource(StoreScheduleLabel(self.store_schedule))
        .insert_resource(SimulationScheduleLabel(self.rollback_schedule))
        // Set up the history plugin
        .add_plugins(history::HistoryPlugin)
        // Set up resimulate systems
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

/// The tick to load data from
#[derive(Resource, Deref)]
pub struct LoadFrom(RepliconTick);

#[derive(Resource, Clone, Copy, Deref)]
pub(crate) struct StoreFor(RepliconTick);

fn set_store_tick<Tick: TickSource>(mut commands: Commands, tick: Option<Res<Tick>>) {
    let Some(tick) = tick else {
        panic!(
            "Tick source ({}) is required but the resource is missing",
            std::any::type_name::<Tick>(),
        );
    };
    commands.insert_resource(StoreFor((*tick).into()));
}

/// The requested number of rollback frames
#[derive(Resource, Default, Deref, DerefMut)]
pub struct RequestedRollback(i16);

fn calculate_rollback_target<Tick: TickSource>(
    mut individual_confirms: MessageReader<EntityReplicated>,
    mut global_confirms: MessageReader<MutateTickReceived>,
    tick: Res<Tick>,
    frames: ResMut<RollbackFrames>,
    mut rollback_target: ResMut<RollbackTarget>,
    mut requested_info: ResMut<RequestedRollback>,
    diverged: history::DivergenceQuery,
    registry: Res<RollbackRegistry>,
    global_confirm: Res<ServerMutateTicks>,
) {
    let tick = (*tick).into();

    // Whether an eager path (input-divergence detection in `receive_inputs`) already
    // requested a rollback this tick, captured before the confirm-message
    // composition below overwrites it. An eager target is a *proven* misprediction —
    // the just-arrived input differs from what was predicted (repeated) for that
    // tick — so it must NOT be vetoed by the state-divergence gate further down,
    // whose confirmed-state comparison lags the input by the state-replication
    // delay. Without this, a client that has the remote peer's new input in hand
    // still coasts on the stale prediction until the *state* confirm catches up,
    // overshooting a remote body's direction change; with it, the client reconciles
    // on input arrival, as promptly as the server's own (ungated) eager path does.
    // No-op on the server: its gate is already inert (`diverged.is_empty()`, no
    // inbound `AuthoritativeHistory` for bodies it owns), so the added term changes
    // nothing there — only the client, whose gate is live, is freed.
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
    // Same-tick targets are dropped — we're already at this tick, nothing to replay.
    if target == tick {
        return;
    }

    // Divergence gate (past targets only). A confirm landing at a past tick only
    // warrants a rollback if replaying from it would actually change the present
    // state — i.e. some self-predicted entity's confirmed authoritative value at a
    // resim load point differs from what it predicted there. At zero latency the
    // global confirm channel lands one tick behind every tick (the server ticks
    // first in the loopback exchange), which previously fired a depth-1 rollback
    // every tick even though the prediction was perfect. Gating on real divergence
    // makes that chronic rollback disappear, while still firing exactly one
    // corrective rollback when the prediction is genuinely wrong — e.g. the
    // spawn-collapse position handoff onto a client's own body. Every `Predicted`
    // body (local-input or remote-replayed) is delivered by this one rollback path.
    // See `game/tests/client_movement_visibility.rs`.
    //
    // Future targets (target > tick) are NOT gated: they fast-forward in
    // `trigger_rollback` to load just-arrived authoritative state stamped ahead of
    // the local tick, which has no predicted counterpart to compare against.
    //
    // NOTE: divergence is decided by `RollbackRegistry`'s `equal`, which is
    // `PartialEq` — bit-exact for floating-point components. This is a deliberately
    // weak placeholder: it is correct only while the client's prediction is
    // bit-identical to the server's authoritative simulation (true for the
    // same-binary deterministic tests; NOT guaranteed across machines, where
    // floating-point drift would resurrect chronic rollback). Replace with a
    // per-component tolerance (epsilon for continuous physics state, exact for
    // discrete state) before relying on this under real cross-machine play.
    //
    // The gate only suppresses when there are self-predicted entities to
    // evaluate. With none present (e.g. machinery tests that inject a target
    // directly to exercise the rollback runtime), we cannot conclude the
    // rollback is a no-op, so we fall through and fire as before. The game
    // always has at least the local player's body, so the gate is always live
    // there.
    if target.get() < tick.get()
        && eager_in.is_none()
        && !diverged.is_empty()
        && !history::rollback_would_change_state(
            &diverged,
            &registry,
            &global_confirm,
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

/// A resource only present if data was already loaded for a given resimulation
#[derive(Resource)]
pub struct AlreadyLoaded;

/// Present only while [`trigger_rollback`] is replaying past ticks (i.e. while
/// the user's simulation schedule is running as a *resim*, not a forward step).
/// Systems that distinguish "load historical input" (resim) from "use live
/// input" (forward) can gate on `Option<Res<Resimulating>>`. Inserted just
/// before each resim tick's `PreResimulation`/schedule/`PostResimulation`
/// triple and removed immediately after, so neither the initial `Rollback`
/// state-load nor `BackToPresent` is included.
#[derive(Resource)]
pub struct Resimulating;

fn trigger_rollback<Tick: TickSource>(world: &mut World) {
    let schedule = **world.resource::<SimulationScheduleLabel>();

    // Swap to Time<Fixed>
    *world.resource_mut::<Time>() = world.resource::<Time<Fixed>>().as_generic();

    let real_tick: RepliconTick = (*world.resource::<Tick>()).into();
    // Read but do not yet clear the target so `PreRollback` systems can inspect it
    // — e.g. `bevy_rewind_entity_management::disable_unspawned_during_rollback`
    // needs the rollback target to mark `Predicted` entities spawned after that
    // tick as `Unspawned` before `Rollback`'s `load_and_clear_prediction` runs.
    // `rollback_requested` is the gate; this resolves to `Some` by construction.
    let start = (**world.resource::<RollbackTarget>())
        .expect("rollback_requested gates trigger_rollback; target must be Some");

    world.run_schedule(RollbackSchedule::PreRollback);

    // Clear the request now that PreRollback has had its chance to read it.
    **world.resource_mut::<RollbackTarget>() = None;

    world.insert_resource(LoadFrom(RepliconTick::new(start.get().saturating_sub(1))));
    world.insert_resource(Tick::from(start));
    world.run_schedule(RollbackSchedule::Rollback);
    world.run_schedule(RollbackSchedule::PostRollback);

    // A future target (start > real_tick) loads just-arrived authoritative state
    // and runs no resim ticks (the loop below is empty). It must NOT touch
    // `Time<Fixed>`: the tick source is re-derived from the confirmed-tick
    // estimate every `FixedPreUpdate`, so the present is owned by that clock, and
    // discarding fixed-time overstep here (as this branch once did) permanently
    // stole wall time from the simulation. On a client running behind the host —
    // every real connection, via the stale connect seed — fresh confirms land
    // ahead of the present every frame, so the discard fired repeatedly, starving
    // fixed steps and compounding into runaway clock drift (input lateness,
    // rendered delay, and overshoot of remote bodies). See
    // `game/tests/clock_drift.rs`.

    // The first resimulated frame should be marked as already loaded
    world.insert_resource(AlreadyLoaded);

    for tick in start.get()..=real_tick.get() {
        // Set the correct ticks
        let tick = RepliconTick::new(tick);
        world.insert_resource(LoadFrom(RepliconTick::new(tick.get().saturating_sub(1))));
        world.insert_resource(Tick::from(tick));

        // Mark resim active so input/state systems can distinguish "replay from
        // history" from "drive from live input". Scoped to the
        // PreResim → schedule → PostResim triple per tick.
        world.insert_resource(Resimulating);

        // Run PreResimulation
        world.run_schedule(RollbackSchedule::PreResimulation);
        world.remove_resource::<AlreadyLoaded>();

        // Run the simulation schedule defined by the user
        world.run_schedule(schedule);

        // Run PostResimulation
        world.run_schedule(RollbackSchedule::PostResimulation);

        world.remove_resource::<Resimulating>();
    }

    world.run_schedule(RollbackSchedule::BackToPresent);

    // Swap back to Time<Virtual>
    *world.resource_mut::<Time>() = world.resource::<Time<Virtual>>().as_generic();
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bevy::{
        ecs::schedule::InternedScheduleLabel,
        prelude::*,
        state::app::StatesPlugin,
        time::{TimePlugin, TimeUpdateStrategy},
    };
    use bevy_replicon::client::server_mutate_ticks::{MutateTickReceived, ServerMutateTicks};

    use crate::*;

    #[derive(Resource, Clone, Copy, Deref, DerefMut, PartialEq, Eq, Debug, Default)]
    pub struct Tick(pub u32);

    impl From<RepliconTick> for Tick {
        fn from(value: RepliconTick) -> Self {
            Self(value.get())
        }
    }

    impl From<Tick> for RepliconTick {
        fn from(value: Tick) -> Self {
            RepliconTick::new(value.0)
        }
    }

    #[derive(Resource, Deref, DerefMut, Default)]
    struct Runs(Vec<Tick>);

    #[derive(Resource, Deref, DerefMut, Default)]
    struct Deltas(Vec<u32>);

    #[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
    struct NoTy;

    fn init_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            StatesPlugin,
            RepliconSharedPlugin::default(),
            RollbackPlugin::<Tick> {
                store_schedule: NoTy.intern(),
                rollback_schedule: FixedUpdate.intern(),
                phantom: PhantomData,
            },
            TimePlugin,
        ))
        .init_resource::<ServerMutateTicks>()
        .add_message::<EntityReplicated>()
        .add_message::<MutateTickReceived>()
        .insert_resource(TimeUpdateStrategy::ManualDuration(Duration::from_millis(
            16,
        )))
        .insert_resource(Tick(15))
        .init_resource::<Runs>()
        .init_resource::<Deltas>();

        app.add_systems(
            FixedUpdate,
            (
                |mut runs: ResMut<Runs>, tick: Res<Tick>| runs.push(*tick),
                |mut deltas: ResMut<Deltas>, time: Res<Time>| {
                    deltas.push(time.delta().as_micros() as u32);
                },
            ),
        );

        // The first update doesn't advance time
        app.update();

        app
    }

    #[test]
    fn rollback_order() {
        let mut app = init_app();
        assert_eq!(*app.world().resource::<Tick>(), Tick(15));

        #[derive(Resource, Deref, DerefMut, Default)]
        struct Schedules(Vec<InternedScheduleLabel>);

        use RollbackSchedule::*;

        app.add_systems(PreRollback, |mut schedules: ResMut<Schedules>| {
            schedules.push(PreRollback.intern());
        })
        .add_systems(Rollback, |mut schedules: ResMut<Schedules>| {
            schedules.push(Rollback.intern())
        })
        .add_systems(PostRollback, |mut schedules: ResMut<Schedules>| {
            schedules.push(PostRollback.intern());
        })
        .add_systems(PreResimulation, |mut schedules: ResMut<Schedules>| {
            schedules.push(PreResimulation.intern());
        })
        .add_systems(PostResimulation, |mut schedules: ResMut<Schedules>| {
            schedules.push(PostResimulation.intern());
        })
        .add_systems(BackToPresent, |mut schedules: ResMut<Schedules>| {
            schedules.push(BackToPresent.intern());
        })
        .add_systems(FixedUpdate, |mut schedules: ResMut<Schedules>| {
            schedules.push(FixedUpdate.intern());
        })
        .init_resource::<Schedules>();

        // Set a rollback target
        **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(14).into());
        app.update();

        // We ran 2 rollback frames, all the expected schedules should've ran in the right order
        assert_eq!(
            **app.world().resource::<Schedules>(),
            [
                // The rollback to tick 14
                PreRollback.intern(),
                Rollback.intern(),
                PostRollback.intern(),
                // Resimulation of tick 14
                PreResimulation.intern(),
                FixedUpdate.intern(),
                PostResimulation.intern(),
                // Resimulation of tick 15
                PreResimulation.intern(),
                FixedUpdate.intern(),
                PostResimulation.intern(),
                // Back to present
                BackToPresent.intern(),
                // The regular fixed update
                FixedUpdate.intern()
            ]
        );
    }

    #[test]
    fn rollback_uses_fixed_deltas() {
        let mut app = init_app();
        assert_eq!(*app.world().resource::<Tick>(), Tick(15));

        app.update();

        assert_eq!(
            app.world().resource::<Time<()>>().delta().as_micros(),
            16000
        );
        assert_eq!(**app.world().resource::<Runs>(), [Tick(15)]);
        assert_eq!(**app.world().resource::<Deltas>(), [15625]);

        // Set a rollback target
        **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(14).into());
        app.update();

        // We ran 2 rollback frames, all executed deltas should match Time<Fixed>, not Time<Virtual>
        assert_eq!(
            app.world().resource::<Time<()>>().delta().as_micros(),
            16000
        );
        assert_eq!(
            **app.world().resource::<Runs>(),
            [Tick(15), Tick(14), Tick(15), Tick(15)]
        );
        assert_eq!(
            **app.world().resource::<Deltas>(),
            [15625, 15625, 15625, 15625]
        );
    }

    #[test]
    fn load_new_not_on_first_frame() {
        let mut app = init_app();
        assert_eq!(*app.world().resource::<Tick>(), Tick(15));

        #[derive(Resource, Deref, DerefMut, Default)]
        struct Loads(Vec<bool>);

        app.add_systems(
            RollbackSchedule::PreResimulation,
            (
                (|mut loads: ResMut<Loads>| {
                    loads.push(false); // Append a false for the general one
                }),
                (|mut loads: ResMut<Loads>| {
                    loads.push(true); // Append a true for the load set
                })
                .in_set(RollbackLoadSet),
            )
                .chain(),
        )
        .init_resource::<Loads>();

        **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(13).into());
        app.update();

        // We ran 3 rollback frames, we expect the general load to run each time, and the load set twice
        assert_eq!(
            **app.world().resource::<Loads>(),
            [
                false, // We expect only a single general load first
                false, true, // Then both for the other frames
                false, true,
            ],
        );
    }

    /// A future rollback target fast-forwards the local tick to the target and loads auth at
    /// the latest confirmed tick. Future targets bypass `calculate_rollback_target`'s
    /// divergence gate (there is no predicted state ahead of the local tick to compare
    /// against), so just-arrived authoritative state stamped ahead of the local tick is still
    /// applied. (This test drives the target resource directly, so it exercises the
    /// fast-forward branch regardless of the gate.)
    #[test]
    fn fast_forward() {
        let mut app = init_app();
        assert_eq!(*app.world().resource::<Tick>(), Tick(15));

        app.update();

        assert_eq!(*app.world().resource::<Tick>(), Tick(15));
        assert_eq!(**app.world().resource::<Runs>(), [Tick(15)]);
        assert!(app.world().resource::<Time<Fixed>>().overstep_fraction() < 1.);

        // Set rollback target to the future
        **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(20).into());
        app.update();

        // Because the target is in the future, we fast forward and only run the newest tick
        assert_eq!(*app.world().resource::<Tick>(), Tick(20));
        assert_eq!(**app.world().resource::<Runs>(), [Tick(15), Tick(20)]);
        assert!(app.world().resource::<Time<Fixed>>().overstep_fraction() < 1.);
    }
}

/// The schedule label for the schedule in which data is stored
#[derive(Resource, Deref)]
pub struct StoreScheduleLabel(Interned<dyn ScheduleLabel>);

/// An extension trait for [`App`] adding functions to register rollback components
pub trait RollbackApp {
    /// Register a predicted-only component
    fn register_predicted_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self;
    /// Register an authoritative component
    fn register_authoritative_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self;
    /// Register an authoritative component whose divergence gate uses `tolerance`
    /// rather than exact equality: a confirmed-vs-predicted difference within
    /// tolerance is not treated as a misprediction and does not trigger a
    /// rollback. For a non-deterministic float simulation this is what keeps
    /// cross-process drift below the sim's non-determinism floor from firing a
    /// chronic, spurious rollback every tick. History de-duplication is unchanged
    /// (still exact `PartialEq`).
    fn register_authoritative_component_with_tolerance<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        tolerance: fn(&T, &T) -> bool,
    ) -> &mut Self;
    /// Register a predicted-only resource
    fn register_predicted_resource<T: Resource + Clone + Debug>(&mut self) -> &mut Self;

    /// Register a predicted-only component with a custom load function
    fn register_predicted_component_with_load<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        load_fn: LoadFn<T>,
    ) -> &mut Self;
    /// Register an authoritative component with a custom load function
    fn register_authoritative_component_with_load<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        load_fn: LoadFn<T>,
    ) -> &mut Self;
    /// Register a predicted-only resource with a custom load function
    fn register_predicted_resource_with_load<T: Resource + Clone + Debug + PartialEq>(
        &mut self,
        load_fn: LoadFn<T>,
    ) -> &mut Self;
}

impl RollbackApp for App {
    fn register_predicted_component<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
    ) -> &mut Self {
        // Register component to rollback component registry
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
            history::remove_authoritative_history::<T>,
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
            history::remove_authoritative_history::<T>,
        )
    }
    fn register_predicted_resource<T: Resource + Clone + Debug>(&mut self) -> &mut Self {
        self.world_mut().init_resource::<ResourceHistory<T>>();

        // Register store systems
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

    fn register_predicted_component_with_load<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        load_fn: LoadFn<T>,
    ) -> &mut Self {
        // Register component to rollback component registry
        let mut registry = self
            .world_mut()
            .remove_resource::<RollbackRegistry>()
            .unwrap();
        registry.register_with_load::<T>(self.world_mut(), load_fn);
        self.world_mut().insert_resource(registry);
        self
    }

    fn register_authoritative_component_with_load<
        T: Component<Mutability = Mutable> + Clone + Debug + PartialEq,
    >(
        &mut self,
        load_fn: LoadFn<T>,
    ) -> &mut Self {
        self.register_predicted_component_with_load::<T>(load_fn);

        self.set_marker_fns::<Predicted, T>(
            history::write_authoritative_history,
            history::remove_authoritative_history::<T>,
        )
    }

    fn register_predicted_resource_with_load<T: Resource + Clone + Debug + PartialEq>(
        &mut self,
        _: LoadFn<T>,
    ) -> &mut Self {
        self.world_mut().init_resource::<ResourceHistory<T>>();

        // Register store systems
        let store_schedule = **self.world().resource::<StoreScheduleLabel>();
        self.add_systems(
            RollbackSchedule::PreRollback,
            predicted_resource::save_initial::<T>.in_set(RollbackStoreSet),
        )
        .add_systems(
            store_schedule,
            predicted_resource::append_history::<T>.in_set(RollbackStoreSet),
        )
    }
}

/// A marker component for predicted entities
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

/// Data for a tick
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum TickData<T> {
    /// There is a value for the tick
    Value(T),
    /// The component/resource has been removed
    Removed,
    /// The data is missing
    Missing,
}

/// A set of schedule labels for rollbacking
#[derive(ScheduleLabel, Clone, PartialEq, Eq, Debug, Hash)]
pub enum RollbackSchedule {
    /// This schedule is executed before the tick is changed and anything is rolled back
    /// It can be used to clear state that would be hard to roll back
    PreRollback,
    /// This schedule is for systems to load the state for the new (old) tick
    Rollback,
    /// This schedule is ran after everything has been rolled back, this can be used to
    /// fix up certain state, or reapply things cleared in `PreRollback`.
    PostRollback,
    /// This schedule is called before each resimulated tick
    PreResimulation,
    /// This schedule is called after each resimulated tick
    PostResimulation,
    /// This schedule is executed when the world is back to the present
    BackToPresent,
}

/// Production default for [`RollbackFrames`]. Public so consumers can put a
/// compile-time canary against it; see `game/src/networking/components.rs`.
pub const DEFAULT_ROLLBACK_FRAMES: u8 = 15;

/// In-test default for [`RollbackFrames`], smaller to keep unit-test histories tight.
pub const TEST_ROLLBACK_FRAMES: u8 = 5;

/// A resource specifying the maximum number of rollback frames that should be stored.
/// Because the current frame is always included and we need to load data from the previous
/// frame, the history size is always 2 higher than thus number
#[derive(Resource, Clone, Copy)]
pub struct RollbackFrames(u8);

impl Default for RollbackFrames {
    fn default() -> Self {
        #[cfg(test)]
        return RollbackFrames(TEST_ROLLBACK_FRAMES);
        #[cfg(not(test))]
        return RollbackFrames(DEFAULT_ROLLBACK_FRAMES);
    }
}

impl RollbackFrames {
    /// Construct a `RollbackFrames`
    pub fn new(frames: u8) -> Self {
        if frames > 60 {
            warn!("Rollback frames cannot exceed 60 frames");
        }
        Self(frames.min(60))
    }

    /// The maximum number of rollback frames configured
    pub fn max_frames(&self) -> u8 {
        self.0
    }

    /// The size of the history necessary for the configured number of frames
    pub fn history_size(&self) -> usize {
        self.0 as usize + 2
    }
}

/// The tick to roll back to, reset to [`None`] after a rollback is triggered
#[derive(Resource, Deref, DerefMut, Default)]
pub struct RollbackTarget(Option<RepliconTick>);

fn rollback_requested(target: Res<RollbackTarget>) -> bool {
    target.is_some()
}
