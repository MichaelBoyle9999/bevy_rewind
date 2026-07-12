use arrayvec::ArrayVec;

#[cfg(feature = "server")]
mod queue;
#[cfg(feature = "server")]
pub use queue::InputQueue;

mod history;
pub use history::{INPUT_HISTORY_CAPACITY, InputHistory};

#[cfg(feature = "client")]
pub mod client;
#[cfg(feature = "server")]
pub mod server;
#[cfg(feature = "server")]
pub use server::InputTarget;

use bevy::{
    ecs::{component::Mutable, entity::MapEntities, intern::Interned, schedule::ScheduleLabel},
    prelude::*,
    reflect::TypePath,
};
use bevy_replicon::{prelude::*, shared::replicon_tick::RepliconTick};
use serde::{Deserialize, Serialize};

pub trait TickSource: Resource + Copy + From<RepliconTick> + Into<RepliconTick> {}

impl<T> TickSource for T where T: Resource + Copy + From<RepliconTick> + Into<RepliconTick> {}

pub struct InputQueuePlugin<T: InputTrait, Tick: TickSource> {
    #[cfg_attr(not(any(feature = "client", feature = "server")), allow(dead_code))]
    schedule: Interned<dyn ScheduleLabel>,
    phantom: std::marker::PhantomData<(T, Tick)>,
}

impl<T: InputTrait, Tick: TickSource> InputQueuePlugin<T, Tick> {
    pub fn new(schedule: impl ScheduleLabel) -> Self {
        Self {
            schedule: schedule.intern(),
            phantom: std::marker::PhantomData::<(T, Tick)>,
        }
    }
}

impl<T: InputTrait, Tick: TickSource> Plugin for InputQueuePlugin<T, Tick> {
    fn build(&self, app: &mut App) {
        app.add_mapped_client_message::<InputHistory<T>>(Channel::Unreliable)
            .add_mapped_server_message::<HistoryFor<T>>(Channel::Unreliable);

        #[cfg(feature = "client")]
        app.add_plugins(client::InputQueueClientPlugin::<T, Tick>::new(
            self.schedule,
        ));

        #[cfg(feature = "server")]
        app.add_plugins(server::InputQueueServerPlugin::<T, Tick>::new(
            self.schedule,
        ));
    }
}

#[derive(SystemSet, Clone, PartialEq, Eq, Debug, Hash)]
pub enum InputQueueSet {
    Network,
    Load,
    Clean,
}

pub trait InputTrait:
    Component<Mutability = Mutable>
    + Sync
    + Send
    + 'static
    + Clone
    + std::fmt::Debug
    + MapEntities
    + PartialEq
    + Serialize
    + for<'a> Deserialize<'a>
    + TypePath
    + Default
{
    fn repeats() -> bool;

    fn repeated(&self, _since: u32) -> Option<Self> {
        Self::repeats().then(|| self.clone())
    }
}

#[derive(Component, Default)]
pub struct InputAuthority;

#[derive(Resource, Debug, Clone, Copy)]
pub struct ConfirmedHorizon(pub u32);

impl Default for ConfirmedHorizon {
    fn default() -> Self {
        Self(u32::MAX)
    }
}

pub const SEAL_GRACE_TICKS: u32 = 2;

#[derive(Message, Clone, TypePath, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(bound(deserialize = "T: for<'de2> serde::Deserialize<'de2>"))]
pub struct HistoryFor<T: InputTrait> {
    pub entity: Entity,
    pub tick: RepliconTick,
    pub past: ArrayVec<(u8, T), 3>,
    pub future: ArrayVec<(u8, T), 7>,
}

impl<T: InputTrait> MapEntities for HistoryFor<T> {
    fn map_entities<M: EntityMapper>(&mut self, mapper: &mut M) {
        self.entity = mapper.get_mapped(self.entity);
        self.past
            .iter_mut()
            .for_each(|(_, t)| t.map_entities(mapper));
        self.future
            .iter_mut()
            .for_each(|(_, t)| t.map_entities(mapper));
    }
}
