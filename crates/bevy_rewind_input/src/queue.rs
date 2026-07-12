use crate::{InputHistory, InputTrait};

use arraydeque::{ArrayDeque, Wrapping};
use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

#[derive(Component, Debug)]
pub struct InputQueue<T: InputTrait> {
    pub past: ArrayDeque<(RepliconTick, T), 3, Wrapping>,
    pub queue: ArrayDeque<(RepliconTick, T), 30>,
    received_horizon: Option<RepliconTick>,
}

impl<T: InputTrait> Default for InputQueue<T> {
    fn default() -> Self {
        Self {
            past: ArrayDeque::new(),
            queue: ArrayDeque::new(),
            received_horizon: None,
        }
    }
}

impl<T: InputTrait> InputQueue<T> {
    pub fn past(&self) -> impl Iterator<Item = &(RepliconTick, T)> {
        self.past.iter()
    }

    pub fn queue(&self) -> impl Iterator<Item = &(RepliconTick, T)> {
        self.queue.iter()
    }

    pub fn received_horizon(&self) -> Option<RepliconTick> {
        self.received_horizon
    }

    pub fn add(
        &mut self,
        tick: impl Into<RepliconTick>,
        history: &InputHistory<T>,
    ) -> Option<RepliconTick> {
        if history.is_empty() {
            return None;
        }
        let cur_tick = tick.into();
        let history_first = history.first_tick();
        let history_last = history.updated_at();

        self.received_horizon = Some(match self.received_horizon {
            Some(prev) if prev >= history_last => prev,
            _ => history_last,
        });

        let highest_consumed = self.past.back().map(|(t, _)| t.get());

        let mut earliest_novel: Option<RepliconTick> = None;
        for (i, new_val) in history.iter().enumerate() {
            let t = RepliconTick::new(history_first.get() + i as u32);
            if t >= cur_tick {
                break;
            }
            let existing: Option<&T> = self
                .past
                .iter()
                .find(|(pt, _)| *pt == t)
                .map(|(_, v)| v)
                .or_else(|| self.queue.iter().find(|(qt, _)| *qt == t).map(|(_, v)| v));
            let is_novel = match existing {
                Some(v) => v != new_val,
                None => match highest_consumed {
                    None => true,
                    Some(hc) => {
                        t.get() > hc && {
                            let predicted = self
                                .past
                                .back()
                                .and_then(|(bt, bv)| bv.repeated(t.get() - bt.get()))
                                .unwrap_or_default();
                            predicted != *new_val
                        }
                    }
                },
            };
            if is_novel {
                earliest_novel = Some(t);
                break;
            }
        }

        let existing: Vec<(RepliconTick, T)> = self
            .queue
            .drain(..)
            .filter(|(t, _)| *t < history_first || *t > history_last)
            .collect();

        let mut combined: Vec<(RepliconTick, T)> =
            Vec::with_capacity(existing.len() + history.iter().count());
        combined.extend(existing);
        combined.extend(
            history
                .iter()
                .enumerate()
                .map(|(i, t)| (history_first + i as u32, t.clone())),
        );
        combined.sort_by_key(|(t, _)| t.get());

        let cap = self.queue.capacity();
        let skip = combined.len().saturating_sub(cap);
        for entry in combined.into_iter().skip(skip) {
            let _ = self.queue.push_back(entry);
        }

        earliest_novel
    }

    pub fn next(&mut self, tick: impl Into<RepliconTick>) -> Option<T> {
        let tick = tick.into();
        let mut newest = None;
        while self.queue.front().is_some_and(|(t, _)| *t <= tick) {
            newest = self.queue.pop_front();
        }
        let hit = newest.and_then(|(from_tick, t)| {
            if from_tick == tick {
                Some(t)
            } else {
                t.repeated(tick - from_tick)
            }
        });
        if let Some(input) = hit {
            self.past.push_back((tick, input.clone()));
            return Some(input);
        }
        self.past
            .back()
            .and_then(|(from_tick, t)| t.repeated(tick - *from_tick))
    }
}
