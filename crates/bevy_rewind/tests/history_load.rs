#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/tick_a.rs"]
mod tick_a;
#[path = "support/ticks.rs"]
mod ticks;

use comp_a::A;
use tick_a::a;
use ticks::r_tick;

use bevy_rewind::history::component::HistoryComponent;
use bevy_rewind::history::component_history::{ComponentHistory, TickData};
use bevy_rewind::history::load::{
    self as history_load, DivergenceQuery, DivergenceScan, load_and_clear_prediction,
    load_confirmed_authoritative, rollback_would_change_state,
};
use bevy_rewind::history::{
    AuthoritativeHistory, ConfirmedInputHorizon, PredictedHistory, RollbackRegistry,
};
use bevy_rewind::{LoadFrom, Predicted};

use std::num::NonZero;

use bevy::{
    ecs::{component::ComponentId, entity_disabling::Disabled, system::ScheduleSystem},
    prelude::*,
};
use bevy_replicon::{
    client::{confirm_history::ConfirmHistory, server_mutate_ticks::ServerMutateTicks},
    shared::replicon_tick::RepliconTick,
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
            TickData::Value(v) => {
                unsafe { comp_hist.write(tick, |ptr| *ptr.deref_mut() = v) };
            }
            TickData::Removed => {
                comp_hist.mark_removed(tick);
            }
            TickData::Missing => {
                todo!();
            }
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
    for tick in confirmed.into_iter() {
        confirm.confirm(RepliconTick::new(tick));
    }
    confirm
}

fn init_app<C: Component + Clone + PartialEq, M>(
    load_from: u32,
    system: impl IntoScheduleConfigs<ScheduleSystem, M>,
) -> (App, ComponentId) {
    let mut app = App::new();
    app.add_systems(Update, system)
        .init_resource::<ServerMutateTicks>()
        .insert_resource(LoadFrom(RepliconTick::new(load_from)));

    let mut registry = RollbackRegistry::default();
    registry.register::<C>(app.world_mut());
    app.insert_resource(registry);

    let comp_id = app.world_mut().register_component::<C>();

    (app, comp_id)
}

#[test]
fn load_predicted_no_authoritative() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history(0, comp_a, [a(5)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist, A(1))).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_predicted_missing_authoritative() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_and_clear_prediction);

    let pred_hist = pred_history(1, comp_a, [a(5)]);
    let auth_hist = auth_history::<A>(0, comp_a, []);
    let confirm = confirm_history([0, 1, 2]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(0)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_predicted_unconfirmed_authoritative() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_and_clear_prediction);

    let pred_hist = pred_history(1, comp_a, [a(5)]);
    let auth_hist = auth_history(1, comp_a, [a(10), a(15)]);
    let confirm = confirm_history([0, 2]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(0)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_authoritative_direct_confirm() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, []);
    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([0]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_authoritative_direct_global_confirm() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, []);
    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
        .id();

    app.world_mut()
        .resource_mut::<ServerMutateTicks>()
        .confirm(r_tick(0), 1);

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_authoritative_future_empty_confirm() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, []);
    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([1]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_authoritative_future_empty_global_confirm() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, []);
    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
        .id();

    app.world_mut()
        .resource_mut::<ServerMutateTicks>()
        .confirm(r_tick(1), 1);

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn remove_predicted() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, [TickData::Removed]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist, A(1))).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(None, e.get::<A>());
}

#[test]
fn remove_authoritative() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history(0, comp_a, [a(2)]);
    let auth_hist = auth_history::<A>(0, comp_a, [TickData::Removed]);
    let confirm = confirm_history([0]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(None, e.get::<A>());
}

#[test]
fn insert_predicted() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history(0, comp_a, [a(5)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist)).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn insert_authoritative() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, []);
    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([0]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn change_detection() {}

#[test]
fn clears_predicted() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, [a(4), a(5), a(6)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist, A(1))).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
    assert_eq!(
        2,
        e.get::<PredictedHistory>()
            .unwrap()
            .get(&comp_a)
            .unwrap()
            .len()
    );

    app.insert_resource(LoadFrom(RepliconTick::new(0)));
    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(4)), e.get::<A>());
    assert_eq!(
        1,
        e.get::<PredictedHistory>()
            .unwrap()
            .get(&comp_a)
            .unwrap()
            .len()
    );
}

#[test]
fn retains_predicted_for_reinsert() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(2, comp_a, [a(4), a(5), a(6)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist, A(1))).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(None, e.get::<A>());
    assert_eq!(
        1,
        e.get::<PredictedHistory>()
            .unwrap()
            .get(&comp_a)
            .unwrap()
            .len()
    );

    app.insert_resource(LoadFrom(RepliconTick::new(2)));
    app.update();
    let e = app.world().entity(e1);
    assert_eq!(Some(&A(4)), e.get::<A>());
}

#[test]
fn load_predicted_authoritative_lacks_this_component() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_and_clear_prediction);

    let pred_hist = pred_history(1, comp_a, [a(5)]);
    let auth_hist = AuthoritativeHistory::default();
    let confirm = confirm_history([1]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(0)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_predicted_auth_present_but_fully_unconfirmed() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_and_clear_prediction);

    let pred_hist = pred_history(1, comp_a, [a(5)]);
    let auth_hist = auth_history(1, comp_a, [a(9)]);
    let confirm = confirm_history([]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(0)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_beyond_horizon_keeps_prediction() {
    let (mut app, comp_a) = init_app::<A, _>(2, load_and_clear_prediction);

    let pred_hist = pred_history(2, comp_a, [a(5)]);
    let auth_hist = auth_history(2, comp_a, [a(9)]);
    let confirm = confirm_history([2]);
    let horizon = ConfirmedInputHorizon(1);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, horizon, A(0)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn removed_on_disabled_entity_skips_remove() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history(0, comp_a, [a(2)]);
    let auth_hist = auth_history::<A>(0, comp_a, [TickData::Removed]);
    let confirm = confirm_history([0]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, auth_hist, confirm, A(1), Disabled))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn missing_on_disabled_entity_skips_remove() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history(2, comp_a, [a(5)]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, pred_hist, A(1), Disabled))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn skip_unpredicted() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

    let pred_hist = pred_history::<A>(0, comp_a, [a(5)]);
    let e1 = app.world_mut().spawn((pred_hist, A(1))).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn load_confirmed_authoritative_value() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_confirmed_authoritative);

    let auth_hist = auth_history(1, comp_a, [a(5)]);
    let confirm = confirm_history([1]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_confirmed_confirmed_gap() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_confirmed_authoritative);

    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([1]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_globally_confirmed_confirmed_gap() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_confirmed_authoritative);

    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, A(1)))
        .id();

    app.world_mut()
        .resource_mut::<ServerMutateTicks>()
        .confirm(r_tick(1), 1);

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn load_confirmed_skips_unconfirmed() {
    let (mut app, comp_a) = init_app::<A, _>(1, load_confirmed_authoritative);

    let auth_hist = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn load_confirmed_beyond_horizon_skips() {
    let (mut app, comp_a) = init_app::<A, _>(2, load_confirmed_authoritative);

    let auth_hist = auth_history(2, comp_a, [a(9)]);
    let confirm = confirm_history([2]);
    let horizon = ConfirmedInputHorizon(1);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, horizon, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn load_confirmed_removed() {
    let (mut app, comp_a) = init_app::<A, _>(0, load_confirmed_authoritative);

    let auth_hist = auth_history::<A>(0, comp_a, [TickData::Removed]);
    let confirm = confirm_history([0]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(None, e.get::<A>());
}

#[test]
fn load_confirmed_missing_at_load_point() {
    let (mut app, comp_a) = init_app::<A, _>(4, load_confirmed_authoritative);

    let auth_hist = auth_history(5, comp_a, [a(9)]);
    let confirm = confirm_history([4]);
    let e1 = app
        .world_mut()
        .spawn((Predicted, auth_hist, confirm, A(1)))
        .id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn reinsert_predicted() {
    let (mut app, comp_a) = init_app::<A, _>(0, history_load::reinsert_predicted);

    let pred_hist = pred_history(0, comp_a, [a(5)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist)).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(5)), e.get::<A>());
}

#[test]
fn reinsert_skips_component_already_present() {
    let (mut app, comp_a) = init_app::<A, _>(0, history_load::reinsert_predicted);

    let pred_hist = pred_history(0, comp_a, [a(5)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist, A(1))).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(Some(&A(1)), e.get::<A>());
}

#[test]
fn reinsert_skips_missing_value() {
    let (mut app, comp_a) = init_app::<A, _>(0, history_load::reinsert_predicted);

    let pred_hist = pred_history(5, comp_a, [a(5)]);
    let e1 = app.world_mut().spawn((Predicted, pred_hist)).id();

    app.update();

    let e = app.world().entity(e1);
    assert_eq!(None, e.get::<A>());
}

fn scan<'a>(
    registry: &'a RollbackRegistry,
    global_confirm: &'a ServerMutateTicks,
    lo: u32,
    hi: u32,
) -> DivergenceScan<'a> {
    DivergenceScan {
        registry,
        global_confirm,
        lo,
        hi,
    }
}

fn registry_with<C: Component + Clone + PartialEq>() -> (RollbackRegistry, ComponentId) {
    let mut world = World::new();
    let mut registry = RollbackRegistry::default();
    registry.register::<C>(&mut world);
    let comp_id = world.register_component::<C>();
    (registry, comp_id)
}

#[test]
fn entity_diverged_on_confirmed_value_mismatch() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(0, comp_a, [a(5)]);
    let auth = auth_history(0, comp_a, [a(9)]);
    let confirm = confirm_history([0]);

    assert!(scan(&registry, &global, 0, 0).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_not_diverged_when_prediction_matches() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(0, comp_a, [a(5)]);
    let auth = auth_history(0, comp_a, [a(5)]);
    let confirm = confirm_history([0]);

    assert!(!scan(&registry, &global, 0, 0).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_diverged_when_authoritative_removes_a_present_component() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(0, comp_a, [a(5)]);
    let auth = auth_history::<A>(0, comp_a, [TickData::Removed]);
    let confirm = confirm_history([0]);

    assert!(scan(&registry, &global, 0, 0).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_not_diverged_when_both_removed() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history::<A>(0, comp_a, [TickData::Removed]);
    let auth = auth_history::<A>(0, comp_a, [TickData::Removed]);
    let confirm = confirm_history([0]);

    assert!(!scan(&registry, &global, 0, 0).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_not_diverged_when_authoritative_unconfirmed() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(1, comp_a, [a(5)]);
    let auth = auth_history(1, comp_a, [a(9), a(15)]);
    let confirm = confirm_history([0, 2]);

    assert!(!scan(&registry, &global, 1, 1).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_diverged_honours_global_confirmation() {
    let (registry, comp_a) = registry_with::<A>();
    let mut global = ServerMutateTicks::default();
    global.confirm(r_tick(0), 1);

    let pred = pred_history(0, comp_a, [a(5)]);
    let auth = auth_history(0, comp_a, [a(9)]);
    let confirm = confirm_history([]);

    assert!(scan(&registry, &global, 0, 0).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_diverged_finds_divergence_later_in_range() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(0, comp_a, [a(5), a(5), a(5)]);
    let auth = auth_history(0, comp_a, [a(5), a(5), a(9)]);
    let confirm = confirm_history([0, 1, 2]);

    assert!(scan(&registry, &global, 0, 2).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_not_diverged_past_confirmed_input_horizon() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(0, comp_a, [a(5), a(5), a(5)]);
    let auth = auth_history(0, comp_a, [a(5), a(5), a(9)]);
    let confirm = confirm_history([0, 1, 2]);
    let horizon = ConfirmedInputHorizon(1);

    assert!(!scan(&registry, &global, 0, 2).entity_diverged(
        &pred,
        &auth,
        &confirm,
        Some(&horizon)
    ));
}

#[test]
fn entity_diverged_within_confirmed_input_horizon() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(0, comp_a, [a(5), a(5), a(5)]);
    let auth = auth_history(0, comp_a, [a(5), a(5), a(9)]);
    let confirm = confirm_history([0, 1, 2]);
    let horizon = ConfirmedInputHorizon(2);

    assert!(scan(&registry, &global, 0, 2).entity_diverged(&pred, &auth, &confirm, Some(&horizon)));
}

#[test]
fn entity_diverged_when_prediction_missing() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(5, comp_a, [a(5)]);
    let auth = auth_history(0, comp_a, [a(9)]);
    let confirm = confirm_history([0]);

    assert!(scan(&registry, &global, 0, 0).entity_diverged(&pred, &auth, &confirm, None));
}

#[test]
fn entity_not_diverged_when_authoritative_missing_at_load_point() {
    let (registry, comp_a) = registry_with::<A>();
    let global = ServerMutateTicks::default();

    let pred = pred_history(4, comp_a, [a(5)]);
    let auth = auth_history(5, comp_a, [a(9)]);
    let confirm = confirm_history([4]);

    assert!(!scan(&registry, &global, 4, 4).entity_diverged(&pred, &auth, &confirm, None));
}

#[derive(Resource, Default)]
struct DivergeOut(bool);

#[derive(Resource)]
struct DivergeWindow {
    start: RepliconTick,
    real: RepliconTick,
}

fn run_divergence(
    q: DivergenceQuery,
    registry: Res<RollbackRegistry>,
    global: Res<ServerMutateTicks>,
    window: Res<DivergeWindow>,
    mut out: ResMut<DivergeOut>,
) {
    out.0 = rollback_would_change_state(&q, &registry, &global, window.start, window.real);
}

fn init_diverge_app<C: Component + Clone + PartialEq>(start: u32, real: u32) -> (App, ComponentId) {
    let mut app = App::new();
    app.add_systems(Update, run_divergence)
        .init_resource::<ServerMutateTicks>()
        .init_resource::<DivergeOut>()
        .insert_resource(DivergeWindow {
            start: r_tick(start),
            real: r_tick(real),
        });

    let mut registry = RollbackRegistry::default();
    registry.register::<C>(app.world_mut());
    app.insert_resource(registry);

    let comp_id = app.world_mut().register_component::<C>();
    (app, comp_id)
}

#[test]
fn rollback_would_change_state_detects_self_predicted_divergence() {
    let (mut app, comp_a) = init_diverge_app::<A>(1, 1);

    let pred = pred_history(0, comp_a, [a(5)]);
    let auth = auth_history(0, comp_a, [a(9)]);
    let confirm = confirm_history([0]);
    app.world_mut()
        .spawn((Predicted, pred, auth, confirm, A(5)));

    app.update();

    assert!(app.world().resource::<DivergeOut>().0);
}

#[test]
fn rollback_would_change_state_false_without_entities() {
    let (mut app, _comp_a) = init_diverge_app::<A>(1, 1);

    app.update();

    assert!(!app.world().resource::<DivergeOut>().0);
}
