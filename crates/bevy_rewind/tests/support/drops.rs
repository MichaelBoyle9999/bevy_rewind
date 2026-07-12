use std::sync::{Arc, RwLock};

use bevy::{platform::collections::HashSet, prelude::*};

#[derive(Resource, Clone, Deref, DerefMut, Debug, Default)]
pub struct DropList(Arc<RwLock<Drops>>);

#[derive(Clone, Debug, Default)]
pub struct Drops {
    pub present: HashSet<u16>,
    pub order: Vec<u16>,
}

#[track_caller]
pub fn assert_drops(drops: &DropList, order: impl Into<Vec<u16>>) {
    let order = order.into();

    let guard = drops.read().unwrap();
    assert_eq!(order, guard.order);
    assert_eq!(order.len(), guard.present.len());
}

#[derive(Component, Clone, Debug)]
pub struct D(pub u16, DropList);

impl D {
    pub fn new(v: u16, list: &DropList) -> Self {
        Self(v, list.clone())
    }
}

impl PartialEq for D {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl Drop for D {
    fn drop(&mut self) {
        let mut guard = self.1.write().unwrap();
        if guard.present.contains(&self.0) {
            panic!("Detected double drop!");
        }
        guard.present.insert(self.0);
        guard.order.push(self.0);
    }
}
