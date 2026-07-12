use super::component_history::ComponentHistory;
use crate::{Predicted, RollbackFrames};

use std::{fmt::Debug, mem::ManuallyDrop, num::NonZero};

use bevy::{
    ecs::component::{ComponentId, Mutable},
    platform::collections::HashMap,
    prelude::*,
};
use bevy_replicon::{
    bytes::Bytes,
    shared::{
        replication::{
            deferred_entity::DeferredEntity,
            registry::{
                ctx::{RemoveCtx, WriteCtx},
                rule_fns::RuleFns,
            },
        },
        replicon_tick::RepliconTick,
    },
};

pub struct AuthoriativeCleanupPlugin;

impl Plugin for AuthoriativeCleanupPlugin {
    fn build(&self, app: &mut App) {
        _ = app;
    }
}

#[derive(Component, Deref, DerefMut, Default)]
pub struct AuthoritativeHistory {
    #[deref]
    components: HashMap<ComponentId, ComponentHistory>,
}

pub(crate) fn write_authoritative_history<
    T: Component<Mutability = Mutable> + Clone + PartialEq + Debug,
>(
    ctx: &mut WriteCtx,
    rule_fns: &RuleFns<T>,
    entity: &mut DeferredEntity,
    cursor: &mut Bytes,
) -> Result<()> {
    let value = rule_fns.deserialize(ctx, cursor)?;
    let frames = entity
        .world()
        .get_resource::<RollbackFrames>()
        .copied()
        .unwrap_or_default();

    write_history_internal(ctx.component_id, entity, ctx.message_tick, value, frames);

    Ok(())
}

pub fn write_history_internal<T: Component + Clone + PartialEq + Debug>(
    component_id: ComponentId,
    entity: &mut DeferredEntity,
    received_tick: RepliconTick,
    value: T,
    frames: RollbackFrames,
) {
    let Some(mut history) = entity.get_mut::<AuthoritativeHistory>() else {
        warn_missing_authoritative_history(entity);
        return;
    };

    let comp_hist = history.entry(component_id).or_insert_with(|| {
        ComponentHistory::from_type::<T>(NonZero::new(frames.history_size() as u8).unwrap())
    });

    // SAFETY: We are writing to a history matching our ComponentId
    unsafe {
        comp_hist.write(received_tick.get(), |dst| {
            let value = ManuallyDrop::new(value);
            std::ptr::copy_nonoverlapping(
                (&value as *const ManuallyDrop<T>).cast(),
                dst.as_ptr(),
                size_of::<T>(),
            );
        });
    }
}

fn warn_missing_authoritative_history(entity: &DeferredEntity) {
    let diagnostic = if entity.contains::<Predicted>() {
        format!(
            "Predicted entity {} is missing AuthoritativeHistory",
            entity.id()
        )
    } else {
        format!(
            "Trying to write history to unpredicted entity {}",
            entity.id()
        )
    };
    warn!("{diagnostic}");
}

pub fn remove_authoritative_history(ctx: &mut RemoveCtx, entity: &mut DeferredEntity) {
    remove_history_internal(ctx.component_id, ctx.message_tick, entity);
}

pub fn remove_history_internal(
    component_id: ComponentId,
    tick: RepliconTick,
    entity: &mut DeferredEntity,
) {
    let Some(mut history) = entity.get_mut::<AuthoritativeHistory>() else {
        let diagnostic = format!(
            "Trying to remove history for {component_id:?} from entity without AuthoritativeHistory"
        );
        warn!("{diagnostic}");
        return;
    };
    let Some(comp_hist) = history.get_mut(&component_id) else {
        let diagnostic = format!(
            "Trying to remove history for {component_id:?} from entity without a history for it"
        );
        warn!("{diagnostic}");
        return;
    };

    comp_hist.mark_removed(tick.get());
}
