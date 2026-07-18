use crate::InputTrait;

use std::collections::VecDeque;

use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use serde::{Deserialize, Serialize};

pub const INPUT_HISTORY_CAPACITY: usize = 25;

#[derive(Message, Component, Clone, TypePath, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: for<'de2> serde::Deserialize<'de2>"))]
pub struct InputHistory<T: InputTrait> {
    pub list: VecDeque<T>,
    pub updated_at: RepliconTick,
}

impl<T: InputTrait> Default for InputHistory<T> {
    fn default() -> Self {
        Self {
            list: std::collections::VecDeque::with_capacity(INPUT_HISTORY_CAPACITY),
            updated_at: default(),
        }
    }
}

impl<T: InputTrait> MapEntities for InputHistory<T> {
    fn map_entities<M: EntityMapper>(&mut self, mapper: &mut M) {
        for t in self.list.iter_mut() {
            t.map_entities(mapper);
        }
    }
}

impl<T: InputTrait> InputHistory<T> {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.list.iter()
    }

    pub fn updated_at(&self) -> RepliconTick {
        self.updated_at
    }

    pub fn first_tick(&self) -> RepliconTick {
        RepliconTick::new(
            self.updated_at
                .get()
                .saturating_sub(self.list.len().saturating_sub(1) as u32),
        )
    }

    pub fn get(&self, tick: impl Into<RepliconTick>, repeat: bool) -> Option<T> {
        let tick = tick.into();
        if tick < self.first_tick() {
            return None;
        }
        if tick > self.updated_at() {
            if !(repeat && T::repeats()) {
                return None;
            }
            return self
                .list
                .back()
                .and_then(|t| t.repeated(tick - self.updated_at()));
        }
        let index = tick - self.first_tick();
        self.list.get(index as usize).cloned()
    }

    pub fn write(&mut self, tick: impl Into<RepliconTick>, value: T) {
        let tick = tick.into();
        if tick <= self.updated_at {
            warn!("Writing past values to history!");
            return;
        }

        if !self.list.is_empty() {
            if tick - self.updated_at > self.list.capacity() as u32 {
                self.list.clear();
            } else {
                while tick - self.first_tick() > self.list.capacity() as u32 {
                    self.list.pop_front();
                }
                let last = self.list.back().cloned();
                let updated_at = self.updated_at.get();
                self.list
                    .extend((updated_at..tick.get()).skip(1).map(|gap_tick| {
                        last.as_ref()
                            .and_then(|input| input.repeated(gap_tick - updated_at))
                            .unwrap_or_default()
                    }));
            }
        }

        if self.list.len() == self.list.capacity() {
            self.list.pop_front();
        }
        self.updated_at = tick;
        self.list.push_back(value);
    }

    #[cfg(feature = "client")]
    pub fn replace_section(&mut self, iter: impl Iterator<Item = (RepliconTick, T)>) {
        for (tick, t) in iter {
            if tick + 10 < self.updated_at {
                continue;
            } else if tick > self.updated_at {
                self.write(tick, t.clone());
            } else if tick < self.first_tick() {
                while tick + 1 < self.first_tick() {
                    let gap = self.first_tick() - tick - 1;
                    self.list.push_front(t.repeated(gap).unwrap_or_default());
                }
                self.list.push_front(t.clone());
            } else if self.list.is_empty() {
                self.updated_at = tick;
                self.list.push_back(t.clone());
            } else {
                let index = tick - self.first_tick();
                self.list[index as usize] = t.clone();
            }
        }
    }

    pub fn reset(&mut self) {
        self.updated_at = default();
        self.list.clear();
    }
}
