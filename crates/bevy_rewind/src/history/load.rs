use super::{
    RollbackRegistry,
    authoritative::AuthoritativeHistory,
    batch::{InsertBatch, RemoveBatch},
    component_history::TickData,
    confirmed::ConfirmedInputHorizon,
    predicted::PredictedHistory,
};
use crate::{LoadFrom, Predicted, RollbackLoadSet, RollbackSchedule};

use bevy::{
    ecs::{archetype::Archetype, entity_disabling::Disabled, world::EntityMutExcept},
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

pub fn load_and_clear_prediction(
    mut commands: Commands,
    mut q: Query<
        (
            Entity,
            &mut PredictedHistory,
            Option<(&AuthoritativeHistory, &ConfirmHistory)>,
            Option<&ConfirmedInputHorizon>,
            Has<Disabled>,
        ),
        (With<Predicted>, Or<(With<Disabled>, Without<Disabled>)>),
    >,
    registry: Res<RollbackRegistry>,
    previous_tick: Res<LoadFrom>,
    global_confirm: Res<ServerMutateTicks>,
) {
    let mut inserts = InsertBatch::new();
    let mut removes = RemoveBatch::new();

    // TODO: Can we par_iter this?
    for (entity, mut predicted, maybe_authoritative, horizon, is_disabled) in q.iter_mut() {
        // Past the body's received-input horizon the authority is a guess; keep the
        // prediction (treat auth as Missing) rather than reconcile to it.
        let beyond_horizon = horizon.is_some_and(|h| previous_tick.get() > h.0);
        for (&comp_id, pred_hist) in predicted.iter_mut() {
            let &reg_idx = registry.ids.get(&comp_id).unwrap();
            let component = registry.components.get(reg_idx).unwrap();

            let auth = if beyond_horizon {
                TickData::Missing
            } else {
                maybe_authoritative
                    .and_then(|(authoritative, confirmed)| {
                        let auth_hist = authoritative.get(&comp_id)?;
                        let check_range = auth_hist.empty_after(previous_tick.get());
                        let end_tick = RepliconTick::new(previous_tick.get() + check_range);
                        (confirmed.contains_any(**previous_tick, end_tick)
                            || global_confirm.contains_any(**previous_tick, end_tick))
                        .then(|| auth_hist.get_latest(previous_tick.get()))
                    })
                    .unwrap_or(TickData::Missing)
            };

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
                        component.load_to_uninit(auth.value(), pred.value(), dst);
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
    }
}

pub fn load_confirmed_authoritative(
    mut commands: Commands,
    mut q: Query<
        (
            EntityMutExcept<(AuthoritativeHistory, ConfirmHistory, ConfirmedInputHorizon)>,
            &AuthoritativeHistory,
            &ConfirmHistory,
            Option<&ConfirmedInputHorizon>,
        ),
        (With<Predicted>, Or<(With<Disabled>, Without<Disabled>)>),
    >,
    registry: Res<RollbackRegistry>,
    previous_tick: Res<LoadFrom>,
    global_confirm: Res<ServerMutateTicks>,
) {
    let mut inserts = InsertBatch::new();
    let mut removes = RemoveBatch::new();

    // TODO: Can we par_iter this?
    for (entity, authoritative, confirmed, horizon) in q.iter_mut() {
        // Past the body's received-input horizon the authority is a guess; keep the
        // resim's own prediction for this body rather than reconciling to it.
        if horizon.is_some_and(|h| previous_tick.get() > h.0) {
            continue;
        }
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
                        component.load_to_uninit(Some(value), entity.get_by_id(comp_id), dst);
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
    }
}

/// Query of the histories needed to decide whether a self-predicted entity
/// mispredicted. Every [`Predicted`] body — whether driven by local input or by a
/// remote peer's replayed input — flows through this one rollback path.
pub type DivergenceQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static PredictedHistory,
        &'static AuthoritativeHistory,
        &'static ConfirmHistory,
        Option<&'static ConfirmedInputHorizon>,
    ),
    With<Predicted>,
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
pub fn rollback_would_change_state(
    q: &DivergenceQuery,
    registry: &RollbackRegistry,
    global_confirm: &ServerMutateTicks,
    start: RepliconTick,
    real_tick: RepliconTick,
) -> bool {
    let scan = DivergenceScan {
        registry,
        global_confirm,
        lo: start.get().saturating_sub(1),
        hi: real_tick.get().saturating_sub(1),
    };
    q.iter()
        .any(|(pred, auth, confirm, horizon)| scan.entity_diverged(pred, auth, confirm, horizon))
}

/// The shared context of a divergence scan: the component registry, the global
/// confirm channel, and the load-point range `[lo, hi]` under evaluation.
pub struct DivergenceScan<'a> {
    /// The rollback component registry
    pub registry: &'a RollbackRegistry,
    /// The global confirm channel
    pub global_confirm: &'a ServerMutateTicks,
    /// The low end of the load-point range under evaluation
    pub lo: u32,
    /// The high end of the load-point range under evaluation
    pub hi: u32,
}

impl DivergenceScan<'_> {
    /// Per-entity half of [`rollback_would_change_state`]: scans the load-point
    /// range `[lo, hi]` for a confirmed authoritative value that differs from the
    /// predicted value, mirroring [`load_confirmed_authoritative`]'s confirm gate
    /// and `get_latest` lookups.
    pub fn entity_diverged(
        &self,
        pred: &PredictedHistory,
        auth: &AuthoritativeHistory,
        confirm: &ConfirmHistory,
        horizon: Option<&ConfirmedInputHorizon>,
    ) -> bool {
        for (&comp_id, auth_hist) in auth.iter() {
            let &reg_idx = self.registry.ids.get(&comp_id).unwrap();
            let component = self.registry.components.get(reg_idx).unwrap();
            let pred_hist = pred.get(&comp_id);

            for p in self.lo..=self.hi {
                // Past the body's received-input horizon the authority is a guess, not
                // a misprediction to correct — skip it so it never triggers a rollback.
                if horizon.is_some_and(|h| p > h.0) {
                    continue;
                }
                let previous_tick = RepliconTick::new(p);
                let check_range = auth_hist.empty_after(p);
                let end_tick = RepliconTick::new(p + check_range);
                if !confirm.contains_any(previous_tick, end_tick)
                    && !self.global_confirm.contains_any(previous_tick, end_tick)
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
                            // Tolerance-aware: a difference within the component's
                            // registered tolerance is not a divergence, so float noise
                            // below the sim's non-determinism floor never rolls back.
                            if !unsafe { component.within_tolerance(auth_value, pred_value) } {
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
}

pub fn reinsert_predicted(
    mut commands: Commands,
    mut q: Query<
        (Entity, &Archetype, &PredictedHistory, &AuthoritativeHistory),
        (With<Predicted>, Or<(With<Disabled>, Without<Disabled>)>),
    >,
    registry: Res<RollbackRegistry>,
    previous_tick: Res<LoadFrom>,
) {
    let mut inserts = InsertBatch::new();

    // TODO: Can we par_iter this?
    for (entity, archetype, predicted, authoritative) in q.iter_mut() {
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
                component.load_to_uninit(None, Some(value), dst);
            });
        }

        if !inserts.is_empty() {
            commands.entity(entity).queue(inserts.clone());
            inserts.clear();
        }
    }
}
