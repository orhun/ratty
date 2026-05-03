//! Scene setup and presentation resources.

use bevy::asset::RenderAssetUsages;
use bevy::camera::ClearColorConfig;
use bevy::ecs::query::With;
use bevy::ecs::system::SystemParam;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::config::AppConfig;
use crate::terminal::TerminalSurface;

/// Marker for the 2D terminal sprite.
#[derive(Component)]
pub struct TerminalSprite;

/// Marker for the front 3D terminal plane.
#[derive(Component)]
pub struct TerminalPlane;

/// Marker for the back 3D terminal plane.
#[derive(Component)]
pub struct TerminalPlaneBack;

/// Marker for the 3D presentation camera.
#[derive(Component)]
pub struct TerminalPlaneCamera;

/// Handles for terminal plane meshes.
#[derive(Resource)]
pub struct TerminalPlaneMeshes {
    /// Front plane mesh.
    pub front: Handle<Mesh>,
    /// Back plane mesh.
    pub back: Handle<Mesh>,
}

/// Plane warp state.
#[derive(Resource, Default)]
pub struct TerminalPlaneWarp {
    /// Warp amount.
    pub amount: f32,
}

impl TerminalPlaneWarp {
    /// Adjusts the warp amount.
    pub fn adjust(&mut self, delta: f32) {
        self.amount = (self.amount + delta).clamp(0.0, 1.0);
    }
}

/// Terminal viewport geometry.
#[derive(Resource, Clone, Copy)]
pub struct TerminalViewport {
    /// Viewport size in logical pixels.
    pub size: Vec2,
    /// Viewport center in world space.
    pub center: Vec2,
}

/// Terminal presentation mode.
#[derive(Resource, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPresentationMode {
    /// Flat 2D presentation.
    Flat2d,
    /// Warped 3D presentation.
    Plane3d,
}

/// Active terminal presentation.
#[derive(Resource)]
pub struct TerminalPresentation {
    /// Current presentation mode.
    pub mode: TerminalPresentationMode,
}

impl TerminalPresentation {
    /// Toggles the presentation mode.
    pub fn toggle(&mut self) {
        self.mode = match self.mode {
            TerminalPresentationMode::Flat2d => TerminalPresentationMode::Plane3d,
            TerminalPresentationMode::Plane3d => TerminalPresentationMode::Flat2d,
        };
    }
}

/// Camera state for 3D presentation.
#[derive(Resource)]
pub struct TerminalPlaneView {
    /// Camera yaw.
    pub yaw: f32,
    /// Camera pitch.
    pub pitch: f32,
    /// Camera zoom factor.
    pub zoom: f32,
    /// Camera pan offset.
    pub camera_offset: Vec2,
    /// Indicates drag rotation.
    pub rotating: bool,
    /// Indicates drag panning.
    pub panning: bool,
    /// Last rotation cursor position.
    pub last_rotate_cursor: Option<Vec2>,
    /// Last pan cursor position.
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

/// Model loading state.
#[derive(Resource)]
pub struct ModelLoadState {
    /// Indicates the scene has loaded models.
    pub loaded: bool,
    /// Indicates the first terminal frame was uploaded.
    pub first_frame_uploaded: bool,
}

type SpriteVisibilityQuery<'w, 's> = Query<'w, 's, &'static mut Visibility, With<TerminalSprite>>;
type PlaneVisibilityQuery<'w, 's> = Query<'w, 's, &'static mut Visibility, With<TerminalPlane>>;
type PlaneBackVisibilityQuery<'w, 's> =
    Query<'w, 's, &'static mut Visibility, With<TerminalPlaneBack>>;
type PlaneTransformQuery<'w, 's> = Query<'w, 's, &'static mut Transform, With<TerminalPlane>>;
type PlaneBackTransformQuery<'w, 's> =
    Query<'w, 's, &'static mut Transform, With<TerminalPlaneBack>>;
type PlaneCameraQuery<'w, 's> =
    Query<'w, 's, (&'static mut Projection, &'static mut Transform), With<TerminalPlaneCamera>>;

#[derive(SystemParam)]
pub(crate) struct PresentationParams<'w, 's> {
    visibility_queries: ParamSet<
        'w,
        's,
        (
            SpriteVisibilityQuery<'w, 's>,
            PlaneVisibilityQuery<'w, 's>,
            PlaneBackVisibilityQuery<'w, 's>,
        ),
    >,
    plane_transforms: ParamSet<
        'w,
        's,
        (
            PlaneTransformQuery<'w, 's>,
            PlaneBackTransformQuery<'w, 's>,
            PlaneCameraQuery<'w, 's>,
        ),
    >,
}

/// Sets up the terminal presentation scene.
///
/// This startup system creates the 2D and 3D cameras, terminal sprite, terminal plane meshes,
/// backing images, lighting and presentation resources used by later update systems.
pub fn setup_scene(
    mut commands: Commands,
    app_config: Res<AppConfig>,
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
        Msaa::Off,
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
        Msaa::Off,
    ));

    let pixmap = terminal.pixmap_dimensions();
    let pixmap_width = pixmap.x;
    let pixmap_height = pixmap.y;

    let mut image = create_terminal_image(pixmap_width, pixmap_height, [0, 0, 0, 255]);
    image.data = Some(vec![0; (pixmap_width * pixmap_height * 4) as usize]);

    let image_handle = images.add(image);
    terminal.image_handle = Some(image_handle.clone());

    let [r, g, b] = app_config.theme.background;
    let back_image = create_terminal_image(
        pixmap_width,
        pixmap_height,
        [
            r.saturating_sub(13),
            g.saturating_sub(11),
            b.saturating_sub(3),
            255,
        ],
    );
    let back_image_handle = images.add(back_image);
    terminal.back_image_handle = Some(back_image_handle.clone());

    let viewport_size = Vec2::new(
        app_config.window.width as f32,
        app_config.window.height as f32,
    );
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

    let front_mesh = meshes.add(terminal_plane_mesh(32, 20));
    let back_mesh = meshes.add(terminal_plane_mesh(32, 20));
    commands.insert_resource(TerminalPlaneMeshes {
        front: front_mesh.clone(),
        back: back_mesh.clone(),
    });
    commands.insert_resource(TerminalPlaneWarp::default());

    commands.spawn((
        TerminalPlane,
        Mesh3d(front_mesh),
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
        Mesh3d(back_mesh),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::WHITE,
            base_color_texture: terminal.back_image_handle.clone(),
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
            intensity: 190_000.0,
            range: 2200.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(220.0, 320.0, 1000.0),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 15_000.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::ZYX, 0.0, -0.9, -0.45)),
    ));
    commands.spawn((
        PointLight {
            intensity: 45_000.0,
            range: 1800.0,
            shadows_enabled: false,
            ..default()
        },
        Transform::from_xyz(-280.0, -120.0, 700.0),
    ));
    commands.insert_resource(TerminalPresentation {
        mode: TerminalPresentationMode::Flat2d,
    });
    commands.insert_resource(TerminalPlaneView::default());
    commands.insert_resource(ModelLoadState {
        loaded: false,
        first_frame_uploaded: false,
    });
}

fn create_terminal_image(width: u32, height: u32, fill: [u8; 4]) -> Image {
    let mut image = Image::new_fill(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &fill,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::default(),
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Applies the active terminal presentation mode.
pub(crate) fn apply_terminal_presentation(
    presentation: Res<TerminalPresentation>,
    plane_view: Res<TerminalPlaneView>,
    mut params: PresentationParams,
) {
    let PresentationParams {
        visibility_queries,
        plane_transforms,
    } = &mut params;
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

fn terminal_plane_mesh(x_segments: u32, y_segments: u32) -> Mesh {
    let x_segments = x_segments.max(2);
    let y_segments = y_segments.max(2);
    let vertex_count = ((x_segments + 1) * (y_segments + 1)) as usize;

    let mut positions = Vec::with_capacity(vertex_count);
    let mut normals = Vec::with_capacity(vertex_count);
    let mut uvs = Vec::with_capacity(vertex_count);
    let mut indices = Vec::with_capacity((x_segments * y_segments * 6) as usize);

    for y in 0..=y_segments {
        let v = y as f32 / y_segments as f32;
        let py = 0.5 - v;
        for x in 0..=x_segments {
            let u = x as f32 / x_segments as f32;
            let px = u - 0.5;
            positions.push([px, py, 0.0]);
            normals.push([0.0, 0.0, 1.0]);
            uvs.push([u, v]);
        }
    }

    for y in 0..y_segments {
        for x in 0..x_segments {
            let row = y * (x_segments + 1);
            let next_row = (y + 1) * (x_segments + 1);
            let i0 = row + x;
            let i1 = i0 + 1;
            let i2 = next_row + x;
            let i3 = i2 + 1;
            indices.extend_from_slice(&[i0, i2, i1, i1, i2, i3]);
        }
    }

    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_POSITION, positions)
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, normals)
    .with_inserted_attribute(Mesh::ATTRIBUTE_UV_0, uvs)
    .with_inserted_indices(Indices::U32(indices))
}
