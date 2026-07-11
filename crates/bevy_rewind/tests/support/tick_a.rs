//! Shorthand [`TickData`] constructor for the `A` test component.

use bevy_rewind::history::component_history::TickData;

use super::comp_a::A;

/// Shorthand for a [`TickData::Value`] holding an [`A`]
pub fn a(v: u16) -> TickData<A> {
    TickData::Value(A(v))
}
