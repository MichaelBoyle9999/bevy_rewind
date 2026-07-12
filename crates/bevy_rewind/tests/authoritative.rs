#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/drops.rs"]
mod drops;
#[path = "support/iter_enumerate.rs"]
mod iter_enumerate;
#[path = "support/tick_a.rs"]
mod tick_a;
#[path = "support/tick_data_deref.rs"]
mod tick_data_deref;
#[path = "support/ticks.rs"]
mod ticks;

use comp_a::A;
use drops::{D, DropList, assert_drops};
use iter_enumerate::IterEnumerate;
use tick_a::a;
use tick_data_deref::TickDataDeref;
use ticks::r_tick;

use TickData::*;
use bevy_replicon::shared::replication::deferred_entity::{DeferredChanges, DeferredEntity};
use bevy_rewind::history::RollbackRegistry;
use bevy_rewind::history::authoritative::{
    AuthoritativeHistory, remove_history_internal, write_history_internal,
};
use bevy_rewind::history::component_history::TickData;
use bevy_rewind::{Predicted, RollbackFrames, TEST_ROLLBACK_FRAMES};

use bevy::prelude::*;

trait DeferredEntityScope {
    fn entity_scope(&mut self, entity: Entity, f: impl Fn(&mut DeferredEntity));
}

impl DeferredEntityScope for World {
    fn entity_scope(&mut self, entity: Entity, f: impl Fn(&mut DeferredEntity)) {
        let mut changes = DeferredChanges::default();
        let entity = self.entity_mut(entity);
        let mut e = DeferredEntity::new(entity, &mut changes);
        f(&mut e);
        e.flush();
    }
}

#[test]
fn write_changes() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(&mut world);
    world.insert_resource(registry);
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();
    let e2 = world.spawn(AuthoritativeHistory::default()).id();

    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(0), A(1), frames);
    });

    world.entity_scope(e2, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(1), A(5), frames);
    });

    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(1), A(2), frames);
        write_history_internal::<A>(comp_a, e, r_tick(3), A(3), frames);
    });

    world.entity_scope(e2, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(2), A(7), frames);
    });

    use Missing as M;

    let e = world.entity(e1);
    let hist = e.get::<AuthoritativeHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [a(1), a(2), M, a(3), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }

    let e = world.entity(e2);
    let hist = e.get::<AuthoritativeHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [M, a(5), a(7), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
}

#[test]
fn write_out_of_order() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(&mut world);
    world.insert_resource(registry);
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();

    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(1), A(2), frames);

        write_history_internal::<A>(comp_a, e, r_tick(3), A(4), frames);
        write_history_internal::<A>(comp_a, e, r_tick(0), A(1), frames);

        write_history_internal::<A>(comp_a, e, r_tick(2), A(3), frames);
    });

    use Missing as M;

    let e = world.entity(e1);
    let hist = e.get::<AuthoritativeHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [a(1), a(2), a(3), a(4), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
}

#[test]
fn multiple_adds() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(&mut world);
    world.insert_resource(registry);
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();

    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(0), A(1), frames);
        write_history_internal::<A>(comp_a, e, r_tick(1), A(2), frames);
        write_history_internal::<A>(comp_a, e, r_tick(2), A(3), frames);
    });

    use Missing as M;

    let e = world.entity(e1);
    let hist = e.get::<AuthoritativeHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [a(1), a(2), a(3), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
}

#[test]
fn drop_once_on_success() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<D>(&mut world);
    world.insert_resource(registry);
    let comp_d = world.register_component::<D>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();

    let drops = DropList::default();

    world.entity_scope(e1, |e| {
        write_history_internal(comp_d, e, r_tick(0), D::new(1, &drops), frames);
        write_history_internal(comp_d, e, r_tick(1), D::new(2, &drops), frames);
        write_history_internal(comp_d, e, r_tick(2), D::new(3, &drops), frames);
        write_history_internal(comp_d, e, r_tick(3), D::new(4, &drops), frames);
        write_history_internal(comp_d, e, r_tick(4), D::new(5, &drops), frames);
    });

    assert_drops(&drops, []);

    world.despawn(e1);

    assert_drops(&drops, [1, 2, 3, 4, 5]);
}

#[test]
fn drop_once_on_fail() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<D>(&mut world);
    world.insert_resource(registry);
    let comp_d = world.register_component::<D>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();

    let drops = DropList::default();

    world.entity_scope(e1, |e| {
        write_history_internal(comp_d, e, r_tick(10), D::new(1, &drops), frames);
        write_history_internal(comp_d, e, r_tick(10), D::new(2, &drops), frames);
    });

    assert_drops(&drops, [1]);

    world.entity_scope(e1, |e| {
        write_history_internal(comp_d, e, r_tick(1), D::new(3, &drops), frames);
        write_history_internal(comp_d, e, r_tick(2), D::new(4, &drops), frames);
        write_history_internal(comp_d, e, r_tick(3), D::new(5, &drops), frames);
    });

    assert_drops(&drops, [1, 3, 4, 5]);

    world.despawn(e1);

    assert_drops(&drops, [1, 3, 4, 5, 2]);
}

#[test]
fn write_to_unpredicted_entity_is_ignored() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(&mut world);
    world.insert_resource(registry);
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn_empty().id();
    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(0), A(1), frames);
    });

    assert!(world.entity(e1).get::<AuthoritativeHistory>().is_none());
}

#[test]
fn write_to_predicted_missing_history_is_ignored() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(&mut world);
    world.insert_resource(registry);
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn(Predicted).id();
    world.entity_mut(e1).remove::<AuthoritativeHistory>();
    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(0), A(1), frames);
    });

    assert!(world.entity(e1).get::<AuthoritativeHistory>().is_none());
}

#[test]
fn remove_history_marks_removed() {
    let mut world = World::new();
    world.insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES));
    let frames = *world.resource::<RollbackFrames>();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(&mut world);
    world.insert_resource(registry);
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();
    world.entity_scope(e1, |e| {
        write_history_internal::<A>(comp_a, e, r_tick(0), A(1), frames);
        write_history_internal::<A>(comp_a, e, r_tick(1), A(2), frames);
        remove_history_internal(comp_a, r_tick(2), e);
    });

    let hist = world.entity(e1).get::<AuthoritativeHistory>().unwrap();
    assert_eq!(
        Missing,
        hist.get(&comp_a).unwrap().get(3).deref::<A>().cloned()
    );
    assert_eq!(
        Removed,
        hist.get(&comp_a).unwrap().get(2).deref::<A>().cloned()
    );
}

#[test]
fn remove_history_without_authoritative_history_is_ignored() {
    let mut world = World::new();
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn_empty().id();
    world.entity_scope(e1, |e| {
        remove_history_internal(comp_a, r_tick(0), e);
    });

    assert!(world.entity(e1).get::<AuthoritativeHistory>().is_none());
}

#[test]
fn remove_history_without_component_history_is_ignored() {
    let mut world = World::new();
    let comp_a = world.register_component::<A>();

    let e1 = world.spawn(AuthoritativeHistory::default()).id();
    world.entity_scope(e1, |e| {
        remove_history_internal(comp_a, r_tick(0), e);
    });

    let hist = world.entity(e1).get::<AuthoritativeHistory>().unwrap();
    assert!(!hist.contains_key(&comp_a));
}
