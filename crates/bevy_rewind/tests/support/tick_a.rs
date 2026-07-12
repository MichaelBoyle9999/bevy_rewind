use bevy_rewind::history::component_history::TickData;

use super::comp_a::A;

pub fn a(v: u16) -> TickData<A> {
    TickData::Value(A(v))
}
