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

/// A plugin adding rollback-friendly entity management to the app.
pub struct EntityManagementPlugin<Tick: TickSource>(PhantomData<Tick>);

impl<Tick: TickSource> Default for EntityManagementPlugin<Tick> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Tick: TickSource> EntityManagementPlugin<Tick> {
    /// Construct the `EntityManagementPlugin`
    pub fn new() -> Self {
        Self(PhantomData)
    }
}

impl<Tick: TickSource> Plugin for EntityManagementPlugin<Tick> {
    fn build(&self, app: &mut App) {
        // `reenable` (the on_remove hook for both `Despawned` and `Unspawned`)
        // looks up both components by id and unwraps; force-register them here so
        // the hook works even before any entity has either marker inserted.
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

/// Stamp [`SpawnedAt`] onto a newly-added [`Predicted`] entity from the current
/// tick source. Skips during a resim so a rollback-driven re-add does not
/// overwrite the original tick.
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

/// Just-before-rollback: mark every [`Predicted`] entity whose [`SpawnedAt`] is
/// strictly after the rollback target as [`Unspawned`], which `#[require]`s
/// [`Disabled`]. The disable lands before `bevy_rewind`'s
/// `load_and_clear_prediction` runs in `RollbackSchedule::Rollback` (commands
/// flush between schedules), so its `if !is_disabled` guards skip these entities
/// and the `(Missing, Missing)` arm cannot strip their components. The entity is
/// then naturally re-enabled by [`reenable_at_spawn_tick`] when forward resim
/// crosses its spawn tick. We skip entities that are already [`Disabled`] for
/// any other reason (typically [`Despawned`] lifecycle) so we don't entangle
/// with that flow.
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

/// Per-resim-tick: remove [`Unspawned`] from entities whose [`SpawnedAt`] tick
/// has been reached by forward resim. The component's `on_remove = reenable`
/// hook drops [`Disabled`] in the same flush so the entity participates in the
/// simulation schedule for that tick onward. Runs in `RollbackSchedule::PreResimulation`,
/// before the user-registered simulation schedule but after the per-iter `Tick`
/// resource has been written by `trigger_rollback`.
///
/// Disabled entities are filtered out of normal queries; the
/// `Or<(With<Disabled>, Without<Disabled>)>` predicate opts back in so we can
/// see them.
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

/// A marker for entities that should be despawned once they fall out of history
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

/// A marker for entities that have been rolled back to before they spawned.
/// These entities are despawned if they aren't re-enabled before [`RollbackSchedule::BackToPresent`]
#[derive(Component)]
#[component(on_remove = reenable)]
#[require(Disabled)]
pub struct Unspawned;

/// A component tracking when an entity became unused, it will be despawned once this tick is
/// outside of the history range.
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
