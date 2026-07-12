use crate::SpawnedAt;

use std::marker::PhantomData;

use bevy::{
    ecs::{entity_disabling::Disabled, lifecycle::HookContext, world::DeferredWorld},
    prelude::*,
};
use bevy_replicon::shared::{
    replication::registry::{ReplicationRegistry, ctx::DespawnCtx},
    replicon_tick::RepliconTick,
};
use bevy_rewind::{
    Predicted, Resimulating, RollbackFrames, RollbackSchedule, RollbackStoreSet, RollbackTarget,
    StoreScheduleLabel, TickSource,
};

pub struct EntityManagementPlugin<Tick: TickSource>(PhantomData<Tick>);

impl<Tick: TickSource> Default for EntityManagementPlugin<Tick> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Tick: TickSource> EntityManagementPlugin<Tick> {
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Tick: TickSource> Plugin for EntityManagementPlugin<Tick> {
    fn build(&self, app: &mut App) {
        app.world_mut().register_component::<Despawned>();
        app.world_mut().register_component::<Unspawned>();

        app.world_mut()
            .resource_mut::<ReplicationRegistry>()
            .despawn = replicon_despawn::<Tick>;

        let store_schedule = **app.world().resource::<StoreScheduleLabel>();
        app.add_systems(
            store_schedule,
            (
                disable_server_despawned_entities::<Tick>,
                despawn_unused_entities::<Tick>,
            )
                .before(RollbackStoreSet),
        )
        .insert_resource(GetTickDeferred(|world| (*world.resource::<Tick>()).into()))
        .add_observer(stamp_spawned_at::<Tick>)
        .add_systems(
            RollbackSchedule::PreRollback,
            disable_unspawned_during_rollback,
        )
        .add_systems(
            RollbackSchedule::PreResimulation,
            reenable_at_spawn_tick::<Tick>,
        )
        .add_systems(RollbackSchedule::BackToPresent, despawn_unspawned_entities);
    }
}

fn stamp_spawned_at<Tick: TickSource>(
    add: On<Add, Predicted>,
    mut commands: Commands,
    existing: Query<&SpawnedAt>,
    tick: Res<Tick>,
    resimulating: Option<Res<Resimulating>>,
) {
    if resimulating.is_some() {
        return;
    }
    let entity = add.entity;
    if existing.get(entity).is_ok() {
        return;
    }
    let cur: RepliconTick = (*tick).into();
    commands.entity(entity).insert(SpawnedAt(cur));
}

// Must disable before bevy_rewind's `load_and_clear_prediction`
// (`RollbackSchedule::Rollback`) so its disabled-guards skip these entities.
fn disable_unspawned_during_rollback(
    mut commands: Commands,
    q: Query<(Entity, &SpawnedAt), (With<Predicted>, Without<Disabled>)>,
    target: Res<RollbackTarget>,
) {
    let Some(start) = **target else {
        return;
    };
    let load_from = start.get().saturating_sub(1);
    for (entity, spawned_at) in q.iter() {
        if (**spawned_at).get() > load_from {
            commands.entity(entity).insert(Unspawned);
        }
    }
}

fn reenable_at_spawn_tick<Tick: TickSource>(
    mut commands: Commands,
    q: Query<(Entity, &SpawnedAt), (With<Unspawned>, Or<(With<Disabled>, Without<Disabled>)>)>,
    tick: Res<Tick>,
) {
    let cur: RepliconTick = (*tick).into();
    for (entity, spawned_at) in q.iter() {
        if (**spawned_at).get() <= cur.get() {
            commands.entity(entity).remove::<Unspawned>();
        }
    }
}

fn replicon_despawn<Tick: TickSource>(ctx: &DespawnCtx, mut entity: EntityWorldMut) {
    if entity.contains::<Predicted>() {
        if ctx.message_tick >= (*entity.world().resource::<Tick>()).into() {
            entity.insert((RemovedByServerAt(ctx.message_tick), Despawned));
        } else {
            entity.insert(RemovedByServerAt(ctx.message_tick));
        }
        return;
    }

    entity.despawn();
}

#[derive(Component, Clone, Copy, Deref)]
struct RemovedByServerAt(RepliconTick);

fn disable_server_despawned_entities<Tick: TickSource>(
    mut commands: Commands,
    query: Query<
        (Entity, &RemovedByServerAt, Has<Despawned>),
        Or<(With<Disabled>, Without<Disabled>)>,
    >,
    tick: Res<Tick>,
    frames: Res<RollbackFrames>,
) {
    let tick = (*tick).into();
    for (entity, at, is_despawned) in query.iter() {
        if is_despawned && **at + (frames.history_size() as u32) < tick {
            commands.entity(entity).try_despawn();
        } else if !is_despawned && **at <= tick {
            commands.entity(entity).insert(Despawned);
        }
    }
}

#[derive(Resource)]
struct GetTickDeferred(fn(&DeferredWorld) -> RepliconTick);

#[derive(Component, Clone, Copy)]
#[component(on_insert=track_unused)]
#[component(on_remove=untrack_unused)]
#[require(Disabled, UnusedAt)]
pub struct Despawned;

fn track_unused(mut world: DeferredWorld, ctx: HookContext) {
    let get_tick = world.resource::<GetTickDeferred>();
    let tick = get_tick.0(&world);
    world.commands().entity(ctx.entity).insert(UnusedAt(tick));
}

fn untrack_unused(mut world: DeferredWorld, ctx: HookContext) {
    world.commands().entity(ctx.entity).try_remove::<UnusedAt>();
    reenable(world, ctx)
}

fn reenable(mut world: DeferredWorld, ctx: HookContext) {
    let despawned_id = world.component_id::<Despawned>().unwrap();
    let unspawned_id = world.component_id::<Unspawned>().unwrap();
    let entity = world.entity(ctx.entity);
    let removed_or_missing = |comp_id| ctx.component_id == comp_id || !entity.contains_id(comp_id);

    if !removed_or_missing(despawned_id) || !removed_or_missing(unspawned_id) {
        return;
    }

    world.commands().entity(ctx.entity).try_remove::<Disabled>();
}

#[derive(Component)]
#[component(on_remove = reenable)]
#[require(Disabled)]
pub struct Unspawned;

#[derive(Component, Deref, Default)]
struct UnusedAt(RepliconTick);

fn despawn_unused_entities<Tick: TickSource>(
    mut commands: Commands,
    query: Query<(Entity, &UnusedAt), (With<Disabled>, With<Despawned>)>,
    tick: Res<Tick>,
    frames: Res<RollbackFrames>,
) {
    for (entity, unused_at) in query.iter() {
        if **unused_at + (frames.history_size() as u32) < (*tick).into() {
            commands.entity(entity).try_despawn();
        }
    }
}

fn despawn_unspawned_entities(
    mut commands: Commands,
    query: Query<Entity, (With<Disabled>, With<Unspawned>)>,
) {
    for entity in query.iter() {
        commands.entity(entity).try_despawn();
    }
}
