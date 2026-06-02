//! this module detects cursor movement, and can spawn effects
//! needs work, to not be hard coded -> serialization and configuration.
use bevy::{ecs::system::SystemParam, image::ImageSampler, prelude::*};
use rand::rngs::ThreadRng;
use rand::{RngExt, seq::IndexedRandom};

use crate::{
    config::AppConfig,
    model::CursorModel,
    runtime::TerminalRuntime,
    scene::{
        MobiusTransition, TerminalPlane, TerminalPlaneWarp, TerminalPresentation, TerminalViewport,
    },
    systems::{CursorPoseContext, CursorTransformQuery, active_mobius_progress, cursor_pose},
    terminal::TerminalSurface,
};

#[allow(missing_docs)]
pub struct CursorEffectPlugin;

impl Plugin for CursorEffectPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, setup_cursor_effects);
        app.add_systems(
            Update,
            (
                update_timed_remove,
                update_missiles,
                float_up,
                update_cursor_effects,
                set_texture_filtering,
            ),
        );
    }
}

/// todo: currently hard-coded, missile definitions, should be configurable through files.
fn setup_cursor_effects(mut commands: Commands, asset_server: Res<AssetServer>) {
    let _maplestory = MissileData {
        spawn_sounds: [
            "sounds/swing_1.ogg",
            "sounds/swing_2.ogg",
            "sounds/swing_3.ogg",
        ]
        .map(ToString::to_string)
        .into_iter()
        .collect(),
        hit_sounds: ["sounds/hit_1.ogg", "sounds/hit_2.ogg"]
            .map(ToString::to_string)
            .into_iter()
            .collect(),
        hit_textures: [
            "textures/maple_1.png",
            "textures/maple_10.png",
            "textures/maple_20.png",
            "textures/maple_50.png",
            "textures/maple_70.png",
            "textures/maple_100.png",
            "textures/maple_150.png",
            "textures/maple_777.png",
        ]
        .map(ToString::to_string)
        .into_iter()
        .collect(),
        missile_stats: MissileStats::default(),
        missile_model_or_texture: MissileModelOrTexture::Texture(
            "textures/maple_sword.png".to_string(),
        ),
        missile_visual_transform: MissileVisualTransform {
            euler_rotation: vec3(0.0, 0.0, 45.0),
            scale: 3.0,
        },
        hit_behaviour: HitBehaviour::FloatUp { speed: 100.0 },
    };
    let _runescape = MissileData {
        spawn_sounds: [
            "sounds/swing_1.ogg",
            "sounds/swing_2.ogg",
            "sounds/swing_3.ogg",
        ]
        .map(ToString::to_string)
        .into_iter()
        .collect(),
        hit_sounds: ["sounds/hit_1.ogg", "sounds/hit_2.ogg"]
            .map(ToString::to_string)
            .into_iter()
            .collect(),
        hit_textures: [
            "textures/splat_1.png",
            "textures/splat_2.png",
            "textures/splat_3.png",
            "textures/splat_4.png",
            "textures/splat_5.png",
            "textures/splat_6.png",
            "textures/splat_7.png",
            "textures/splat_8.png",
            "textures/splat_9.png",
        ]
        .map(ToString::to_string)
        .into_iter()
        .collect(),
        missile_stats: MissileStats {
            hit_texture_scale: 3.0,
            ..default()
        },
        missile_model_or_texture: MissileModelOrTexture::Model(
            //     "objects/scimitar/scene.gltf".to_string(),
            "objects/Ferris.glb".to_string(),
        ),
        missile_visual_transform: MissileVisualTransform {
            euler_rotation: Vec3::ZERO,
            scale: 10.0,
        },
        hit_behaviour: HitBehaviour::None,
    };

    // enable/disable these comments to try different effects
    // commands.insert_resource(_runescape.to_runtime(&asset_server));
    commands.insert_resource(_maplestory.to_runtime(&asset_server));
}

#[derive(Clone)]
/// missile initial velocity behaviour definition
pub enum InitialVelocity {
    /// Completely random direction
    Random,
    /// Randomized in a cone arc towards target in euler
    ConeDeg(f32),
}

impl InitialVelocity {
    /// returns a randomized velocity based upon variant
    pub fn get_velocity(&self, start: Vec3, end: Vec3) -> Vec3 {
        let mut rng = rand::rng();
        match self {
            InitialVelocity::Random => random_direction(&mut rng),
            InitialVelocity::ConeDeg(degrees) => {
                let to_target = (end - start).normalize();
                random_direction_in_cone(to_target, *degrees, &mut rng)
            }
        }
    }
}

/// defines how to render the missile, model or texture
/// todo: could be used as a serialized format
/// at runtime we use: MissileModelOrTextureRuntime
#[allow(missing_docs)]
pub enum MissileModelOrTexture {
    Model(String),
    Texture(String),
}

impl MissileModelOrTexture {
    /// load the asset + convert to runtime friendly handle: MissileModelOrTextureRuntime
    pub fn to_runtime(self, asset_server: &AssetServer) -> MissileModelOrTextureRuntime {
        match self {
            MissileModelOrTexture::Model(file) => MissileModelOrTextureRuntime::Model(
                asset_server.load(GltfAssetLabel::Scene(0).from_asset(file)),
            ),
            MissileModelOrTexture::Texture(file) => {
                MissileModelOrTextureRuntime::Texture(asset_server.load(file))
            }
        }
    }
}

/// runtime friendly version of MissileModelOrTexture
#[allow(missing_docs)]
pub enum MissileModelOrTextureRuntime {
    Model(Handle<Scene>),
    Texture(Handle<Image>),
}

/// how the hit/splat entity behaves
#[allow(missing_docs)]
pub enum HitBehaviour {
    /// no movement
    None,
    /// hit-entity move upwards on y
    FloatUp { speed: f32 },
}

/// this entity will move
#[derive(Component)]
pub struct FloatUp {
    speed: f32,
}

/// missile entity, might need some transform modifications
/// for example: the model/texture, is rotated.
#[allow(missing_docs)]
pub struct MissileVisualTransform {
    pub euler_rotation: Vec3,
    pub scale: f32,
}

#[allow(missing_docs)]
/// Missile behaviour settings
#[derive(Clone)]
pub struct MissileStats {
    /// radius to despawn missile
    pub hit_radius: f32,
    pub initial_speed: f32,
    pub max_speed: f32,
    pub max_acceleration: f32,
    /// how to assign velocity
    pub initial_velocity: InitialVelocity,
    /// how much to scale the hit texture transform
    pub hit_texture_scale: f32,
}

impl Default for MissileStats {
    fn default() -> Self {
        Self {
            initial_speed: 1000.0,
            max_speed: 19000.,
            max_acceleration: 19000.,
            hit_radius: 40.0,
            initial_velocity: InitialVelocity::Random,
            hit_texture_scale: 1.0,
        }
    }
}

/// fly entity towards target
#[allow(missing_docs)]
#[derive(Component, Clone)]
pub struct Missile {
    pub target_pos: Vec3,
    pub velocity: Vec3,
    pub stats: MissileStats,
}

/// Cursor synchronization parameters.
#[derive(SystemParam)]
pub(crate) struct CursorSyncParams<'w, 's> {
    app_config: Res<'w, AppConfig>,
    runtime: NonSend<'w, TerminalRuntime>,
    terminal: NonSend<'w, TerminalSurface>,
    viewport: Res<'w, TerminalViewport>,
    presentation: Res<'w, TerminalPresentation>,
    mobius_transition: Res<'w, MobiusTransition>,
    plane_warp: Res<'w, TerminalPlaneWarp>,
    time: Res<'w, Time>,
    plane_query: Query<'w, 's, &'static Transform, (With<TerminalPlane>, Without<CursorModel>)>,
    query: CursorTransformQuery<'w, 's>,
    prev_pos: Local<'s, Vec3>,
}

/// detects cursor move, to spawn our cursor effect
fn update_cursor_effects(
    mut params: CursorSyncParams,
    mut commands: Commands,
    missile_data_runtime: Res<MissileDataRuntime>,
) {
    let CursorSyncParams {
        app_config,
        runtime,
        terminal,
        viewport,
        presentation,
        mobius_transition,
        plane_warp,
        time,
        plane_query,
        query,
        prev_pos,
    } = &mut params;
    if query.is_empty() {
        return;
    }

    let pose_ctx = CursorPoseContext {
        runtime,
        terminal,
        viewport,
        mode: presentation.mode,
        plane_warp_amount: plane_warp.amount,
        mobius_progress: active_mobius_progress(presentation.mode, mobius_transition),
        elapsed_secs: time.elapsed_secs(),
        plane_query,
    };
    let (translation, _rotation, _scale, _cursor_visibility) = cursor_pose(app_config, &pose_ctx);
    let diff = translation.distance(*(*prev_pos));
    if diff > 14.0 {
        let start = **prev_pos;
        let end = translation;

        if let Some(sound_file) = missile_data_runtime.get_random_sound_spawn() {
            commands.spawn(AudioPlayer::new(sound_file));
        }

        let visual_transform = Transform::default()
            .with_rotation(Quat::from_euler(
                EulerRot::XYZ,
                missile_data_runtime
                    .missile_visual_transform
                    .euler_rotation
                    .x,
                missile_data_runtime
                    .missile_visual_transform
                    .euler_rotation
                    .y,
                missile_data_runtime
                    .missile_visual_transform
                    .euler_rotation
                    .z,
            ))
            .with_scale(Vec3::splat(
                missile_data_runtime.missile_visual_transform.scale,
            ));
        commands
            .spawn((
                Transform::from_translation(start),
                Visibility::Visible,
                TimedRemove(2.0),
                missile_data_runtime.make_missile(start, end),
            ))
            .with_children(
                |parent| match &missile_data_runtime.missile_presentation_runtime {
                    MissileModelOrTextureRuntime::Model(handle) => {
                        parent.spawn((SceneRoot(handle.clone()), visual_transform));
                    }
                    MissileModelOrTextureRuntime::Texture(handle) => {
                        parent.spawn((Sprite::from_image(handle.clone()), visual_transform));
                    }
                },
            );
        **prev_pos = translation;
    }
}

fn random_direction_in_cone(forward: Vec3, degrees: f32, rng: &mut ThreadRng) -> Vec3 {
    let radians = degrees.to_radians();
    let random_axis = random_direction(rng).cross(forward).normalize_or_zero();
    let angle = rng.random_range(0.0..radians);

    Quat::from_axis_angle(random_axis, angle) * forward
}

fn random_direction(rng: &mut ThreadRng) -> Vec3 {
    Vec3::new(
        rng.random_range(-1.0..1.0),
        rng.random_range(-1.0..1.0),
        rng.random_range(-1.0..1.0),
    )
    .normalize_or_zero()
}

/// serializable settings for a missle
/// todo: actually serialize :p
pub struct MissileData {
    spawn_sounds: Vec<String>,
    hit_sounds: Vec<String>,
    hit_textures: Vec<String>,
    missile_stats: MissileStats,
    missile_model_or_texture: MissileModelOrTexture,
    missile_visual_transform: MissileVisualTransform,
    hit_behaviour: HitBehaviour,
}

impl MissileData {
    /// loads all specified assets, and remember handles
    pub fn to_runtime(self, asset_server: &AssetServer) -> MissileDataRuntime {
        let MissileData {
            spawn_sounds,
            hit_sounds,
            hit_textures,
            missile_stats,
            missile_model_or_texture: sword_presentation,
            missile_visual_transform,
            hit_behaviour,
        } = self;
        let spawn_sounds = spawn_sounds
            .into_iter()
            .map(|file| asset_server.load(file))
            .collect();
        let hit_sounds = hit_sounds
            .into_iter()
            .map(|file| asset_server.load(file))
            .collect();
        let hit_textures = hit_textures
            .into_iter()
            .map(|file| asset_server.load(file))
            .collect();

        MissileDataRuntime {
            spawn_sounds,
            hit_sounds,
            hit_textures,
            missile_stats,
            missile_presentation_runtime: sword_presentation.to_runtime(asset_server),
            missile_visual_transform,
            hit_behaviour,
        }
    }
}

/// a runtime friendly version for MissileData: direct asset handles
#[derive(Resource)]
pub struct MissileDataRuntime {
    spawn_sounds: Vec<Handle<AudioSource>>,
    hit_sounds: Vec<Handle<AudioSource>>,
    hit_textures: Vec<Handle<Image>>,
    missile_stats: MissileStats,
    missile_presentation_runtime: MissileModelOrTextureRuntime,
    missile_visual_transform: MissileVisualTransform,
    hit_behaviour: HitBehaviour,
}

#[allow(missing_docs)]
impl MissileDataRuntime {
    /// helper to based upon configurations, make Missile component
    pub fn make_missile(&self, start: Vec3, end: Vec3) -> Missile {
        let mut velocity = self.missile_stats.initial_velocity.get_velocity(start, end);
        velocity *= self.missile_stats.initial_speed;
        Missile {
            target_pos: end,
            velocity,
            stats: self.missile_stats.clone(),
        }
    }

    pub fn get_random_sound_spawn(&self) -> Option<Handle<AudioSource>> {
        let mut rng = rand::rng();
        self.spawn_sounds.choose(&mut rng).cloned()
    }

    pub fn get_random_sound_hit(&self) -> Option<Handle<AudioSource>> {
        let mut rng = rand::rng();
        self.hit_sounds.choose(&mut rng).cloned()
    }

    pub fn get_random_texture(&self) -> Option<Handle<Image>> {
        let mut rng = rand::rng();
        self.hit_textures.choose(&mut rng).cloned()
    }
}

/// sets image filtering for all textures upon load
fn set_texture_filtering(
    mut events: MessageReader<AssetEvent<Image>>,
    missile_runtime_data: Option<Res<MissileDataRuntime>>,
    mut images: ResMut<Assets<Image>>,
) {
    let Some(missile_runtime_data) = &missile_runtime_data else {
        return;
    };
    for event in events.read() {
        let id = match event {
            AssetEvent::Added { id } => *id,
            AssetEvent::LoadedWithDependencies { id } => *id,
            _ => continue,
        };
        let mut is_hit_texture = missile_runtime_data
            .hit_textures
            .iter()
            .any(|handle| handle.id() == id);
        if let MissileModelOrTextureRuntime::Texture(texture) =
            &missile_runtime_data.missile_presentation_runtime
        {
            is_hit_texture |= texture.id() == id;
        }
        if !is_hit_texture {
            continue;
        }

        if let Some(image) = images.get_mut(id) {
            image.sampler = ImageSampler::nearest();
        }
    }
}

/// move all FloatUp entities
fn float_up(mut query: Query<(&mut FloatUp, &mut Transform)>, time: Res<Time>) {
    for (float, mut transform) in query.iter_mut() {
        transform.translation.y += float.speed * time.delta_secs();
    }
}

/// handle missile movement + collision
fn update_missiles(
    mut commands: Commands,
    mut query: Query<(Entity, &mut Missile, &mut Transform)>,
    time: Res<Time>,
    missile_runtime: Res<MissileDataRuntime>,
) {
    let dt = time.delta_secs();

    for (entity, mut missile, mut transform) in query.iter_mut() {
        let to_target = missile.target_pos - transform.translation;
        let distance = to_target.length();

        if distance <= missile.stats.hit_radius {
            commands.entity(entity).despawn();
            // the idea is to offset the z towards the camera
            // so hit textures always appear infront of the plane.
            // but it doesn't work in 3D mode...
            let mut offset = vec3(0.0, 0.0, 300.0);
            // todo: randomize spawn offset, should be a setting
            let r = 30.0;
            let mut rng = rand::rng();
            offset.x += rng.random_range(-r..=r);
            offset.y += rng.random_range(-r..=r);

            if let Some(sound) = missile_runtime.get_random_sound_hit() {
                commands.spawn(AudioPlayer::new(sound));
            }

            if let Some(splat_image) = missile_runtime.get_random_texture() {
                let entity = commands
                    .spawn((
                        Sprite::from_image(splat_image),
                        Transform::from_translation(missile.target_pos + offset).with_scale(
                            Vec3::splat(missile_runtime.missile_stats.hit_texture_scale),
                        ),
                        TimedRemove(0.5),
                    ))
                    .id();

                match missile_runtime.hit_behaviour {
                    HitBehaviour::None => (),
                    HitBehaviour::FloatUp { speed } => {
                        commands.entity(entity).insert(FloatUp { speed });
                    }
                }
            }
        }

        // the missile logic isn't perfect...
        let dir = to_target.normalize();
        let slow_radius = 400.0;

        let desired_speed = if distance < slow_radius {
            missile.stats.max_speed * (distance / slow_radius)
        } else {
            missile.stats.max_speed
        };

        let desired_velocity = dir * desired_speed;
        let steering =
            (desired_velocity - missile.velocity).clamp_length_max(missile.stats.max_acceleration);
        missile.velocity += steering * dt;

        let closing_speed = missile.velocity.dot(dir);
        if closing_speed < 0.0 {
            missile.velocity *= 0.50;
        }
        missile.velocity = missile.velocity.clamp_length_max(missile.stats.max_speed);
        transform.translation += missile.velocity * dt;
        if missile.velocity.length_squared() > 0.0001 {
            transform.look_to(missile.velocity.normalize(), Vec3::Y);
        }
    }
}

/// will despawn entity after specified seconds
#[derive(Component)]
pub struct TimedRemove(f32);

/// process and despawn TimedRemove entities
fn update_timed_remove(
    mut commands: Commands,
    mut query: Query<(Entity, &mut TimedRemove)>,
    time: Res<Time>,
) {
    for (entity, mut remove) in query.iter_mut() {
        remove.0 -= time.delta_secs();
        if remove.0 < 0.0 {
            commands.entity(entity).despawn();
        }
    }
}
