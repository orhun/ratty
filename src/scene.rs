use bevy::asset::RenderAssetUsages;
use bevy::camera::ClearColorConfig;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::config::{WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::terminal::TerminalSurface;

#[derive(Component)]
pub struct TerminalSprite;

#[derive(Component)]
pub struct TerminalPlane;

#[derive(Component)]
pub struct TerminalPlaneBack;

#[derive(Component)]
pub struct TerminalPlaneCamera;

#[derive(Resource, Clone, Copy)]
pub struct TerminalViewport {
    pub size: Vec2,
    pub center: Vec2,
}

#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPresentationMode {
    Flat2d,
    Plane3d,
}

#[derive(Resource)]
pub struct TerminalPresentation {
    pub mode: TerminalPresentationMode,
}

impl TerminalPresentation {
    pub fn toggle(&mut self) {
        self.mode = match self.mode {
            TerminalPresentationMode::Flat2d => TerminalPresentationMode::Plane3d,
            TerminalPresentationMode::Plane3d => TerminalPresentationMode::Flat2d,
        };
    }
}

#[derive(Resource)]
pub struct TerminalPlaneView {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub camera_offset: Vec2,
    pub rotating: bool,
    pub panning: bool,
    pub last_rotate_cursor: Option<Vec2>,
    pub last_pan_cursor: Option<Vec2>,
}

impl Default for TerminalPlaneView {
    fn default() -> Self {
        Self {
            yaw: 0.18,
            pitch: 0.08,
            zoom: 1.0,
            camera_offset: Vec2::ZERO,
            rotating: false,
            panning: false,
            last_rotate_cursor: None,
            last_pan_cursor: None,
        }
    }
}

#[derive(Resource)]
pub struct ModelLoadState {
    pub loaded: bool,
}

pub fn setup_scene(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
    mut terminal: NonSendMut<TerminalSurface>,
) {
    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
    ));
    commands.spawn((
        TerminalPlaneCamera,
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

    let pixmap_width = terminal.tui.backend().get_pixmap_width() as u32;
    let pixmap_height = terminal.tui.backend().get_pixmap_height() as u32;

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
    image.data = Some(terminal.tui.backend().get_pixmap_data_as_rgba());
    image.sampler = ImageSampler::nearest();

    let image_handle = images.add(image);
    terminal.image_handle = Some(image_handle.clone());

    let viewport_size = Vec2::new(WINDOW_WIDTH, WINDOW_HEIGHT);
    let viewport_center = Vec2::ZERO;
    commands.insert_resource(TerminalViewport {
        size: viewport_size,
        center: viewport_center,
    });

    let mut sprite = Sprite::from_image(image_handle);
    sprite.custom_size = Some(viewport_size);
    commands.spawn((
        TerminalSprite,
        sprite,
        Transform::from_translation(Vec3::new(viewport_center.x, viewport_center.y, 0.0)),
    ));

    commands.spawn((
        TerminalPlane,
        Mesh3d(meshes.add(Rectangle::new(1.0, 1.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::WHITE,
            base_color_texture: terminal.image_handle.clone(),
            unlit: true,
            ..default()
        })),
        Transform::from_scale(viewport_size.extend(1.0)),
        Visibility::Hidden,
    ));

    commands.spawn((
        TerminalPlaneBack,
        Mesh3d(meshes.add(Rectangle::new(1.0, 1.0))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb_u8(31, 31, 40),
            unlit: true,
            ..default()
        })),
        Transform {
            translation: Vec3::new(0.0, 0.0, -2.0),
            rotation: Quat::from_rotation_y(std::f32::consts::PI),
            scale: viewport_size.extend(1.0),
        },
        Visibility::Hidden,
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
    commands.insert_resource(TerminalPresentation {
        mode: TerminalPresentationMode::Flat2d,
    });
    commands.insert_resource(TerminalPlaneView::default());
    commands.insert_resource(ModelLoadState { loaded: false });
}

pub fn apply_terminal_presentation(
    presentation: Res<TerminalPresentation>,
    plane_view: Res<TerminalPlaneView>,
    mut visibility_queries: ParamSet<(
        Query<&mut Visibility, With<TerminalSprite>>,
        Query<&mut Visibility, With<TerminalPlane>>,
        Query<&mut Visibility, With<TerminalPlaneBack>>,
    )>,
    mut plane_transforms: ParamSet<(
        Query<&mut Transform, With<TerminalPlane>>,
        Query<&mut Transform, With<TerminalPlaneBack>>,
        Query<(&mut Projection, &mut Transform), With<TerminalPlaneCamera>>,
    )>,
) {
    let sprite_visibility = match presentation.mode {
        TerminalPresentationMode::Flat2d => Visibility::Visible,
        TerminalPresentationMode::Plane3d => Visibility::Hidden,
    };
    let plane_visibility = match presentation.mode {
        TerminalPresentationMode::Flat2d => Visibility::Hidden,
        TerminalPresentationMode::Plane3d => Visibility::Visible,
    };

    for mut visibility in &mut visibility_queries.p0() {
        *visibility = sprite_visibility;
    }

    for mut visibility in &mut visibility_queries.p1() {
        *visibility = plane_visibility;
    }

    for mut visibility in &mut visibility_queries.p2() {
        *visibility = plane_visibility;
    }

    for mut transform in &mut plane_transforms.p0() {
        transform.rotation = if presentation.mode == TerminalPresentationMode::Plane3d {
            Quat::from_euler(EulerRot::XYZ, plane_view.pitch, plane_view.yaw, 0.0)
        } else {
            Quat::IDENTITY
        };
    }

    for mut transform in &mut plane_transforms.p1() {
        transform.rotation = if presentation.mode == TerminalPresentationMode::Plane3d {
            Quat::from_euler(
                EulerRot::XYZ,
                plane_view.pitch,
                plane_view.yaw + std::f32::consts::PI,
                0.0,
            )
        } else {
            Quat::IDENTITY
        };
    }

    for (mut projection, mut transform) in &mut plane_transforms.p2() {
        if let Projection::Orthographic(ortho) = projection.as_mut() {
            ortho.scale = match presentation.mode {
                TerminalPresentationMode::Flat2d => 1.0,
                TerminalPresentationMode::Plane3d => plane_view.zoom,
            };
        }

        let offset = match presentation.mode {
            TerminalPresentationMode::Flat2d => Vec3::ZERO,
            TerminalPresentationMode::Plane3d => plane_view.camera_offset.extend(0.0),
        };
        transform.translation = Vec3::new(0.0, 0.0, 800.0) + offset;
        transform.look_at(offset, Vec3::Y);
    }
}
