//! Scene setup and presentation resources.

mod mobius;

pub use mobius::{MobiusEnterZoomFloor, MobiusTransition, MobiusTransitionDirection};

use bevy::asset::RenderAssetUsages;
use bevy::camera::ClearColorConfig;
use bevy::ecs::query::With;
use bevy::ecs::system::SystemParam;
use bevy::image::ImageSampler;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, Face, TextureDimension, TextureFormat};
use bevy::window::PrimaryWindow;

use bevy::camera::visibility::NoFrustumCulling;

use crate::camera::{
    MIN_ORTHOGRAPHIC_SCALE, TerminalCameraInteraction, TerminalCameraPreset, TerminalCameraSlots,
};
use crate::config::AppConfig;
use crate::direct_render::{new_terminal_image, new_terminal_render_image};
use crate::present::{TerminalPresentMaterial, fullscreen_quad};
use crate::runtime::TerminalRuntime;
use crate::terminal::{TerminalLayout, TerminalSurface, render_scale_for_window};

const TERMINAL_PERSPECTIVE_NEAR: f32 = 0.1;
const TERMINAL_PERSPECTIVE_FAR: f32 = 10_000.0;

/// Marker for the 2D terminal sprite.
#[derive(Component)]
pub struct TerminalSprite;

/// Marker for the front 3D terminal plane.
#[derive(Component)]
pub struct TerminalPlane;

/// Marker for the back 3D terminal plane.
#[derive(Component)]
pub struct TerminalPlaneBack;

/// Marker for the orthographic 3D presentation camera.
#[derive(Component)]
pub struct TerminalOrthographicCamera;

/// Marker for the perspective 3D presentation camera.
#[derive(Component)]
pub struct TerminalPerspectiveCamera;

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TerminalPresentationMode {
    /// Flat 2D presentation.
    #[default]
    Flat2d,
    /// Warped 3D presentation.
    Plane3d,
    /// Perspective 3D presentation.
    Perspective3d,
    /// Mobius-strip 3D presentation.
    Mobius3d,
}

impl TerminalPresentationMode {
    /// Returns whether the mode uses the 3D presentation camera and terminal plane.
    pub const fn is_3d(self) -> bool {
        !matches!(self, Self::Flat2d)
    }

    /// Returns whether the mode uses the Mobius-strip terminal surface.
    pub const fn is_mobius(self) -> bool {
        matches!(self, Self::Mobius3d)
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
type PlaneMaterialQuery<'w, 's> =
    Query<'w, 's, &'static MeshMaterial3d<StandardMaterial>, With<TerminalPlane>>;
type PlaneTransformQuery<'w, 's> = Query<'w, 's, &'static mut Transform, With<TerminalPlane>>;
type PlaneBackTransformQuery<'w, 's> =
    Query<'w, 's, &'static mut Transform, With<TerminalPlaneBack>>;
type OrthographicCameraQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Projection,
        &'static mut Transform,
        &'static mut Camera,
    ),
    (
        With<TerminalOrthographicCamera>,
        Without<TerminalPerspectiveCamera>,
        Without<Camera2d>,
        Without<TerminalPlane>,
        Without<TerminalPlaneBack>,
    ),
>;
type PerspectiveCameraQuery<'w, 's> = Query<
    'w,
    's,
    (
        &'static mut Projection,
        &'static mut Transform,
        &'static mut Camera,
    ),
    (
        With<TerminalPerspectiveCamera>,
        Without<TerminalOrthographicCamera>,
        Without<Camera2d>,
        Without<TerminalPlane>,
        Without<TerminalPlaneBack>,
    ),
>;
type FlatCameraQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Camera,
    (
        With<Camera2d>,
        Without<TerminalOrthographicCamera>,
        Without<TerminalPerspectiveCamera>,
    ),
>;
pub(crate) type TerminalPlaneLayoutQuery<'w, 's> =
    Query<'w, 's, &'static mut Transform, (With<TerminalPlane>, Without<TerminalSprite>)>;
pub(crate) type TerminalPlaneBackLayoutQuery<'w, 's> = Query<
    'w,
    's,
    &'static mut Transform,
    (
        With<TerminalPlaneBack>,
        Without<TerminalPlane>,
        Without<TerminalSprite>,
    ),
>;

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
    plane_materials: PlaneMaterialQuery<'w, 's>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    plane_transforms:
        ParamSet<'w, 's, (PlaneTransformQuery<'w, 's>, PlaneBackTransformQuery<'w, 's>)>,
    camera_2d: FlatCameraQuery<'w, 's>,
    orthographic_camera: OrthographicCameraQuery<'w, 's>,
    perspective_camera: PerspectiveCameraQuery<'w, 's>,
}

#[derive(SystemParam)]
pub(crate) struct SetupSceneParams<'w, 's> {
    commands: Commands<'w, 's>,
    app_config: Res<'w, AppConfig>,
    meshes: ResMut<'w, Assets<Mesh>>,
    materials: ResMut<'w, Assets<StandardMaterial>>,
    images: ResMut<'w, Assets<Image>>,
    present_materials: ResMut<'w, Assets<TerminalPresentMaterial>>,
    primary_window: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    runtime: ResMut<'w, TerminalRuntime>,
    terminal: ResMut<'w, TerminalSurface>,
}

/// Sets up the terminal presentation scene.
///
/// This startup system creates the 2D and 3D cameras, terminal sprite, terminal plane meshes,
/// backing images, lighting and presentation resources used by later update systems.
pub(crate) fn setup_scene(mut params: SetupSceneParams) {
    let SetupSceneParams {
        commands,
        app_config,
        meshes,
        materials,
        images,
        present_materials,
        primary_window,
        runtime,
        terminal,
    } = &mut params;
    let terminal_opacity = app_config.window.opacity.clamp(0.0, 1.0);
    let window = primary_window.single().expect("primary window");
    let window_size = window.resolution.size().max(Vec2::ONE);
    let render_scale = render_scale_for_window(window);
    let layout = terminal.resize_to_fit(window_size, render_scale);
    let pty_pixels = layout.pty_pixels();
    runtime.resize(
        layout.cols,
        layout.rows,
        pty_pixels.x as u16,
        pty_pixels.y as u16,
    );

    commands.spawn((
        Camera2d,
        Camera {
            order: 0,
            ..default()
        },
        Msaa::Off,
    ));
    commands.spawn((
        TerminalPerspectiveCamera,
        Camera3d::default(),
        Camera {
            order: 1,
            clear_color: ClearColorConfig::None,
            ..default()
        },
        Projection::Perspective(PerspectiveProjection {
            near: TERMINAL_PERSPECTIVE_NEAR,
            far: TERMINAL_PERSPECTIVE_FAR,
            ..PerspectiveProjection::default()
        }),
        Transform::from_xyz(0.0, 0.0, 800.0).looking_at(Vec3::ZERO, Vec3::Y),
        Msaa::Off,
    ));
    commands.spawn((
        TerminalOrthographicCamera,
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

    let terminal_alpha = (terminal_opacity * 255.0).round() as u8;
    let render_image_handle = images.add(new_terminal_render_image(
        layout.texture_size.x,
        layout.texture_size.y,
        crate::config::TERMINAL_RENDER_TEXTURE_LABEL,
    ));
    terminal.render_image_handle = Some(render_image_handle);

    let image_handle = images.add(new_terminal_image(
        layout.texture_size.x,
        layout.texture_size.y,
        crate::config::TERMINAL_TEXTURE_LABEL,
    ));
    terminal.image_handle = Some(image_handle.clone());

    let [r, g, b] = app_config.theme.background;
    let back_image = create_terminal_image(
        layout.texture_size.x,
        layout.texture_size.y,
        [
            r.saturating_sub(13),
            g.saturating_sub(11),
            b.saturating_sub(3),
            terminal_alpha,
        ],
    );
    let back_image_handle = images.add(back_image);
    terminal.back_image_handle = Some(back_image_handle.clone());

    let viewport_center = Vec2::ZERO;
    commands.insert_resource(TerminalViewport {
        size: layout.logical_size,
        center: viewport_center,
    });

    // Present the terminal texture 1:1 with physical pixels via a fullscreen quad
    // whose shader fetches each texel by pixel coordinate (no resampling), rather
    // than a world-positioned sprite whose interpolated UVs resample it. The
    // `TerminalSprite` marker is kept so the flat/3D visibility toggle applies.
    commands.spawn((
        TerminalSprite,
        Mesh2d(meshes.add(fullscreen_quad())),
        MeshMaterial2d(present_materials.add(TerminalPresentMaterial {
            texture: image_handle,
        })),
        Transform::default(),
        Visibility::Visible,
        NoFrustumCulling,
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
            base_color: Color::srgba(1.0, 1.0, 1.0, terminal_opacity),
            base_color_texture: terminal.image_handle.clone(),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        })),
        Transform::from_scale(layout.logical_size.extend(1.0)),
        Visibility::Hidden,
        NoFrustumCulling,
    ));

    commands.spawn((
        TerminalPlaneBack,
        Mesh3d(back_mesh),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgba(1.0, 1.0, 1.0, terminal_opacity),
            base_color_texture: terminal.back_image_handle.clone(),
            alpha_mode: AlphaMode::Blend,
            unlit: true,
            ..default()
        })),
        Transform {
            translation: Vec3::new(0.0, 0.0, -2.0),
            rotation: Quat::from_rotation_y(std::f32::consts::PI),
            scale: layout.logical_size.extend(1.0),
        },
        Visibility::Hidden,
        NoFrustumCulling,
    ));

    commands.spawn((
        PointLight {
            intensity: 190_000.0,
            range: 2200.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(220.0, 320.0, 1000.0),
    ));
    commands.spawn((
        DirectionalLight {
            illuminance: 15_000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_rotation(Quat::from_euler(EulerRot::ZYX, 0.0, -0.9, -0.45)),
    ));
    commands.spawn((
        PointLight {
            intensity: 45_000.0,
            range: 1800.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-280.0, -120.0, 700.0),
    ));
    commands.insert_resource(TerminalCameraSlots::default());
    commands.insert_resource(TerminalCameraInteraction::default());
    commands.insert_resource(MobiusTransition::default());
    commands.insert_resource(ModelLoadState {
        loaded: false,
        first_frame_uploaded: false,
    });
}

/// Synchronizes Bevy presentation entities to the terminal texture layout.
pub(crate) fn sync_terminal_layout(
    layout: TerminalLayout,
    viewport: &mut TerminalViewport,
    plane_query: &mut TerminalPlaneLayoutQuery,
    plane_back_query: &mut TerminalPlaneBackLayoutQuery,
) {
    viewport.size = layout.logical_size;
    viewport.center = Vec2::ZERO;

    for mut transform in plane_query.iter_mut() {
        transform.scale = layout.logical_size.extend(1.0);
    }

    for mut transform in plane_back_query.iter_mut() {
        transform.scale = layout.logical_size.extend(1.0);
    }
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalClearOwner {
    Flat,
    Orthographic,
    Perspective,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TerminalCameraActivation {
    flat: bool,
    orthographic: bool,
    perspective: bool,
    clear_owner: TerminalClearOwner,
}

fn camera_activation(mode: TerminalPresentationMode) -> TerminalCameraActivation {
    match mode {
        TerminalPresentationMode::Flat2d => TerminalCameraActivation {
            flat: true,
            orthographic: true,
            perspective: false,
            clear_owner: TerminalClearOwner::Flat,
        },
        TerminalPresentationMode::Plane3d | TerminalPresentationMode::Mobius3d => {
            TerminalCameraActivation {
                flat: false,
                orthographic: true,
                perspective: false,
                clear_owner: TerminalClearOwner::Orthographic,
            }
        }
        TerminalPresentationMode::Perspective3d => TerminalCameraActivation {
            flat: false,
            orthographic: false,
            perspective: true,
            clear_owner: TerminalClearOwner::Perspective,
        },
    }
}

fn set_camera_state(mut camera: Mut<Camera>, active: bool, owns_clear: bool) {
    if camera.is_active != active {
        camera.is_active = active;
    }
    let clear_matches = if owns_clear {
        matches!(camera.clear_color, ClearColorConfig::Default)
    } else {
        matches!(camera.clear_color, ClearColorConfig::None)
    };
    if !clear_matches {
        camera.clear_color = if owns_clear {
            ClearColorConfig::Default
        } else {
            ClearColorConfig::None
        };
    }
}

/// Composes the orbit rotation applied to the 3D terminal cameras.
///
/// Yaw must stay outermost and pitch inside it (YXZ order) so this matches the
/// inverse of the `Rx(pitch) * Ry(yaw)` plane rotation the fixed camera
/// replaced; with pitch outermost, pitch input degenerates into roll as yaw
/// approaches 90 degrees. Roll is innermost so it stays on the view axis.
fn camera_orbit_rotation(yaw: f32, pitch: f32, roll: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, -yaw, -pitch, roll)
}

/// Synchronizes the active camera preset to the presentation entities.
pub(crate) fn apply_terminal_presentation(
    camera_slots: Res<TerminalCameraSlots>,
    mobius_transition: Res<MobiusTransition>,
    mut last_active_preset: Local<Option<(usize, TerminalCameraPreset)>>,
    mut params: PresentationParams,
) {
    let current = (camera_slots.active_slot(), *camera_slots.active());
    if last_active_preset.as_ref() == Some(&current) && !mobius_transition.is_changed() {
        return;
    }
    *last_active_preset = Some(current);

    let PresentationParams {
        visibility_queries,
        plane_materials,
        materials,
        plane_transforms,
        camera_2d,
        orthographic_camera,
        perspective_camera,
    } = &mut params;
    let preset = camera_slots.active();
    let mode = preset.mode;
    let pose = preset.pose;
    let activation = camera_activation(mode);
    let is_3d = mode.is_3d();
    let is_mobius = mode.is_mobius();
    let yaw = if is_mobius && mobius_transition.active {
        mobius_transition.current_yaw()
    } else {
        pose.yaw
    };
    let pitch = if is_mobius && mobius_transition.active {
        mobius_transition.current_pitch()
    } else {
        pose.pitch
    };
    let roll = if is_mobius && mobius_transition.active {
        mobius_transition.current_roll()
    } else {
        pose.roll
    };
    let translation = if is_mobius && mobius_transition.active {
        mobius_transition.current_translation()
    } else {
        pose.translation
    };
    let sprite_visibility = if is_3d {
        Visibility::Hidden
    } else {
        Visibility::Visible
    };
    let plane_visibility = if is_3d {
        Visibility::Visible
    } else {
        Visibility::Hidden
    };

    for mut visibility in &mut visibility_queries.p0() {
        if *visibility != sprite_visibility {
            *visibility = sprite_visibility;
        }
    }

    for mut visibility in &mut visibility_queries.p1() {
        if *visibility != plane_visibility {
            *visibility = plane_visibility;
        }
    }

    for mut visibility in &mut visibility_queries.p2() {
        // A Mobius strip is one continuous ribbon, so the separate back sheet model does not map
        // cleanly. Render the front material double-sided instead.
        let target = if is_3d && !is_mobius {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
        if *visibility != target {
            *visibility = target;
        }
    }

    if let Ok(front_material) = plane_materials.single() {
        let cull_mode = if is_mobius { None } else { Some(Face::Back) };
        // `get_mut` marks the material modified and re-prepares it on the
        // GPU; only take it when the cull mode actually changes.
        let needs_update = materials
            .get(&front_material.0)
            .is_some_and(|material| material.cull_mode != cull_mode);
        if needs_update && let Some(mut material) = materials.get_mut(&front_material.0) {
            material.cull_mode = cull_mode;
        }
    }

    for camera in camera_2d.iter_mut() {
        set_camera_state(
            camera,
            activation.flat,
            activation.clear_owner == TerminalClearOwner::Flat,
        );
    }
    for mut transform in &mut plane_transforms.p0() {
        if transform.rotation != Quat::IDENTITY {
            transform.rotation = Quat::IDENTITY;
        }
    }

    for mut transform in &mut plane_transforms.p1() {
        let rotation = Quat::from_rotation_y(std::f32::consts::PI);
        let translation = Vec3::new(0.0, 0.0, -2.0);
        if transform.rotation != rotation {
            transform.rotation = rotation;
        }
        if transform.translation != translation {
            transform.translation = translation;
        }
    }

    let camera_translation = if is_3d { translation } else { Vec3::ZERO };
    let camera_distance = (800.0 + camera_translation.z).max(0.1);
    let camera_target = Vec3::new(camera_translation.x, camera_translation.y, 0.0);
    let camera_rotation = if is_3d {
        camera_orbit_rotation(yaw, pitch, roll)
    } else {
        Quat::IDENTITY
    };
    let camera_position = camera_target + camera_rotation * Vec3::Z * camera_distance;
    let camera_transform = Transform {
        translation: camera_position,
        rotation: camera_rotation,
        ..default()
    };

    for (mut projection, mut transform, camera) in orthographic_camera.iter_mut() {
        set_camera_state(
            camera,
            activation.orthographic,
            activation.clear_owner == TerminalClearOwner::Orthographic,
        );
        if !activation.orthographic {
            continue;
        }
        let scale = if is_3d {
            if is_mobius && mobius_transition.active {
                mobius_transition.current_zoom().max(MIN_ORTHOGRAPHIC_SCALE)
            } else {
                pose.resolved_orthographic_scale()
            }
        } else {
            1.0
        };
        let projection_needs_update = matches!(
            &*projection,
            Projection::Orthographic(orthographic) if orthographic.scale != scale
        );
        if projection_needs_update
            && let Projection::Orthographic(orthographic) = projection.as_mut()
        {
            orthographic.scale = scale;
        }
        if *transform != camera_transform {
            *transform = camera_transform;
        }
    }

    for (mut projection, mut transform, camera) in perspective_camera.iter_mut() {
        set_camera_state(
            camera,
            activation.perspective,
            activation.clear_owner == TerminalClearOwner::Perspective,
        );
        if !activation.perspective {
            continue;
        }
        let fov = pose.resolved_perspective_fov();
        let projection_needs_update = matches!(
            &*projection,
            Projection::Perspective(perspective)
                if perspective.fov != fov
                    || perspective.near != TERMINAL_PERSPECTIVE_NEAR
                    || perspective.far != TERMINAL_PERSPECTIVE_FAR
        );
        if projection_needs_update && let Projection::Perspective(perspective) = projection.as_mut()
        {
            perspective.fov = fov;
            perspective.near = TERMINAL_PERSPECTIVE_NEAR;
            perspective.far = TERMINAL_PERSPECTIVE_FAR;
        }
        if *transform != camera_transform {
            *transform = camera_transform;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Resource, Default)]
    struct ChangedCameraCount(usize);

    fn count_changed_cameras(
        cameras: Query<(), Changed<Camera>>,
        mut count: ResMut<ChangedCameraCount>,
    ) {
        count.0 = cameras.iter().count();
    }

    fn presentation_app() -> (App, Entity, Entity, Entity) {
        let mut app = App::new();
        app.init_resource::<TerminalCameraSlots>()
            .init_resource::<MobiusTransition>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<ChangedCameraCount>();
        let material = app
            .world_mut()
            .resource_mut::<Assets<StandardMaterial>>()
            .add(StandardMaterial::default());
        app.world_mut().spawn((TerminalSprite, Visibility::Visible));
        app.world_mut().spawn((
            TerminalPlane,
            MeshMaterial3d(material),
            Transform::default(),
            Visibility::Hidden,
        ));
        app.world_mut()
            .spawn((TerminalPlaneBack, Transform::default(), Visibility::Hidden));
        let flat = app.world_mut().spawn((Camera2d, Camera::default())).id();
        let orthographic = app
            .world_mut()
            .spawn((
                TerminalOrthographicCamera,
                Camera::default(),
                Projection::Orthographic(OrthographicProjection::default_3d()),
                Transform::default(),
            ))
            .id();
        let perspective = app
            .world_mut()
            .spawn((
                TerminalPerspectiveCamera,
                Camera::default(),
                Projection::Perspective(PerspectiveProjection::default()),
                Transform::default(),
            ))
            .id();
        app.add_systems(
            Update,
            (apply_terminal_presentation, count_changed_cameras).chain(),
        );
        (app, flat, orthographic, perspective)
    }

    #[test]
    fn camera_activation_matrix_has_one_clear_owner() {
        let cases = [
            (
                TerminalPresentationMode::Flat2d,
                TerminalCameraActivation {
                    flat: true,
                    orthographic: true,
                    perspective: false,
                    clear_owner: TerminalClearOwner::Flat,
                },
            ),
            (
                TerminalPresentationMode::Plane3d,
                TerminalCameraActivation {
                    flat: false,
                    orthographic: true,
                    perspective: false,
                    clear_owner: TerminalClearOwner::Orthographic,
                },
            ),
            (
                TerminalPresentationMode::Mobius3d,
                TerminalCameraActivation {
                    flat: false,
                    orthographic: true,
                    perspective: false,
                    clear_owner: TerminalClearOwner::Orthographic,
                },
            ),
            (
                TerminalPresentationMode::Perspective3d,
                TerminalCameraActivation {
                    flat: false,
                    orthographic: false,
                    perspective: true,
                    clear_owner: TerminalClearOwner::Perspective,
                },
            ),
        ];

        for (mode, expected) in cases {
            assert_eq!(camera_activation(mode), expected);
            let active_clear_owner = match expected.clear_owner {
                TerminalClearOwner::Flat => expected.flat,
                TerminalClearOwner::Orthographic => expected.orthographic,
                TerminalClearOwner::Perspective => expected.perspective,
            };
            assert!(active_clear_owner);
        }
    }

    #[test]
    fn perspective_depth_range_is_valid() {
        let projection = PerspectiveProjection {
            near: TERMINAL_PERSPECTIVE_NEAR,
            far: TERMINAL_PERSPECTIVE_FAR,
            ..PerspectiveProjection::default()
        };
        assert!(projection.near.is_finite());
        assert!(projection.near > 0.0);
        assert!(projection.far.is_finite());
        assert!(projection.far > projection.near);
    }

    #[test]
    fn presentation_system_applies_activation_and_clear_ownership() {
        let (mut app, flat, orthographic, perspective) = presentation_app();
        let cases = [
            (TerminalPresentationMode::Flat2d, [true, true, false]),
            (TerminalPresentationMode::Plane3d, [false, true, false]),
            (TerminalPresentationMode::Mobius3d, [false, true, false]),
            (
                TerminalPresentationMode::Perspective3d,
                [false, false, true],
            ),
        ];

        for (mode, expected_active) in cases {
            app.world_mut()
                .resource_mut::<TerminalCameraSlots>()
                .active_mut()
                .mode = mode;
            app.update();

            let cameras = [flat, orthographic, perspective].map(|entity| {
                app.world()
                    .entity(entity)
                    .get::<Camera>()
                    .expect("camera entity")
                    .clone()
            });
            assert_eq!(
                cameras.each_ref().map(|camera| camera.is_active),
                expected_active,
                "wrong activation matrix for {mode:?}"
            );
            assert_eq!(
                cameras
                    .iter()
                    .filter(|camera| matches!(camera.clear_color, ClearColorConfig::Default))
                    .count(),
                1,
                "wrong clear ownership for {mode:?}"
            );
        }
    }

    #[test]
    fn orthographic_zoom_out_does_not_collapse_perspective_projection() {
        let (mut app, _, _, perspective) = presentation_app();
        {
            let mut slots = app.world_mut().resource_mut::<TerminalCameraSlots>();
            slots.active_mut().mode = TerminalPresentationMode::Plane3d;
            slots.active_mut().pose.orthographic_scale = 4.0;
        }
        app.update();
        app.world_mut()
            .resource_mut::<TerminalCameraSlots>()
            .active_mut()
            .mode = TerminalPresentationMode::Perspective3d;
        app.update();

        let projection = app
            .world()
            .entity(perspective)
            .get::<Projection>()
            .expect("perspective projection");
        let Projection::Perspective(projection) = projection else {
            panic!("expected perspective projection");
        };
        assert_eq!(projection.fov, 1.0);
        assert!(projection.fov < std::f32::consts::FRAC_PI_2);
    }

    #[test]
    fn orbit_rotation_keeps_pitch_and_yaw_independent() {
        let pitch = 0.5_f32;
        for yaw in [
            0.0_f32,
            std::f32::consts::FRAC_PI_2,
            2.0,
            std::f32::consts::PI,
        ] {
            let rotation = camera_orbit_rotation(yaw, pitch, 0.0);
            let offset = rotation * Vec3::Z;
            assert!(
                (offset.y - pitch.sin()).abs() < 1e-6,
                "pitch must tilt the orbit at yaw {yaw}"
            );
            // The two compositions are analytically identical, but
            // Quat::angle_between is ill-conditioned near zero (acos of a
            // dot product near 1.0 amplifies f32 rounding into ~1e-3 rad),
            // so this pins the composition order, not bit-exact equality.
            let plane_equivalent = Quat::from_euler(EulerRot::XYZ, pitch, yaw, 0.0).inverse();
            assert!(
                rotation.angle_between(plane_equivalent) < 5e-3,
                "orbit must invert the legacy plane rotation at yaw {yaw}"
            );
        }
    }

    #[test]
    fn pose_only_updates_do_not_mark_camera_components_changed() {
        let (mut app, _, _, _) = presentation_app();
        app.update();
        app.world_mut().clear_trackers();
        app.world_mut()
            .resource_mut::<TerminalCameraSlots>()
            .active_mut()
            .pose
            .yaw += 0.25;

        app.update();

        assert_eq!(app.world().resource::<ChangedCameraCount>().0, 0);
    }
}
