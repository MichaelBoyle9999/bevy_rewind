use bevy_replicon::shared::replicon_tick::RepliconTick;

pub fn r_tick(tick: u32) -> RepliconTick {
    RepliconTick::new(tick)
}
