use crate::{RollbackFrames, StoreFor, TickData};

use std::{collections::VecDeque, fmt::Debug};

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

#[derive(Resource, Clone)]
pub struct ResourceHistory<T> {
    pub list: VecDeque<TickData<T>>,
    pub last_tick: u32,
}

impl<T> Default for ResourceHistory<T> {
    fn default() -> Self {
        Self {
            list: default(),
            last_tick: 0,
        }
    }
}

impl<T> ResourceHistory<T> {
    pub fn from_list<const N: usize>(start_tick: u32, list: [TickData<T>; N]) -> Self {
        let last_tick = start_tick + (list.len() as u32).saturating_sub(1);
        Self {
            list: VecDeque::from(list),
            last_tick,
        }
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn get(&self, previous_tick: RepliconTick) -> &TickData<T> {
        if previous_tick.get() > self.last_tick {
            return &TickData::Missing;
        }
        let ago = (self.last_tick - previous_tick.get()) as usize;
        let len = self.list.len();
        if ago >= len {
            return if self
                .list
                .front()
                .is_some_and(|v| matches!(v, TickData::Removed))
            {
                &TickData::Removed
            } else {
                &TickData::Missing
            };
        }
        self.list.get(len - 1 - ago).unwrap_or(&TickData::Missing)
    }

    pub fn clean(&mut self, previous_tick: RepliconTick) {
        let ago = self.last_tick.saturating_sub(previous_tick.get());
        let len = self.list.len();
        self.list.drain(len.saturating_sub(ago as usize)..);
        self.last_tick = self.last_tick.min(previous_tick.get());
    }

    pub fn keep_one(&mut self) {
        let len = self.list.len();
        self.list.truncate(1);
        self.last_tick -= (len as u32).saturating_sub(1);
    }
}

pub fn append_history<T: Resource + Clone + Debug>(
    t: Option<Res<T>>,
    mut hist: ResMut<ResourceHistory<T>>,
    tick: Res<StoreFor>,
    frames: Res<RollbackFrames>,
) {
    let max_ticks = frames.history_size();

    let cap = hist.list.capacity();
    match cap.cmp(&max_ticks) {
        std::cmp::Ordering::Greater => {
            let mut old_list =
                std::mem::replace(&mut hist.list, VecDeque::with_capacity(max_ticks));
            let skip = old_list.len().saturating_sub(max_ticks);
            hist.list.extend(old_list.drain(..).skip(skip));
        }
        std::cmp::Ordering::Less => {
            hist.list.reserve_exact(max_ticks - cap);
        }
        _ => {}
    }

    if !hist.is_empty() {
        if tick.get() <= hist.last_tick {
            return;
        }
        while tick.get() > hist.last_tick + 1 {
            if hist.list.len() == hist.list.capacity() {
                hist.list.pop_front();
            }
            let cloned = hist.list.back().unwrap().clone();
            hist.list.push_back(cloned);
            hist.last_tick += 1;
        }
    }

    if hist.list.len() == hist.list.capacity() {
        hist.list.pop_front();
    }
    hist.list.push_back(
        t.map(|t| TickData::Value(t.clone()))
            .unwrap_or(TickData::Removed),
    );
    hist.last_tick = tick.get();
}

pub(super) fn save_initial<T: Resource + Clone + Debug>(
    t: Res<T>,
    mut history: ResMut<ResourceHistory<T>>,
    tick: Res<StoreFor>,
) {
    if history.is_empty() {
        history.last_tick = tick.get();
        history.list.push_back(TickData::Value(t.clone()));
    }
}
