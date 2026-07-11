//! Tests for the rollback plugin's schedule orchestration (`src/lib.rs`).

#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/comp_b.rs"]
mod comp_b;
#[path = "support/sim_tick.rs"]
mod sim_tick;
#[path = "support/tick_a.rs"]
mod tick_a;

use comp_a::A;
use comp_b::B;
use sim_tick::Tick;
use tick_a::a;

use std::marker::PhantomData;
use std::num::NonZero;
use std::time::Duration;

use bevy::{
    ecs::{
        component::ComponentId,
        schedule::{InternedScheduleLabel, ScheduleLabel},
    },
    prelude::*,
    state::app::StatesPlugin,
    time::{TimePlugin, TimeUpdateStrategy},
};
use bevy_replicon::client::confirm_history::{ConfirmHistory, EntityReplicated};
use bevy_replicon::client::server_mutate_ticks::{MutateTickReceived, ServerMutateTicks};
use bevy_replicon::prelude::RepliconSharedPlugin;
use bevy_replicon::shared::replicon_tick::RepliconTick;

use bevy_rewind::history::component::HistoryComponent;
use bevy_rewind::history::component_history::{ComponentHistory, TickData};
use bevy_rewind::history::{AuthoritativeHistory, PredictedHistory};
use bevy_rewind::{
    Predicted, RequestedRollback, ResourceHistory, RollbackApp, RollbackFrames, RollbackLoadSet,
    RollbackPlugin, RollbackSchedule, RollbackTarget, StoreFor, TEST_ROLLBACK_FRAMES,
};

/// Build a single-component history for `T` from tick data starting at `first_tick`.
fn comp_history<T: Component + Clone + PartialEq>(
    first_tick: u32,
    data: impl IntoIterator<Item = TickData<T>>,
) -> ComponentHistory {
    let data = data.into_iter();
    let len = data.size_hint().0;
    let mut comp_hist = ComponentHistory::from_component(&HistoryComponent::new::<T>(), unsafe {
        NonZero::new_unchecked(len.max(5) as u8)
    });
    for (offset, v) in data.enumerate() {
        let tick = first_tick + offset as u32;
        match v {
            TickData::Value(v) => unsafe { comp_hist.write(tick, |ptr| *ptr.deref_mut() = v) },
            TickData::Removed => comp_hist.mark_removed(tick),
            TickData::Missing => todo!(),
        }
    }
    comp_hist
}

fn pred_history<T: Component + Clone + PartialEq>(
    first_tick: u32,
    comp_id: ComponentId,
    data: impl IntoIterator<Item = TickData<T>>,
) -> PredictedHistory {
    let mut pred_hist = PredictedHistory::default();
    pred_hist.insert(comp_id, comp_history(first_tick, data));
    pred_hist
}

fn auth_history<T: Component + Clone + PartialEq>(
    first_tick: u32,
    comp_id: ComponentId,
    data: impl IntoIterator<Item = TickData<T>>,
) -> AuthoritativeHistory {
    let mut auth_hist = AuthoritativeHistory::default();
    auth_hist.insert(comp_id, comp_history(first_tick, data));
    auth_hist
}

fn confirm_history(confirmed: impl IntoIterator<Item = u32>) -> ConfirmHistory {
    let mut confirm = ConfirmHistory::new(RepliconTick::new(0));
    for tick in confirmed {
        confirm.confirm(RepliconTick::new(tick));
    }
    confirm
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
    .insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES))
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

/// Registering an authoritative component and driving the store-time schedule
/// exercises the `RollbackApp` registration API and the predicted-history
/// `store_initial` pass across archetype changes.
#[test]
fn registers_and_stores_predicted_component() {
    let mut app = init_app();
    app.register_authoritative_component::<A>();
    app.register_authoritative_component::<B>();

    let e1 = app.world_mut().spawn((Predicted, A(1))).id();

    app.world_mut().insert_resource(StoreFor(Tick(15).into()));
    // `run_store` (the store schedule) records `last_archetype`; without it,
    // `store_initial` never sees an unchanged archetype.
    app.world_mut().run_schedule(NoTy);
    // `save_initial` (which calls `store_initial`) runs in `PreRollback`. The
    // archetype is unchanged since the store, so it is skipped.
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);

    // Add B to change the archetype, then re-run: `store_initial` now writes the
    // new component (and skips the already-stored one).
    app.world_mut().entity_mut(e1).insert(B);
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let comp_b = world.register_component::<B>();
    let hist = world.entity(e1).get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    assert!(hist.contains_key(&comp_b));
}

/// Registering a predicted resource wires `save_initial`, which seeds the
/// history once and then leaves it alone.
#[test]
fn registers_and_seeds_predicted_resource() {
    #[derive(Resource, Clone, Debug, PartialEq)]
    struct R(u32);

    let mut app = init_app();
    app.register_predicted_resource::<R>();
    app.world_mut().insert_resource(R(7));

    app.world_mut().insert_resource(StoreFor(Tick(15).into()));
    // First pass: the history is empty, so the spawn value is seeded.
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);
    let len_after_seed = app.world().resource::<ResourceHistory<R>>().list.len();
    // Second pass: the history is non-empty, so it is left alone.
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);
    let len_after_second = app.world().resource::<ResourceHistory<R>>().list.len();

    assert_eq!(1, len_after_seed);
    assert_eq!(1, len_after_second);
}

/// A confirm message seeds a past rollback target, but the divergence gate
/// suppresses it when replaying would not change any self-predicted entity's
/// state. Also exercises the tolerance-registered divergence comparator.
#[test]
fn divergence_gate_suppresses_via_confirm_message() {
    let mut app = init_app();
    app.register_authoritative_component_with_tolerance::<A>(|a, b| a == b);

    let comp_a = app.world_mut().register_component::<A>();

    // A self-predicted entity whose prediction matches the confirmed authority at
    // every load point of the window [12, 14].
    let pred = pred_history(12, comp_a, [a(5), a(5), a(5)]);
    let auth = auth_history(12, comp_a, [a(5), a(5), a(5)]);
    let confirm = confirm_history([12, 13, 14]);
    app.world_mut()
        .spawn((Predicted, pred, auth, confirm, A(5)));

    // Several confirm messages land at past ticks; the compose loop keeps the
    // minimum, seeding a target at tick 13.
    for tick in [14u32, 13, 20] {
        app.world_mut().write_message(MutateTickReceived {
            tick: RepliconTick::new(tick),
        });
    }
    app.update();

    // The gate suppresses the rollback: no target, no requested frames.
    assert_eq!(None, **app.world().resource::<RollbackTarget>());
    assert_eq!(0, **app.world().resource::<RequestedRollback>());
}
