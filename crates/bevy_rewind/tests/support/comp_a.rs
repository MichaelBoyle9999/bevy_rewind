//! The shared `A` test component: a simple component with a value.

use bevy::prelude::*;

/// A simple component with a value
#[derive(Component, Clone, PartialEq, Eq, Deref, DerefMut, Debug)]
pub struct A(pub u16);
