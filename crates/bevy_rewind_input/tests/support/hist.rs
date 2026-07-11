//! The `hist` fixture: build an [`InputHistory`] literal for assertions.

use std::collections::VecDeque;

use bevy_replicon::shared::replicon_tick::RepliconTick;
use bevy_rewind_input::{InputHistory, InputTrait};

/// Construct an [`InputHistory`] whose first entry sits at `first_tick`.
pub fn hist<T: InputTrait>(first_tick: u32, list: impl IntoIterator<Item = T>) -> InputHistory<T> {
    let list = list.into_iter().collect::<VecDeque<T>>();
    InputHistory {
        updated_at: RepliconTick::new(first_tick + list.len().saturating_sub(1) as u32),
        list,
    }
}
