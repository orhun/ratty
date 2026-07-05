//! Scene setup and presentation resources.

mod mobius;

pub use mobius::{MobiusTransition, MobiusTransitionDirection};

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

use crate::config::AppConfig;
use crate::direct_render::{new_terminal_image, new_terminal_render_image};
use crate::present::{TerminalPresentMaterial, fullscreen_quad};
use crate::runtime::TerminalRuntime;
use crate::terminal::{TerminalLayout, TerminalSurface, render_scale_for_window};

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

/// Active terminal presentation.
#[derive(Resource)]
pub struct TerminalPresentation {
    /// Current presentation mode.
    pub mode: TerminalPresentationMode,
}

impl TerminalPresentation {
    /// Toggles between the flat and warped 3D terminal views.
    pub fn toggle_plane_mode(&mut self) {
        self.mode = match self.mode {
            TerminalPresentationMode::Flat2d => TerminalPresentationMode::Plane3d,
            TerminalPresentationMode::Plane3d | TerminalPresentationMode::Mobius3d => {
                TerminalPresentationMode::Flat2d
            }
        };
    }

    /// Toggles the Mobius-strip terminal view.
    pub fn toggle_mobius_mode(&mut self) {
        self.mode = match self.mode {
            TerminalPresentationMode::Mobius3d => TerminalPresentationMode::Flat2d,
            TerminalPresentationMode::Flat2d | TerminalPresentationMode::Plane3d => {
                TerminalPresentationMode::Mobius3d
            }
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
type PlaneMaterialQuery<'w, 's> =
    Query<'w, 's, &'static MeshMaterial3d<StandardMaterial>, With<TerminalPlane>>;
type PlaneTransformQuery<'w, 's> = Query<'w, 's, &'static mut Transform, With<TerminalPlane>>;
type PlaneBackTransformQuery<'w, 's> =
    Query<'w, 's, &'static mut Transform, With<TerminalPlaneBack>>;
type PlaneCameraQuery<'w, 's> =
    Query<'w, 's, (&'static mut Projection, &'static mut Transform), With<TerminalPlaneCamera>>;
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
    plane_transforms: ParamSet<
        'w,
        's,
        (
            PlaneTransformQuery<'w, 's>,
            PlaneBackTransformQuery<'w, 's>,
            PlaneCameraQuery<'w, 's>,
        ),
    >,
    camera_2d: Query<'w, 's, &'static mut Camera, (With<Camera2d>, Without<TerminalPlaneCamera>)>,
    camera_3d: Query<'w, 's, &'static mut Camera, (With<TerminalPlaneCamera>, Without<Camera2d>)>,
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
    commands.insert_resource(TerminalPresentation {
        mode: TerminalPresentationMode::Flat2d,
    });
    commands.insert_resource(TerminalPlaneView::default());
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

/// Applies the active terminal presentation mode.
pub(crate) fn apply_terminal_presentation(
    presentation: Res<TerminalPresentation>,
    plane_view: Res<TerminalPlaneView>,
    mobius_transition: Res<MobiusTransition>,
    mut params: PresentationParams,
) {
    let PresentationParams {
        visibility_queries,
        plane_materials,
        materials,
        plane_transforms,
        camera_2d,
        camera_3d,
    } = &mut params;
    let is_3d = presentation.mode.is_3d();
    let is_mobius = presentation.mode.is_mobius();
    let yaw = if is_mobius && mobius_transition.active {
        mobius_transition.current_yaw()
    } else {
        plane_view.yaw
    };
    let pitch = if is_mobius && mobius_transition.active {
        mobius_transition.current_pitch()
    } else {
        plane_view.pitch
    };
    let camera_offset = if is_mobius && mobius_transition.active {
        mobius_transition.current_camera_offset()
    } else {
        plane_view.camera_offset
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
        *visibility = sprite_visibility;
    }

    for mut visibility in &mut visibility_queries.p1() {
        *visibility = plane_visibility;
    }

    for mut visibility in &mut visibility_queries.p2() {
        // A Mobius strip is one continuous ribbon, so the separate back sheet model does not map
        // cleanly. Render the front material double-sided instead.
        *visibility = if is_3d && !is_mobius {
            Visibility::Visible
        } else {
            Visibility::Hidden
        };
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

    // The 2D camera only contributes in flat mode; the 3D camera stays active
    // everywhere because the cursor model and RGP objects render through it
    // even in 2D mode. Whichever camera renders first owns the screen clear.
    for mut camera in camera_2d.iter_mut() {
        let active = !is_3d;
        if camera.is_active != active {
            camera.is_active = active;
        }
    }
    // This system only runs when presentation state changes, so assigning
    // unconditionally does not churn change detection every frame.
    for mut camera in camera_3d.iter_mut() {
        camera.clear_color = if is_3d {
            ClearColorConfig::Default
        } else {
            ClearColorConfig::None
        };
    }

    for mut transform in &mut plane_transforms.p0() {
        transform.rotation = if is_3d {
            Quat::from_euler(EulerRot::XYZ, pitch, yaw, 0.0)
        } else {
            Quat::IDENTITY
        };
    }

    for mut transform in &mut plane_transforms.p1() {
        if is_3d {
            transform.rotation =
                Quat::from_euler(EulerRot::XYZ, pitch, yaw + std::f32::consts::PI, 0.0);
            transform.translation = if is_mobius {
                Vec3::ZERO
            } else {
                Vec3::new(0.0, 0.0, -2.0)
            };
        } else {
            transform.rotation = Quat::IDENTITY;
            transform.translation = Vec3::new(0.0, 0.0, -2.0);
        }
    }

    for (mut projection, mut transform) in &mut plane_transforms.p2() {
        if let Projection::Orthographic(ortho) = projection.as_mut() {
            let zoom = if is_mobius && mobius_transition.active {
                mobius_transition.current_zoom()
            } else {
                plane_view.zoom
            };
            ortho.scale = if is_3d { zoom } else { 1.0 };
        }

        let offset = if is_3d {
            camera_offset.extend(0.0)
        } else {
            Vec3::ZERO
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
