use bevy::prelude::*;

#[derive(Component, Clone, PartialEq, Eq, Deref, DerefMut, Debug)]
pub struct A(pub u16);
