use crate::InputTrait;

use std::collections::VecDeque;

use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::shared::replicon_tick::RepliconTick;
use serde::{Deserialize, Serialize};

/// Capacity of the per-body input history ring. Must cover at least the
/// configured rollback window (`bevy_rewind::RollbackFrames`) plus the
/// application's tick lead, otherwise a wide rollback will resim with
/// `T::default()` for ticks outside the ring — which would silently lose
/// movement on a remote body. Public so consumers can put a compile-time
/// canary against it; see `game/src/networking/components.rs`.
pub const INPUT_HISTORY_CAPACITY: usize = 20;

/// The input history for an input. Used when sending data to the server, also useful for rollback
#[derive(Message, Component, Clone, TypePath, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(bound(deserialize = "T: for<'de2> serde::Deserialize<'de2>"))]
pub struct InputHistory<T: InputTrait> {
    // TODO: ArrayDeque?
    list: VecDeque<T>,
    updated_at: RepliconTick,
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
    /// Returns true is the history is empty
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Iterate over all inputs in the history
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.list.iter()
    }

    /// Get the tick the queue was last updated at
    pub fn updated_at(&self) -> RepliconTick {
        self.updated_at
    }

    /// Get the tick for the first input in the history
    pub fn first_tick(&self) -> RepliconTick {
        RepliconTick::new(
            self.updated_at
                .get()
                .saturating_sub(self.list.len().saturating_sub(1) as u32),
        )
    }

    /// Get the input for the specified tick, if it exists
    pub fn get(&self, tick: impl Into<RepliconTick>, repeat: bool) -> Option<T> {
        let tick = tick.into();
        if (tick > self.updated_at() && !(repeat && T::repeats())) || tick < self.first_tick() {
            return None;
        }
        if tick > self.updated_at() {
            return self
                .list
                .back()
                .and_then(|t| t.repeated(tick - self.updated_at()));
        }
        let index = tick - self.first_tick();
        self.list.get(index as usize).cloned()
    }

    /// Write an input to the history
    pub fn write(&mut self, tick: impl Into<RepliconTick>, value: T) {
        let tick = tick.into();
        if tick <= self.updated_at {
            warn!("Writing past values to history!");
            return;
        }

        if !self.list.is_empty() && tick > self.updated_at + 1 {
            if tick - self.updated_at > self.list.capacity() as u32 {
                self.list.clear();
            } else {
                while tick - self.first_tick() > self.list.capacity() as u32 {
                    self.list.pop_front();
                }
                self.list.extend(
                    (self.updated_at.get()..tick.get())
                        .skip(1)
                        .map(|_| T::default()),
                );
            }
        }

        if self.list.len() == self.list.capacity() {
            self.list.pop_front();
        }
        self.updated_at = tick;
        self.list.push_back(value);
    }

    #[cfg(feature = "client")]
    pub(super) fn replace_section(&mut self, iter: impl Iterator<Item = (RepliconTick, T)>) {
        for (tick, t) in iter {
            // TODO: Better capacity system
            if tick + 10 < self.updated_at {
                continue;
            } else if tick > self.updated_at {
                self.write(tick, t.clone());
            } else if tick < self.first_tick() {
                while tick + 1 < self.first_tick() {
                    self.list.push_front(T::default());
                }
                self.list.push_front(t.clone());
            } else if self.list.is_empty() {
                // Degenerate default history (`updated_at == 0`, empty list): the
                // tick lands "in range" only because `first_tick()` collapses to
                // `updated_at`. Seed the slot instead of indexing an empty deque.
                self.updated_at = tick;
                self.list.push_back(t.clone());
            } else {
                let index = tick - self.first_tick();
                self.list[index as usize] = t.clone();
            }
        }
    }

    /// Reset the input history to an empty state
    pub fn reset(&mut self) {
        self.updated_at = default();
        self.list.clear();
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::tests::{A, Tick};

    pub fn hist<T: InputTrait>(
        first_tick: u32,
        list: impl IntoIterator<Item = T>,
    ) -> InputHistory<T> {
        let list = list.into_iter().collect::<VecDeque<T>>();
        InputHistory {
            updated_at: RepliconTick::new(first_tick + list.len().saturating_sub(1) as u32),
            list,
        }
    }

    #[test]
    fn get() {
        let history = hist(10, [A(1), A(2), A(3), A(4), A(5)]);

        for i in 0..5 {
            assert_eq!(Some(A(1 + i)), history.get(Tick(10 + i as u32), false));
        }

        // All values outside of the history should return None
        assert_eq!(None, history.get(Tick(9), false));
        assert_eq!(None, history.get(Tick(0), false));
        assert_eq!(None, history.get(Tick(5), false));
        assert_eq!(None, history.get(Tick(15), false));
        assert_eq!(None, history.get(Tick(20), false));
        assert_eq!(None, history.get(Tick(598182), false));
    }

    #[test]
    fn get_repeats() {
        let history = hist(0, [A(1)]);

        // For a repeating input, the latest known value is returned for any
        // future tick — no hard cap. This is the "last input drives forever
        // until disconnect" pattern; the dropped cap removes a former tight
        // 5-tick window that snapped client prediction to default() under
        // any network jitter.
        for i in 0..5 {
            assert_eq!(Some(A(1)), history.get(Tick(i as u32), true));
        }
        assert_eq!(Some(A(1)), history.get(Tick(6), true));
        assert_eq!(Some(A(1)), history.get(Tick(2309), true));
    }

    #[test]
    fn write() {
        let mut history = InputHistory::<A>::default();

        history.write(Tick(15), A(1));
        assert_eq!(1, history.list.len());
        assert_eq!(RepliconTick::new(15), history.updated_at());

        history.write(Tick(16), A(2));
        assert_eq!(2, history.list.len());
        assert_eq!(RepliconTick::new(16), history.updated_at());

        // Writes in the past get ignored
        history.write(Tick(14), A(0));
        assert_eq!(2, history.list.len());
        assert_eq!(RepliconTick::new(16), history.updated_at());

        // When there's a gap, the history is patched up
        history.write(Tick(20), A(6));
        assert_eq!(6, history.list.len());
        assert_eq!(RepliconTick::new(20), history.updated_at());

        assert_eq!(hist(15, [A(1), A(2), A(0), A(0), A(0), A(6)]), history);

        // When the gap exceeds capacity, the old history is cleared. Gap must
        // exceed `INPUT_HISTORY_CAPACITY` (20), so write at tick 41 (gap 21).
        history.write(Tick(41), A(10));
        assert_eq!(1, history.list.len());
    }

    #[test]
    fn write_with_gaps_wrap() {
        let mut history = hist(10, (0..10).map(A));
        assert_eq!(10, history.list.len());

        history.write(Tick(25), A(15));
        assert_eq!(10, history.list.len());
        assert_eq!(
            hist(
                16,
                (6..10).map(A).chain((0..5).map(|_| A(0))).chain([A(15)])
            ),
            history
        );
    }

    #[test]
    fn first_tick() {
        let mut history = InputHistory::<A>::default();

        assert_eq!(RepliconTick::new(0), history.updated_at());
        assert_eq!(RepliconTick::new(0), history.first_tick());

        history.write(Tick(15), A(1));
        assert_eq!(RepliconTick::new(15), history.updated_at());
        assert_eq!(RepliconTick::new(15), history.first_tick());

        history.write(Tick(16), A(1));
        assert_eq!(2, history.list.len());
        assert_eq!(RepliconTick::new(15), history.first_tick());

        let history = hist(10, [A(0), A(1), A(2), A(3), A(4), A(5)]);
        assert_eq!(RepliconTick::new(15), history.updated_at());
        assert_eq!(6, history.list.len());
        assert_eq!(RepliconTick::new(10), history.first_tick());
    }

    #[cfg(feature = "client")]
    #[test]
    fn replace_section() {
        let original = hist(10, [A(1), A(2), A(3), A(4)]);

        // We replace a section at the end
        let mut history = original.clone();
        history.list.reserve_exact(6);
        history.replace_section((0..=1).map(|i| (Tick(13 + i).into(), A(10 + i as u8))));

        let expected = hist(10, [A(1), A(2), A(3), A(10), A(11)]);
        assert_eq!(expected, history);

        // We replace a section at the start
        let mut history = original.clone();
        history.list.reserve_exact(6);
        history.replace_section((0..=2).map(|i| (Tick(8 + i).into(), A(10 + i as u8))));

        let expected = hist(8, [A(10), A(11), A(12), A(2), A(3), A(4)]);
        assert_eq!(expected, history);

        // We replace a section in the middle
        let mut history = original.clone();
        history.list.reserve_exact(6);
        history.replace_section((0..=1).map(|i| (Tick(11 + i).into(), A(10 + i as u8))));

        let expected = hist(10, [A(1), A(10), A(11), A(4)]);
        assert_eq!(expected, history);

        // We replace the history with section much later
        let mut history = original.clone();
        history.list.reserve_exact(6);
        history.replace_section((0..=1).map(|i| (Tick(50 + i).into(), A(10 + i as u8))));

        let expected = hist(50, [A(10), A(11)]);
        assert_eq!(expected, history);
    }
}
