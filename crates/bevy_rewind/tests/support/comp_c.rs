//! The shared `C` test component: a component with multiple fields.

use bevy::prelude::*;

/// A component with multiple fields
#[derive(Component, Clone, PartialEq, Eq, Debug)]
pub struct C(pub u8, pub u16);
