use crate::simulation::*;

use avian3d::{
    dynamics::solver::constraint_graph::ConstraintGraph, physics_transform::PhysicsTransformConfig,
    prelude::*,
};
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use bevy_rewind::*;

pub fn avian_plugin(app: &mut App) {
    app.add_plugins(
        PhysicsPlugins::new(SimulationPostUpdate)
            .build()
            .disable::<IslandPlugin>()
            .disable::<IslandSleepingPlugin>(),
    )
    .insert_resource(PhysicsTransformConfig {
        propagate_before_physics: false,
        transform_to_position: true,
        position_to_transform: false,
        transform_to_collider_scale: false,
    })
    .replicate::<Position>()
    .replicate::<Rotation>()
    .replicate::<LinearVelocity>()
    .replicate::<AngularVelocity>()
    // Set up rollback on avian components/resources
    .register_authoritative_component::<Position>()
    .register_authoritative_component::<Rotation>()
    .register_authoritative_component::<LinearVelocity>()
    .register_authoritative_component::<AngularVelocity>()
    .register_predicted_resource::<ContactGraph>()
    .register_predicted_resource::<ConstraintGraph>()
    .add_systems(
        RollbackSchedule::Rollback,
        (|mut commands: Commands,
          col: Option<Res<ContactGraph>>,
          cons: Option<Res<ConstraintGraph>>| {
            if col.is_none() {
                commands.init_resource::<ContactGraph>();
                commands.insert_resource(ResourceHistory::<ContactGraph>::default());
            }
            if cons.is_none() {
                commands.init_resource::<ConstraintGraph>();
                commands.insert_resource(ResourceHistory::<ConstraintGraph>::default());
            }
        })
        .after(RollbackLoadSet),
    )
    .add_systems(
        bevy::app::RunFixedMainLoop,
        position_to_transform.in_set(bevy::app::RunFixedMainLoopSystems::AfterFixedMainLoop),
    );
}

fn position_to_transform(
    mut query: Query<
        (&mut Transform, &Position, &Rotation),
        Or<(Added<Transform>, Changed<Position>, Changed<Rotation>)>,
    >,
) {
    for (mut transform, pos, rot) in query.iter_mut() {
        transform.translation = **pos;
        transform.rotation = **rot;
    }
}
