use crate::{LoadFrom, ResourceHistory, TickData};

use std::fmt::Debug;

use bevy::prelude::*;

pub fn load_and_clear_resource_prediction<T: Resource + Clone + Debug>(
    mut commands: Commands,
    t: Option<ResMut<T>>,
    mut hist: ResMut<ResourceHistory<T>>,
    previous_tick: Res<LoadFrom>,
) {
    match hist.get(**previous_tick) {
        TickData::Value(value) => {
            if let Some(mut t) = t {
                *t = value.clone();
            } else {
                commands.insert_resource(value.clone());
            }
        }
        TickData::Removed => {
            commands.remove_resource::<T>();
        }
        TickData::Missing => {
            commands.remove_resource::<T>();
            hist.keep_one();
            return;
        }
    }
    hist.clean(**previous_tick);
}

pub fn reinsert_predicted_resource<T: Resource + Clone>(
    mut commands: Commands,
    t: Option<Res<T>>,
    history: ResMut<ResourceHistory<T>>,
    previous_tick: Res<LoadFrom>,
) {
    if t.is_some() {
        return;
    }

    if let TickData::Value(v) = history.get(**previous_tick) {
        commands.insert_resource(v.clone());
    }
}
