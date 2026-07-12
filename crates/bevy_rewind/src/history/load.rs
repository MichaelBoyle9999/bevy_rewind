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

    for (entity, mut predicted, maybe_authoritative, horizon, is_disabled) in q.iter_mut() {
        // Past the input horizon the authority is a guess; keep the prediction.
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

    for (entity, authoritative, confirmed, horizon) in q.iter_mut() {
        // Past the input horizon the authority is a guess; keep the prediction.
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

/// Resim of tick `t` loads at `t - 1`, so the load points span the closed range
/// `[start - 1, real_tick - 1]`.
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

pub struct DivergenceScan<'a> {
    pub registry: &'a RollbackRegistry,
    pub global_confirm: &'a ServerMutateTicks,
    pub lo: u32,
    pub hi: u32,
}

impl DivergenceScan<'_> {
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
                // Past the input horizon the authority is a guess, not a misprediction.
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
                    TickData::Missing => continue,
                    TickData::Removed => {
                        if matches!(pred_data, TickData::Value(_)) {
                            return true;
                        }
                    }
                    TickData::Value(auth_value) => match pred_data {
                        TickData::Value(pred_value) => {
                            // SAFETY: both pointers come from histories registered
                            // under `comp_id`, which is what `component` describes.
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

    for (entity, archetype, predicted, authoritative) in q.iter_mut() {
        for (&comp_id, pred_hist) in predicted.iter() {
            if archetype.contains(comp_id) {
                continue;
            }

            let TickData::Value(value) = pred_hist.get(previous_tick.get()) else {
                continue;
            };

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
