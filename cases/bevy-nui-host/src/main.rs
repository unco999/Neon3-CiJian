use bevy::prelude::*;
use neon3_bevy_nui_host::{Neon3BevyPlugin, Neon3HostObject, Neon3WalkableCharacter, Neon3WorldUi};

fn main() {
    App::new()
        .add_plugins(DefaultPlugins)
        .add_plugins(Neon3BevyPlugin::default())
        .add_systems(Startup, setup_case)
        .add_systems(Update, walk_character)
        .run();
}

fn setup_case(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.spawn((
        Camera3d::default(),
        Transform::from_xyz(0.0, 2.4, 6.0).looking_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y),
    ));
    commands.spawn((
        PointLight {
            shadow_maps_enabled: true,
            intensity: 1500.0,
            ..default()
        },
        Transform::from_xyz(4.0, 6.0, 4.0),
    ));
    commands.spawn((
        Mesh3d(meshes.add(Cuboid::new(12.0, 0.1, 12.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.08, 0.1, 0.12),
            ..default()
        })),
        Transform::from_xyz(0.0, -0.05, 0.0),
    ));

    commands.spawn((
        WorldAssetRoot(asset_server.load(GltfAssetLabel::Scene(0).from_asset("character.glb"))),
        Transform::from_xyz(0.0, 0.0, 0.0),
        Neon3WalkableCharacter,
        Neon3HostObject {
            object_id: "player.main".into(),
        },
        Neon3WorldUi {
            surface_id: "character.player.main.status".into(),
            billboard: true,
        },
    ));
}

fn walk_character(time: Res<Time>, keyboard: Res<ButtonInput<KeyCode>>, mut query: Query<&mut Transform, With<Neon3WalkableCharacter>>) {
    let mut direction = Vec3::ZERO;
    if keyboard.pressed(KeyCode::KeyW) { direction.z -= 1.0; }
    if keyboard.pressed(KeyCode::KeyS) { direction.z += 1.0; }
    if keyboard.pressed(KeyCode::KeyA) { direction.x -= 1.0; }
    if keyboard.pressed(KeyCode::KeyD) { direction.x += 1.0; }
    if direction == Vec3::ZERO { return; }
    let direction = direction.normalize();
    for mut transform in &mut query {
        transform.translation += direction * time.delta_secs() * 2.0;
        transform.rotation = Quat::from_rotation_y(direction.x.atan2(-direction.z));
    }
}
