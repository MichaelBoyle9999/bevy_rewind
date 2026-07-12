#[path = "support/comp_a.rs"]
mod comp_a;
#[path = "support/comp_b.rs"]
mod comp_b;
#[path = "support/drops.rs"]
mod drops;
#[path = "support/iter_enumerate.rs"]
mod iter_enumerate;
#[path = "support/tick_a.rs"]
mod tick_a;
#[path = "support/tick_data_deref.rs"]
mod tick_data_deref;

use comp_a::A;
use comp_b::B;
use drops::{D, DropList, assert_drops};
use iter_enumerate::IterEnumerate;
use tick_a::a;
use tick_data_deref::TickDataDeref;

use TickData::*;
use bevy_rewind::history::RollbackRegistry;
use bevy_rewind::history::component_history::TickData;
use bevy_rewind::history::predicted::{ArchetypeCache, PredictedHistory, run_store};
use bevy_rewind::{Predicted, RollbackFrames, StoreFor, TEST_ROLLBACK_FRAMES};

use bevy::prelude::*;
use bevy_replicon::shared::replicon_tick::RepliconTick;

#[derive(Component, Clone, PartialEq, Deref, DerefMut, Debug)]
struct F(f32);

fn init_app() -> App {
    let mut app = App::new();
    app.init_resource::<ArchetypeCache>()
        .insert_resource(RollbackFrames::new(TEST_ROLLBACK_FRAMES))
        .add_systems(Update, run_store);
    app
}

#[test]
fn history_stores_changes() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1)))
        .id();
    let e2 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(12)))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    app.insert_resource(registry);

    for i in 0..=5 {
        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();
        **app.world_mut().entity_mut(e1).get_mut::<A>().unwrap() += 1;
        **app.world_mut().entity_mut(e2).get_mut::<A>().unwrap() -= 1;
    }

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    use Missing as M;

    let e = world.entity(e1);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [a(1), a(2), a(3), a(4), a(5), a(6), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }

    let e = world.entity(e2);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [a(12), a(11), a(10), a(9), a(8), a(7), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
}

#[test]
fn stores_removed() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1)))
        .id();
    let e2 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(12)))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    app.insert_resource(registry);

    for i in 0..=5 {
        if i == 1 {
            app.world_mut().entity_mut(e1).remove::<A>();
        }
        if i == 3 {
            app.world_mut().entity_mut(e2).remove::<A>();
        }

        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();
    }

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();

    let e = world.entity(e1);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for i in 0..=5 {
        let v = hist
            .get(&comp_a)
            .unwrap()
            .get(i as u32)
            .deref::<A>()
            .cloned();
        if i == 1 {
            assert_eq!(Removed, v);
        } else {
            assert_ne!(Removed, v);
        }
    }

    let e = world.entity(e2);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for i in 0..=5 {
        let v = hist
            .get(&comp_a)
            .unwrap()
            .get(i as u32)
            .deref::<A>()
            .cloned();
        if i == 3 {
            assert_eq!(Removed, v);
        } else {
            assert_ne!(Removed, v);
        }
    }
}

#[test]
fn history_skips_unchanged() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1), F(f32::NAN)))
        .id();
    let e2 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(10), F(f32::NAN)))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    registry.register::<F>(app.world_mut());
    app.insert_resource(registry);

    for i in 0..7 {
        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();

        let change = (i % 3 == 2) as u16;
        **app.world_mut().entity_mut(e1).get_mut::<A>().unwrap() += change;
        **app.world_mut().entity_mut(e1).get_mut::<F>().unwrap() += change as f32;

        if i % 3 == 0 {
            **app.world_mut().entity_mut(e2).get_mut::<A>().unwrap() += 1;
            **app.world_mut().entity_mut(e2).get_mut::<F>().unwrap() += 1.;
        }
    }

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let comp_f = world.register_component::<F>();
    use Missing as M;

    let e = world.entity(e1);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    assert!(hist.contains_key(&comp_f));
    for (i, v) in [a(1), M, M, a(2), M, M, a(3), M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
    for i in 0..7 {
        let TickData::Value(ptr) = hist.get(&comp_f).unwrap().get(i as u32) else {
            panic!("expected a value at tick {i}");
        };
        assert!(unsafe { ptr.deref::<F>() }.is_nan());
    }

    let e = world.entity(e2);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    assert!(hist.contains_key(&comp_f));
    for (i, v) in [a(10), a(11), M, M, a(12), M, M, M].iter_enumerate() {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
    let (v, m) = (true, false);
    for (i, present) in [v, v, m, m, v, m, m, m].iter_enumerate() {
        let entry = hist.get(&comp_f).unwrap().get(i as u32);
        if present {
            let TickData::Value(ptr) = entry else {
                panic!("expected a value at tick {i}");
            };
            assert!(unsafe { ptr.deref::<F>() }.is_nan());
        } else {
            assert!(matches!(entry, Missing));
        }
    }
}

#[test]
fn stores_reinserts() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1)))
        .id();
    let e2 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(12)))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    app.insert_resource(registry);

    for i in 0..=5 {
        if i == 1 {
            app.world_mut().entity_mut(e1).remove::<A>();
        }
        if i == 2 {
            app.world_mut().entity_mut(e1).insert(A(2));
            app.world_mut().entity_mut(e2).remove::<A>();
        }
        if i == 3 {
            app.world_mut().entity_mut(e2).insert(A(20));
        }

        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();
    }

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    use Removed as R;

    let e = world.entity(e1);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [(0, a(1)), (1, R), (2, a(2))] {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }

    let e = world.entity(e2);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [(0, a(12)), (2, R), (3, a(20))] {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
}

#[test]
fn stores_inserts() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1)))
        .id();
    let e2 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default()))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    registry.register::<B>(app.world_mut());
    app.insert_resource(registry);

    for i in 0..=5 {
        if i == 2 {
            app.world_mut().entity_mut(e1).insert(B);
        }
        if i == 3 {
            app.world_mut().entity_mut(e2).insert(A(2));
        }

        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();
    }

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let comp_b = world.register_component::<B>();

    let e = world.entity(e1);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    assert!(hist.contains_key(&comp_b));
    assert!(matches!(hist.get(&comp_b).unwrap().get(1), Missing));
    assert!(matches!(hist.get(&comp_b).unwrap().get(2), Value(_)));

    let e = world.entity(e2);
    let hist = e.get::<PredictedHistory>().unwrap();
    assert!(hist.contains_key(&comp_a));
    for (i, v) in [(2, Missing), (3, Value(A(2)))] {
        assert_eq!(v, hist.get(&comp_a).unwrap().get(i as u32).deref().cloned());
    }
}

#[test]
fn drop_once_unique_values() {
    let mut app = init_app();
    let drops = DropList::default();

    let mut registry = RollbackRegistry::default();
    registry.register::<D>(app.world_mut());
    app.insert_resource(registry);

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), D::new(1, &drops)))
        .id();

    for i in 0..5 {
        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();
        app.world_mut().entity_mut(e1).get_mut::<D>().unwrap().0 += 1;
    }

    assert_drops(&drops, []);

    app.world_mut().despawn(e1);

    assert_drops(&drops, [6, 1, 2, 3, 4, 5]);
}

#[test]
fn drop_once_duplicates() {
    let mut app = init_app();
    let drops = DropList::default();

    let mut registry = RollbackRegistry::default();
    registry.register::<D>(app.world_mut());
    app.insert_resource(registry);

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), D::new(1, &drops)))
        .id();

    for i in 0..5 {
        app.insert_resource(StoreFor(RepliconTick::new(i)));
        app.update();
        app.world_mut()
            .entity_mut(e1)
            .get_mut::<D>()
            .unwrap()
            .set_changed();
    }
    app.world_mut().entity_mut(e1).get_mut::<D>().unwrap().0 += 1;

    assert_drops(&drops, []);

    app.world_mut().despawn(e1);

    assert_drops(&drops, [2, 1]);
}

#[test]
fn drop_once_out_of_bounds() {
    let mut app = init_app();
    let drops = DropList::default();

    let mut registry = RollbackRegistry::default();
    registry.register::<D>(app.world_mut());
    app.insert_resource(registry);

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), D::new(1, &drops)))
        .id();

    app.insert_resource(StoreFor(RepliconTick::new(10)));
    app.update();
    app.world_mut().entity_mut(e1).get_mut::<D>().unwrap().0 += 1;

    app.insert_resource(StoreFor(RepliconTick::new(2)));
    app.update();

    assert_drops(&drops, []);

    app.world_mut().despawn(e1);

    assert_drops(&drops, [2, 1]);
}

#[test]
fn marks_removed_on_predicted_archetype_change() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1), B))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    registry.register::<B>(app.world_mut());
    app.insert_resource(registry);

    app.insert_resource(StoreFor(RepliconTick::new(0)));
    app.update();

    app.world_mut().entity_mut(e1).remove::<A>();
    app.insert_resource(StoreFor(RepliconTick::new(1)));
    app.update();

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let hist = world.entity(e1).get::<PredictedHistory>().unwrap();
    assert_eq!(
        Removed,
        hist.get(&comp_a).unwrap().get(1).deref::<A>().cloned()
    );
}

#[test]
fn skips_future_history_on_predicted_archetype_change() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1), B))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    registry.register::<B>(app.world_mut());
    app.insert_resource(registry);

    app.insert_resource(StoreFor(RepliconTick::new(5)));
    app.update();

    app.world_mut().entity_mut(e1).remove::<A>();
    app.insert_resource(StoreFor(RepliconTick::new(3)));
    app.update();

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let hist = world.entity(e1).get::<PredictedHistory>().unwrap();
    assert_eq!(a(1), hist.get(&comp_a).unwrap().get(5).deref().cloned());
    assert_eq!(1, hist.get(&comp_a).unwrap().stored_items());
}

#[test]
fn skips_future_history_when_all_predicted_removed() {
    let mut app = init_app();

    let e1 = app
        .world_mut()
        .spawn((Predicted, PredictedHistory::default(), A(1)))
        .id();

    let mut registry = RollbackRegistry::default();
    registry.register::<A>(app.world_mut());
    app.insert_resource(registry);

    app.insert_resource(StoreFor(RepliconTick::new(5)));
    app.update();

    app.world_mut().entity_mut(e1).remove::<A>();
    app.insert_resource(StoreFor(RepliconTick::new(3)));
    app.update();

    let world = app.world_mut();
    let comp_a = world.register_component::<A>();
    let hist = world.entity(e1).get::<PredictedHistory>().unwrap();
    assert_eq!(a(1), hist.get(&comp_a).unwrap().get(5).deref().cloned());
    assert_eq!(1, hist.get(&comp_a).unwrap().stored_items());
}
