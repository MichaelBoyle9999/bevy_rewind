use super::{
    RollbackRegistry,
    authoritative::AuthoritativeHistory,
    batch::{InsertBatch, RemoveBatch},
    component_history::TickData,
    predicted::PredictedHistory,
};
use crate::{LoadFrom, Predicted, RemoteReplicated, RollbackLoadSet, RollbackSchedule, TickSource};

use bevy::{
    ecs::{
        archetype::Archetype,
        entity::{Entities, EntityAllocator},
        entity_disabling::Disabled,
        world::{CommandQueue, EntityMutExcept},
    },
    prelude::*,
};
use bevy_replicon::{
    client::{confirm_history::ConfirmHistory, server_mutate_ticks::ServerMutateTicks},
    shared::replicon_tick::RepliconTick,
};

pub struct HistoryLoadPlugin;

impl Plugin for HistoryLoadPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            RollbackSchedule::PreResimulation,
            (load_confirmed_authoritative, reinsert_predicted)
                .chain()
                .in_set(RollbackLoadSet),
        )
        .add_systems(RollbackSchedule::Rollback, load_and_clear_prediction);
    }
}

fn load_and_clear_prediction(
    mut commands: Commands,
    mut q: Query<
        (
            Entity,
            &mut PredictedHistory,
            Option<(&AuthoritativeHistory, &ConfirmHistory)>,
            Has<Disabled>,
        ),
        (With<Predicted>, Or<(With<Disabled>, Without<Disabled>)>),
    >,
    registry: Res<RollbackRegistry>,
    previous_tick: Res<LoadFrom>,
    global_confirm: Res<ServerMutateTicks>,
    entities: &Entities,
    e_alloc: &EntityAllocator,
) {
    let mut inserts = InsertBatch::new();
    let mut load_queue = CommandQueue::default();
    let mut removes = RemoveBatch::new();

    // TODO: Can we par_iter this?
    for (entity, mut predicted, maybe_authoritative, is_disabled) in q.iter_mut() {
        let mut load_commands = Commands::new_from_entities(&mut load_queue, e_alloc, entities);
        for (&comp_id, pred_hist) in predicted.iter_mut() {
            let &reg_idx = registry.ids.get(&comp_id).unwrap();
            let component = registry.components.get(reg_idx).unwrap();

            let auth = maybe_authoritative
                .map(|(authoritative, confirmed)| {
                    if let Some(auth_hist) = authoritative.get(&comp_id) {
                        let check_range = auth_hist.empty_after(previous_tick.get());
                        let end_tick = RepliconTick::new(previous_tick.get() + check_range);
                        if confirmed.contains_any(**previous_tick, end_tick)
                            || global_confirm.contains_any(**previous_tick, end_tick)
                        {
                            return auth_hist.get_latest(previous_tick.get());
                        }
                    }
                    TickData::Missing
                })
                .unwrap_or(TickData::Missing);

            let pred = pred_hist.get_latest(previous_tick.get());

            match (auth, pred) {
                (TickData::Removed, _) | (TickData::Missing, TickData::Removed) => {
                    if !is_disabled {
                        removes.push(comp_id);
                    }
                }
                (TickData::Missing, TickData::Missing) => {
                    if !is_disabled {
                        // We are loading a value from before the history
                        // remove the component until the history starts
                        removes.push(comp_id);
                        pred_hist.keep_first_item();
                        continue;
                    }
                }
                (auth, pred) => {
                    inserts.push(comp_id, component, |dst| unsafe {
                        component.load_to_uninit(
                            auth.value(),
                            pred.value(),
                            dst,
                            load_commands.reborrow(),
                            entity,
                        );
                    });
                }
            }

            pred_hist.clean(previous_tick.get());
        }

        if !inserts.is_empty() {
            commands.entity(entity).queue(inserts.clone());
            inserts.clear();
        }

        if !removes.is_empty() {
            commands.entity(entity).queue(removes.clone());
            removes.clear();
        }

        if !load_queue.is_empty() {
            let mut queue = std::mem::take(&mut load_queue);
            commands.queue(move |world: &mut World| queue.apply(world));
        }
    }
}

fn load_confirmed_authoritative(
    mut commands: Commands,
    mut q: Query<
        (
            EntityMutExcept<(AuthoritativeHistory, ConfirmHistory)>,
            &AuthoritativeHistory,
            &ConfirmHistory,
        ),
        (With<Predicted>, Or<(With<Disabled>, Without<Disabled>)>),
    >,
    registry: Res<RollbackRegistry>,
    previous_tick: Res<LoadFrom>,
    global_confirm: Res<ServerMutateTicks>,
    entities: &Entities,
    e_alloc: &EntityAllocator,
) {
    let mut inserts = InsertBatch::new();
    let mut load_queue = CommandQueue::default();
    let mut removes = RemoveBatch::new();

    // TODO: Can we par_iter this?
    for (entity, authoritative, confirmed) in q.iter_mut() {
        let mut load_commands = Commands::new_from_entities(&mut load_queue, e_alloc, entities);
        for (&comp_id, auth_hist) in authoritative.iter() {
            let &reg_idx = registry.ids.get(&comp_id).unwrap();
            let component = registry.components.get(reg_idx).unwrap();

            let check_range = auth_hist.empty_after(previous_tick.get());
            let end_tick = RepliconTick::new(previous_tick.get() + check_range);
            if !confirmed.contains_any(**previous_tick, end_tick)
                && !global_confirm.contains_any(**previous_tick, end_tick)
            {
                continue;
            }

            match auth_hist.get_latest(previous_tick.get()) {
                TickData::Value(value) => {
                    inserts.push(comp_id, component, |dst| unsafe {
                        component.load_to_uninit(
                            Some(value),
                            entity.get_by_id(comp_id),
                            dst,
                            load_commands.reborrow(),
                            entity.id(),
                        );
                    });
                    continue;
                }
                TickData::Removed => {
                    removes.push(comp_id);
                    continue;
                }
                TickData::Missing => {}
            }
        }

        if !inserts.is_empty() {
            commands.entity(entity.id()).queue(inserts.clone());
            inserts.clear();
        }

        if !removes.is_empty() {
            commands.entity(entity.id()).queue(removes.clone());
            removes.clear();
        }

        if !load_queue.is_empty() {
            let mut queue = std::mem::take(&mut load_queue);
            commands.queue(move |world: &mut World| queue.apply(world));
        }
    }
}

/// Eagerly copy the latest confirmed authoritative state onto every
/// [`Predicted`] entity that also carries [`RemoteReplicated`], using
/// `previous_tick = current_tick - 1` as the load point.
///
/// This is the *steady-state* delivery path for remote-replicated entities:
/// without it, the only path that lifts an [`AuthoritativeHistory`] entry
/// onto an entity's component is [`load_confirmed_authoritative`] running
/// inside the rollback resim loop, which makes chronic rollback structurally
/// load-bearing. With this system wired in user-side `FixedPreUpdate` (after
/// the tick has been advanced for the frame), remote bodies stay current
/// without any rollback firing, and the rollback runtime can be gated on
/// genuine prediction divergence.
///
/// The body mirrors [`load_confirmed_authoritative`]; the only differences
/// are the entity filter ([`With<RemoteReplicated>`]) and that the
/// previous-tick load point is derived from [`Res<Tick>`] rather than
/// [`Res<LoadFrom>`] (so it can run outside the rollback context, where
/// [`LoadFrom`] is not present).
pub fn lift_remote_replicated<Tick: TickSource>(
    mut commands: Commands,
    mut q: Query<
        (
            EntityMutExcept<(AuthoritativeHistory, ConfirmHistory)>,
            &AuthoritativeHistory,
            &ConfirmHistory,
        ),
        (
            With<Predicted>,
            With<RemoteReplicated>,
            Or<(With<Disabled>, Without<Disabled>)>,
        ),
    >,
    registry: Res<RollbackRegistry>,
    tick: Res<Tick>,
    global_confirm: Res<ServerMutateTicks>,
    entities: &Entities,
    e_alloc: &EntityAllocator,
) {
    let current: RepliconTick = (*tick).into();
    // Before the FixedUpdate that simulates `current`, the entity should hold
    // the end-of-(current-1) state, matching the rollback runtime's resim
    // load point (`LoadFrom = start - 1`).
    let previous_tick = RepliconTick::new(current.get().saturating_sub(1));

    let mut inserts = InsertBatch::new();
    let mut load_queue = CommandQueue::default();
    let mut removes = RemoveBatch::new();

    for (entity, authoritative, confirmed) in q.iter_mut() {
        let mut load_commands = Commands::new_from_entities(&mut load_queue, e_alloc, entities);
        for (&comp_id, auth_hist) in authoritative.iter() {
            let &reg_idx = registry.ids.get(&comp_id).unwrap();
            let component = registry.components.get(reg_idx).unwrap();

            let check_range = auth_hist.empty_after(previous_tick.get());
            let end_tick = RepliconTick::new(previous_tick.get() + check_range);
            if !confirmed.contains_any(previous_tick, end_tick)
                && !global_confirm.contains_any(previous_tick, end_tick)
            {
                continue;
            }

            match auth_hist.get_latest(previous_tick.get()) {
                TickData::Value(value) => {
                    inserts.push(comp_id, component, |dst| unsafe {
                        component.load_to_uninit(
                            Some(value),
                            entity.get_by_id(comp_id),
                            dst,
                            load_commands.reborrow(),
                            entity.id(),
                        );
                    });
                    continue;
                }
                TickData::Removed => {
                    removes.push(comp_id);
                    continue;
                }
                TickData::Missing => {}
            }
        }

        if !inserts.is_empty() {
            commands.entity(entity.id()).queue(inserts.clone());
            inserts.clear();
        }

        if !removes.is_empty() {
            commands.entity(entity.id()).queue(removes.clone());
            removes.clear();
        }

        if !load_queue.is_empty() {
            let mut queue = std::mem::take(&mut load_queue);
            commands.queue(move |world: &mut World| queue.apply(world));
        }
    }
}

/// Query of the histories needed to decide whether a self-predicted entity
/// mispredicted. Excludes [`RemoteReplicated`] entities: those are delivered by
/// [`lift_remote_replicated`], not by rollback, so their (lift-synced) state is
/// not what should drive the rollback trigger.
pub(crate) type DivergenceQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static PredictedHistory,
        &'static AuthoritativeHistory,
        &'static ConfirmHistory,
    ),
    (With<Predicted>, Without<RemoteReplicated>),
>;

/// Returns `true` if replaying the resim window `[start, real_tick]` would
/// actually change the present state of any self-predicted entity — i.e. some
/// confirmed authoritative value at a resim load point differs from what that
/// entity predicted there. The resim of tick `t` loads at `t - 1`, so the load
/// points span the closed range `[start - 1, real_tick - 1]`.
///
/// This is the divergence gate for `calculate_rollback_target`: a confirm that
/// merely *arrives* does not warrant a rollback unless the prediction was
/// actually wrong. Without it, the loopback exchange (server ticks first, so
/// the global confirm channel lands one tick behind every tick) fires a depth-1
/// rollback every tick even when the prediction is perfect.
///
/// The confirm gating and `get_latest` load-point lookups mirror
/// [`load_confirmed_authoritative`] exactly, so "would change state" is
/// precisely "the rollback's authoritative load would overwrite a predicted
/// value with a different one".
///
/// NOTE: equality is `RollbackRegistry`'s `equal` (`PartialEq`), which is
/// bit-exact for floating-point components. This is a deliberately weak
/// placeholder — see `calculate_rollback_target` for the full caveat.
pub(crate) fn rollback_would_change_state(
    q: &DivergenceQuery,
    registry: &RollbackRegistry,
    global_confirm: &ServerMutateTicks,
    start: RepliconTick,
    real_tick: RepliconTick,
) -> bool {
    let lo = start.get().saturating_sub(1);
    let hi = real_tick.get().saturating_sub(1);
    q.iter().any(|(pred, auth, confirm)| {
        entity_diverged(pred, auth, confirm, registry, global_confirm, lo, hi)
    })
}

/// Per-entity half of [`rollback_would_change_state`]: scans the load-point
/// range `[lo, hi]` for a confirmed authoritative value that differs from the
/// predicted value, mirroring [`load_confirmed_authoritative`]'s confirm gate
/// and `get_latest` lookups.
fn entity_diverged(
    pred: &PredictedHistory,
    auth: &AuthoritativeHistory,
    confirm: &ConfirmHistory,
    registry: &RollbackRegistry,
    global_confirm: &ServerMutateTicks,
    lo: u32,
    hi: u32,
) -> bool {
    for (&comp_id, auth_hist) in auth.iter() {
        let &reg_idx = registry.ids.get(&comp_id).unwrap();
        let component = registry.components.get(reg_idx).unwrap();
        let pred_hist = pred.get(&comp_id);

        for p in lo..=hi {
            let previous_tick = RepliconTick::new(p);
            let check_range = auth_hist.empty_after(p);
            let end_tick = RepliconTick::new(p + check_range);
            if !confirm.contains_any(previous_tick, end_tick)
                && !global_confirm.contains_any(previous_tick, end_tick)
            {
                continue;
            }

            let auth_data = auth_hist.get_latest(p);
            let pred_data = pred_hist
                .map(|h| h.get_latest(p))
                .unwrap_or(TickData::Missing);

            match auth_data {
                // No authoritative value at this load point: the rollback would
                // not load anything here, so it cannot change state.
                TickData::Missing => continue,
                // The component would be removed; that changes state only if it
                // is currently present in the prediction.
                TickData::Removed => {
                    if matches!(pred_data, TickData::Value(_)) {
                        return true;
                    }
                }
                // A value would be loaded; that changes state unless the
                // prediction already holds an equal value at this load point.
                TickData::Value(auth_value) => match pred_data {
                    TickData::Value(pred_value) => {
                        // SAFETY: both pointers come from histories registered
                        // under `comp_id`, which is what `component` describes.
                        if !unsafe { component.equal(auth_value, pred_value) } {
                            return true;
                        }
                    }
                    _ => return true,
                },
            }
        }
    }

    false
}

fn reinsert_predicted(
    mut commands: Commands,
    mut q: Query<
        (Entity, &Archetype, &PredictedHistory, &AuthoritativeHistory),
        (With<Predicted>, Or<(With<Disabled>, Without<Disabled>)>),
    >,
    registry: Res<RollbackRegistry>,
    previous_tick: Res<LoadFrom>,
    entities: &Entities,
    e_alloc: &EntityAllocator,
) {
    let mut inserts = InsertBatch::new();
    let mut load_queue = CommandQueue::default();

    // TODO: Can we par_iter this?
    for (entity, archetype, predicted, authoritative) in q.iter_mut() {
        let mut load_commands = Commands::new_from_entities(&mut load_queue, e_alloc, entities);
        for (&comp_id, pred_hist) in predicted.iter() {
            if archetype.contains(comp_id) {
                continue;
            }

            let TickData::Value(value) = pred_hist.get(previous_tick.get()) else {
                continue;
            };

            // TODO: only insert if authoritative is not known yet
            _ = authoritative;

            let &reg_idx = registry.ids.get(&comp_id).unwrap();
            let component = registry.components.get(reg_idx).unwrap();

            inserts.push(comp_id, component, |dst| unsafe {
                component.load_to_uninit(None, Some(value), dst, load_commands.reborrow(), entity);
            });
        }

        if !inserts.is_empty() {
            commands.entity(entity).queue(inserts.clone());
            inserts.clear();
        }

        if !load_queue.is_empty() {
            let mut queue = std::mem::take(&mut load_queue);
            commands.queue(move |world: &mut World| queue.apply(world));
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{LoadFrom, Predicted};

    use super::{
        super::{
            component_history::TickData, load::load_confirmed_authoritative,
            predicted::PredictedHistory, test_utils::*,
        },
        RollbackRegistry, load_and_clear_prediction,
    };
    use bevy::{
        ecs::{component::ComponentId, system::ScheduleSystem},
        prelude::*,
    };
    use bevy_replicon::{
        client::server_mutate_ticks::ServerMutateTicks, shared::replicon_tick::RepliconTick,
    };

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
        let confirm = confirm_history([0, 2]); // Only the previous and next tick are confirmed
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
        let confirm = confirm_history([0]); // The target tick is confirmed
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
        let confirm = confirm_history([]); // The tick is unconfirmed on the entity
        let e1 = app
            .world_mut()
            .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
            .id();

        // The tick is confirmed globally
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
        let confirm = confirm_history([1]); // A future empty tick is confirmed
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
        let confirm = confirm_history([]); // No ticks are confirmed on the entity
        let e1 = app
            .world_mut()
            .spawn((Predicted, pred_hist, auth_hist, confirm, A(1)))
            .id();

        // A future tick is confirmed globally
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
        let confirm = confirm_history([0]); // The target tick is confirmed
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
        let confirm = confirm_history([0]); // The target tick is confirmed
        let e1 = app
            .world_mut()
            .spawn((Predicted, pred_hist, auth_hist, confirm))
            .id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(5)), e.get::<A>());
    }

    #[test]
    fn change_detection() {
        // TODO
    }

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
        // The value should've been removed, but the first item should be retained
        assert_eq!(None, e.get::<A>());
        assert_eq!(
            1,
            e.get::<PredictedHistory>()
                .unwrap()
                .get(&comp_a)
                .unwrap()
                .len()
        );

        // We should be able to load the item again when we get back to that tick
        app.insert_resource(LoadFrom(RepliconTick::new(2)));
        app.update();
        let e = app.world().entity(e1);
        assert_eq!(Some(&A(4)), e.get::<A>());
    }

    #[test]
    fn skip_unpredicted() {
        let (mut app, comp_a) = init_app::<A, _>(0, load_and_clear_prediction);

        // Spawn an entity with the history but no Predicted, it should stay untouched
        let pred_hist = pred_history::<A>(0, comp_a, [a(5)]);
        let e1 = app.world_mut().spawn((pred_hist, A(1))).id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(1)), e.get::<A>());
    }

    ///////////////////////////////////////////////////
    /////   load_confirmed_authoritative tests   //////
    ///////////////////////////////////////////////////

    #[test]
    fn load_confirmed_authoritative_value() {
        let (mut app, comp_a) = init_app::<A, _>(1, load_confirmed_authoritative);

        let auth_hist = auth_history(1, comp_a, [a(5)]);
        let confirm = confirm_history([1]); // The target tick is confirmed
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
        let confirm = confirm_history([1]); // The target tick is confirmed
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
        let confirm = confirm_history([]); // No ticks are confirmed on the entity
        let e1 = app
            .world_mut()
            .spawn((Predicted, auth_hist, confirm, A(1)))
            .id();

        // The gap is confirmed globally
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
        let confirm = confirm_history([]); // Nothing is confirmed
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
        let (mut app, comp_a) = init_app::<A, _>(0, super::reinsert_predicted);

        let pred_hist = pred_history(0, comp_a, [a(5)]);
        let e1 = app.world_mut().spawn((Predicted, pred_hist)).id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(5)), e.get::<A>());
    }

    ///////////////////////////////////////////////////
    /////      lift_remote_replicated tests       //////
    ///////////////////////////////////////////////////

    use crate::RemoteReplicated;
    use crate::history::load::lift_remote_replicated;

    /// A test [`TickSource`] that the lift system reads.
    #[derive(Resource, Clone, Copy, Default)]
    struct Tick(u32);

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

    fn init_lift_app<C: Component + Clone + PartialEq>(tick: u32) -> (App, ComponentId) {
        let mut app = App::new();
        app.add_systems(Update, lift_remote_replicated::<Tick>)
            .init_resource::<ServerMutateTicks>()
            .insert_resource(Tick(tick));

        let mut registry = RollbackRegistry::default();
        registry.register::<C>(app.world_mut());
        app.insert_resource(registry);

        let comp_id = app.world_mut().register_component::<C>();

        (app, comp_id)
    }

    /// Steady-state delivery: a `Predicted + RemoteReplicated` entity with a
    /// confirmed authoritative value at the prior tick gets that value lifted
    /// onto the component, with no rollback involved.
    #[test]
    fn lift_writes_latest_confirmed_to_entity() {
        let (mut app, comp_a) = init_lift_app::<A>(2);

        let auth_hist = auth_history(1, comp_a, [a(5)]);
        let confirm = confirm_history([1]);
        let e1 = app
            .world_mut()
            .spawn((Predicted, RemoteReplicated, auth_hist, confirm, A(1)))
            .id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(5)), e.get::<A>());
    }

    /// Without [`RemoteReplicated`], the lift system must leave the entity
    /// alone — that is the contract that lets self-predicted entities keep
    /// their predicted component value between confirms.
    #[test]
    fn lift_skips_entities_without_remote_replicated_marker() {
        let (mut app, comp_a) = init_lift_app::<A>(2);

        let auth_hist = auth_history(1, comp_a, [a(5)]);
        let confirm = confirm_history([1]);
        let e1 = app
            .world_mut()
            .spawn((Predicted, auth_hist, confirm, A(1)))
            .id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(1)), e.get::<A>());
    }

    /// The lift must not run for entities lacking [`Predicted`] — they are
    /// not part of the rollback machinery and have no histories to read.
    #[test]
    fn lift_skips_entities_without_predicted_marker() {
        let (mut app, comp_a) = init_lift_app::<A>(2);

        let auth_hist = auth_history(1, comp_a, [a(5)]);
        let confirm = confirm_history([1]);
        let e1 = app
            .world_mut()
            .spawn((RemoteReplicated, auth_hist, confirm, A(1)))
            .id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(1)), e.get::<A>());
    }

    /// If no tick covering the auth history is confirmed (locally or
    /// globally), the lift system must not write — exactly the same gate as
    /// [`load_confirmed_authoritative`], so an unconfirmed value never leaks
    /// onto the entity ahead of its confirm.
    #[test]
    fn lift_skips_unconfirmed_history() {
        let (mut app, comp_a) = init_lift_app::<A>(2);

        let auth_hist = auth_history(1, comp_a, [a(5)]);
        let confirm = confirm_history([]);
        let e1 = app
            .world_mut()
            .spawn((Predicted, RemoteReplicated, auth_hist, confirm, A(1)))
            .id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(1)), e.get::<A>());
    }

    /// Global (`ServerMutateTicks`) confirmation is honoured in the same way
    /// as per-entity `ConfirmHistory` confirmation — mirrors
    /// `load_globally_confirmed_confirmed_gap`.
    #[test]
    fn lift_honours_global_confirmation() {
        let (mut app, comp_a) = init_lift_app::<A>(2);

        let auth_hist = auth_history(1, comp_a, [a(5)]);
        let confirm = confirm_history([]);
        let e1 = app
            .world_mut()
            .spawn((Predicted, RemoteReplicated, auth_hist, confirm, A(1)))
            .id();

        app.world_mut()
            .resource_mut::<ServerMutateTicks>()
            .confirm(r_tick(1), 1);

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(Some(&A(5)), e.get::<A>());
    }

    /// A `TickData::Removed` entry in the authoritative history at the prior
    /// tick must cause the component to be removed from the entity, so a
    /// server-side despawn-of-component propagates without needing rollback.
    #[test]
    fn lift_removes_when_authoritative_history_is_removed() {
        let (mut app, comp_a) = init_lift_app::<A>(2);

        let auth_hist = auth_history::<A>(1, comp_a, [TickData::Removed]);
        let confirm = confirm_history([1]);
        let e1 = app
            .world_mut()
            .spawn((Predicted, RemoteReplicated, auth_hist, confirm, A(1)))
            .id();

        app.update();

        let e = app.world().entity(e1);
        assert_eq!(None, e.get::<A>());
    }

    ///////////////////////////////////////////////////
    /////      divergence-gate (rollback) tests    //////
    ///////////////////////////////////////////////////

    use super::{DivergenceQuery, entity_diverged, rollback_would_change_state};

    /// Build a registry with `C` registered and return the matching `ComponentId`.
    fn registry_with<C: Component + Clone + PartialEq>() -> (RollbackRegistry, ComponentId) {
        let mut world = World::new();
        let mut registry = RollbackRegistry::default();
        registry.register::<C>(&mut world);
        let comp_id = world.register_component::<C>();
        (registry, comp_id)
    }

    /// A confirmed authoritative value that differs from the predicted value at
    /// the load point is a real divergence — the rollback would change state.
    #[test]
    fn entity_diverged_on_confirmed_value_mismatch() {
        let (registry, comp_a) = registry_with::<A>();
        let global = ServerMutateTicks::default();

        let pred = pred_history(0, comp_a, [a(5)]);
        let auth = auth_history(0, comp_a, [a(9)]);
        let confirm = confirm_history([0]);

        assert!(entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 0, 0
        ));
    }

    /// When the confirmed authoritative value equals the prediction, there is no
    /// divergence — replaying would reproduce the same state, so no rollback.
    #[test]
    fn entity_not_diverged_when_prediction_matches() {
        let (registry, comp_a) = registry_with::<A>();
        let global = ServerMutateTicks::default();

        let pred = pred_history(0, comp_a, [a(5)]);
        let auth = auth_history(0, comp_a, [a(5)]);
        let confirm = confirm_history([0]);

        assert!(!entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 0, 0
        ));
    }

    /// An authoritative `Removed` at the load point diverges only if the
    /// prediction currently holds a value there (the component would be removed).
    #[test]
    fn entity_diverged_when_authoritative_removes_a_present_component() {
        let (registry, comp_a) = registry_with::<A>();
        let global = ServerMutateTicks::default();

        let pred = pred_history(0, comp_a, [a(5)]);
        let auth = auth_history::<A>(0, comp_a, [TickData::Removed]);
        let confirm = confirm_history([0]);

        assert!(entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 0, 0
        ));
    }

    /// An authoritative `Removed` matching a prediction that is also `Removed` is
    /// not a divergence.
    #[test]
    fn entity_not_diverged_when_both_removed() {
        let (registry, comp_a) = registry_with::<A>();
        let global = ServerMutateTicks::default();

        let pred = pred_history::<A>(0, comp_a, [TickData::Removed]);
        let auth = auth_history::<A>(0, comp_a, [TickData::Removed]);
        let confirm = confirm_history([0]);

        assert!(!entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 0, 0
        ));
    }

    /// An unconfirmed authoritative value must not count as divergence — the same
    /// gate `load_confirmed_authoritative` uses to avoid loading ahead of confirm.
    #[test]
    fn entity_not_diverged_when_authoritative_unconfirmed() {
        let (registry, comp_a) = registry_with::<A>();
        let global = ServerMutateTicks::default();

        let pred = pred_history(1, comp_a, [a(5)]);
        let auth = auth_history(1, comp_a, [a(9), a(15)]);
        // Neither tick 1's own slot is confirmed (only its neighbours).
        let confirm = confirm_history([0, 2]);

        assert!(!entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 1, 1
        ));
    }

    /// Global (`ServerMutateTicks`) confirmation is honoured exactly like
    /// per-entity confirmation.
    #[test]
    fn entity_diverged_honours_global_confirmation() {
        let (registry, comp_a) = registry_with::<A>();
        let mut global = ServerMutateTicks::default();
        global.confirm(r_tick(0), 1);

        let pred = pred_history(0, comp_a, [a(5)]);
        let auth = auth_history(0, comp_a, [a(9)]);
        let confirm = confirm_history([]); // unconfirmed on the entity

        assert!(entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 0, 0
        ));
    }

    /// The scan covers the whole load-point range, not just its start: a
    /// divergence at a later tick in `[lo, hi]` is still found.
    #[test]
    fn entity_diverged_finds_divergence_later_in_range() {
        let (registry, comp_a) = registry_with::<A>();
        let global = ServerMutateTicks::default();

        // Matches at tick 0, diverges at tick 2.
        let pred = pred_history(0, comp_a, [a(5), a(5), a(5)]);
        let auth = auth_history(0, comp_a, [a(5), a(5), a(9)]);
        let confirm = confirm_history([0, 1, 2]);

        assert!(entity_diverged(
            &pred, &auth, &confirm, &registry, &global, 0, 2
        ));
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

    fn init_diverge_app<C: Component + Clone + PartialEq>(
        start: u32,
        real: u32,
    ) -> (App, ComponentId) {
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

    /// A self-predicted entity (no `RemoteReplicated`) whose confirmed
    /// authoritative value differs from its prediction makes the query report a
    /// state-changing rollback.
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

    /// `RemoteReplicated` entities are excluded from the divergence query: their
    /// state is delivered by `lift_remote_replicated`, not by rollback, so even a
    /// mismatch must not drive the rollback trigger.
    #[test]
    fn rollback_would_change_state_excludes_remote_replicated() {
        let (mut app, comp_a) = init_diverge_app::<A>(1, 1);

        let pred = pred_history(0, comp_a, [a(5)]);
        let auth = auth_history(0, comp_a, [a(9)]);
        let confirm = confirm_history([0]);
        app.world_mut()
            .spawn((Predicted, RemoteReplicated, pred, auth, confirm, A(5)));

        app.update();

        assert!(!app.world().resource::<DivergeOut>().0);
    }

    /// With no self-predicted entities at all, no rollback would change state.
    #[test]
    fn rollback_would_change_state_false_without_entities() {
        let (mut app, _comp_a) = init_diverge_app::<A>(1, 1);

        app.update();

        assert!(!app.world().resource::<DivergeOut>().0);
    }

    // TODO: This behavior is temporarily disabled, we need a better version of it
    //       that isn't as incompatible with required components
    // #[test]
    // fn reinsert_predicted_skips_authoritative_components() {
    //     let (mut app, comp_a) = init_app::<A, _>(0, super::reinsert_predicted);

    //     let comp_b = app.world_mut().register_component::<B>();

    //     app.world_mut()
    //         .resource_scope::<RollbackRegistry, _>(|world, mut registry| {
    //             registry.register::<B>(world)
    //         });

    //     let mut pred_hist = pred_history(0, comp_a, [a(5)]);
    //     pred_hist.insert(comp_b, comp_history(0, [b()]));

    //     let auth_hist = auth_history::<A>(0, comp_a, []);

    //     let e1 = app
    //         .world_mut()
    //         .spawn((Predicted, pred_hist, auth_hist))
    //         .id();

    //     app.update();

    //     let e = app.world().entity(e1);
    //     assert_eq!(None, e.get::<A>());
    //     assert_eq!(Some(&B), e.get::<B>());
    // }

    // TODO: Test command order, commands from loading should apply AFTER inserts/removes
}
