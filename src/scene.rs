use bevy::asset::RenderAssetUsages;
use bevy::camera::ClearColorConfig;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::config::{VIEW_PADDING, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::model::spawn_3d_asset_showcase;
use crate::soft_terminal::SoftTerminal;

#[derive(Resource, Clone, Copy)]
pub struct TerminalViewport {
    pub size: Vec2,
    pub center: Vec2,
}

pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut soft_terminal: NonSendMut<SoftTerminal>,
) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
    ));
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        Projection::Orthographic(OrthographicProjection {
            near: -2000.0,
            far: 2000.0,
            ..OrthographicProjection::default_3d()
        }),
        Transform::from_xyz(0.0, 0.0, 800.0).looking_at(Vec3::ZERO, Vec3::Y),
    ));

    let pixmap_width = soft_terminal.terminal.backend().get_pixmap_width() as u32;
    let pixmap_height = soft_terminal.terminal.backend().get_pixmap_height() as u32;

    let mut image = Image::new_fill(
        Extent3d {
            width: pixmap_width,
            height: pixmap_height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.data = Some(soft_terminal.terminal.backend().get_pixmap_data_as_rgba());
    image.sampler = ImageSampler::nearest();

    let image_handle = images.add(image);
    soft_terminal.image_handle = Some(image_handle.clone());

    let viewport_size = Vec2::new(
        WINDOW_WIDTH - 2.0 * VIEW_PADDING,
        WINDOW_HEIGHT - 2.0 * VIEW_PADDING,
    );
    let viewport_center = Vec2::ZERO;
    commands.insert_resource(TerminalViewport {
        size: viewport_size,
        center: viewport_center,
    });

    let mut sprite = Sprite::from_image(image_handle);
    sprite.custom_size = Some(viewport_size);
    commands.spawn((
        sprite,
        Transform::from_translation(Vec3::new(viewport_center.x, viewport_center.y, 0.0)),
    ));

    commands.spawn((
        PointLight {
            intensity: 90_000.0,
            range: 1600.0,
            shadows_enabled: true,
            ..default()
        },
        Transform::from_xyz(0.0, 260.0, 900.0),
    ));

    spawn_3d_asset_showcase(&mut commands, &mut meshes, &mut materials);
}
