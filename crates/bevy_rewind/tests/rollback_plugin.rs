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

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(14).into());
    app.update();

    assert_eq!(
        **app.world().resource::<Schedules>(),
        [
            PreRollback.intern(),
            Rollback.intern(),
            PostRollback.intern(),
            PreResimulation.intern(),
            FixedUpdate.intern(),
            PostResimulation.intern(),
            PreResimulation.intern(),
            FixedUpdate.intern(),
            PostResimulation.intern(),
            BackToPresent.intern(),
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

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(14).into());
    app.update();

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
                loads.push(false);
            }),
            (|mut loads: ResMut<Loads>| {
                loads.push(true);
            })
            .in_set(RollbackLoadSet),
        )
            .chain(),
    )
    .init_resource::<Loads>();

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(13).into());
    app.update();

    assert_eq!(
        **app.world().resource::<Loads>(),
        [false, false, true, false, true],
    );
}

#[test]
fn fast_forward() {
    let mut app = init_app();
    assert_eq!(*app.world().resource::<Tick>(), Tick(15));

    app.update();

    assert_eq!(*app.world().resource::<Tick>(), Tick(15));
    assert_eq!(**app.world().resource::<Runs>(), [Tick(15)]);
    assert!(app.world().resource::<Time<Fixed>>().overstep_fraction() < 1.);

    **app.world_mut().resource_mut::<RollbackTarget>() = Some(Tick(20).into());
    app.update();

    assert_eq!(*app.world().resource::<Tick>(), Tick(20));
    assert_eq!(**app.world().resource::<Runs>(), [Tick(15), Tick(20)]);
    assert!(app.world().resource::<Time<Fixed>>().overstep_fraction() < 1.);
}

#[test]
fn registers_and_stores_predicted_component() {
    let mut app = init_app();
    app.register_authoritative_component::<A>();
    app.register_authoritative_component::<B>();

    let e1 = app.world_mut().spawn((Predicted, A(1))).id();

    app.world_mut().insert_resource(StoreFor(Tick(15).into()));
    app.world_mut().run_schedule(NoTy);
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);

    app.world_mut().entity_mut(e1).insert(B);
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let comp_b = world.register_component::<B>();
    let hist = world.entity(e1).get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    assert!(hist.contains_key(&comp_b));
}

#[test]
fn registers_and_seeds_predicted_resource() {
    #[derive(Resource, Clone, Debug, PartialEq)]
    struct R(u32);

    let mut app = init_app();
    app.register_predicted_resource::<R>();
    app.world_mut().insert_resource(R(7));

    app.world_mut().insert_resource(StoreFor(Tick(15).into()));
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);
    let len_after_seed = app.world().resource::<ResourceHistory<R>>().list.len();
    app.world_mut().run_schedule(RollbackSchedule::PreRollback);
    let len_after_second = app.world().resource::<ResourceHistory<R>>().list.len();

    assert_eq!(1, len_after_seed);
    assert_eq!(1, len_after_second);
}

#[test]
fn divergence_gate_suppresses_via_confirm_message() {
    let mut app = init_app();
    app.register_authoritative_component_with_tolerance::<A>(|a, b| a == b);

    let comp_a = app.world_mut().register_component::<A>();

    let pred = pred_history(12, comp_a, [a(5), a(5), a(5)]);
    let auth = auth_history(12, comp_a, [a(5), a(5), a(5)]);
    let confirm = confirm_history([12, 13, 14]);
    app.world_mut()
        .spawn((Predicted, pred, auth, confirm, A(5)));

    for tick in [14u32, 13, 20] {
        app.world_mut().write_message(MutateTickReceived {
            tick: RepliconTick::new(tick),
        });
    }
    app.update();

    assert_eq!(None, **app.world().resource::<RollbackTarget>());
    assert_eq!(0, **app.world().resource::<RequestedRollback>());
}
