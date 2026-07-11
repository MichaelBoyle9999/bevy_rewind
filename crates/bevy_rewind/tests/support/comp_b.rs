//! The shared `B` test component: a simple component without a value.

use bevy::prelude::*;

/// A simple component without a value
#[derive(Component, Clone, PartialEq, Eq, Debug)]
pub struct B;
