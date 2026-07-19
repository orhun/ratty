//! Cursor and object asset loading.

use std::io::Cursor;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail, ensure};
use bevy::asset::RenderAssetUsages;
use bevy::gltf::GltfAssetLabel;
use bevy::mesh::{Indices, PrimitiveTopology, VertexAttributeValues};
use bevy::prelude::*;
use rust_embed::RustEmbed;

use crate::config::{AppConfig, CURSOR_DEPTH};
use crate::inline::{InlineObject, RgpInlineObject};
use crate::paths::{expand_path, runtime_asset_root};

#[derive(RustEmbed)]
#[folder = "assets/objects/"]
struct EmbeddedObjects;

/// Marker for the spawned cursor model root.
#[derive(Component)]
pub struct CursorModel;

/// Loaded object source.
pub enum ObjectSource {
    /// OBJ mesh parts.
    Obj(Vec<Mesh>),
    /// glTF scene asset path.
    Gltf(String),
    /// STL, should be similar to OBJ
    Stl(Mesh),
}

impl From<ObjectSource> for InlineObject {
    fn from(val: ObjectSource) -> Self {
        InlineObject::RgpObject(match val {
            ObjectSource::Stl(mesh) => RgpInlineObject::Stl { mesh, handle: None },
            ObjectSource::Obj(meshes) => RgpInlineObject::Obj {
                meshes,
                handles: None,
            },
            ObjectSource::Gltf(asset_path) => RgpInlineObject::Gltf {
                asset_path,
                handle: None,
            },
        })
    }
}

/// Options that control object source loading.
#[derive(Clone, Copy, Debug)]
pub struct ObjectLoadOptions {
    /// Controls whether OBJ meshes are centered and scaled at load time.
    ///
    /// When enabled, each OBJ mesh is centered around its bounding-box center
    /// and scaled by the largest bounding-box axis. Disable this for generated
    /// or assembled OBJ assets whose source coordinates should be preserved.
    pub normalize: bool,
}

impl Default for ObjectLoadOptions {
    fn default() -> Self {
        Self { normalize: true }
    }
}

/// Spawns the configured cursor model.
pub fn spawn_cursor_model(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    images: &mut Assets<Image>,
    asset_server: &AssetServer,
    app_config: &AppConfig,
) {
    let root = commands
        .spawn((
            CursorModel,
            Transform::from_xyz(0.0, 0.0, CURSOR_DEPTH),
            Visibility::Visible,
        ))
        .id();

    let base_color_texture = app_config.cursor.model.texture.as_deref().and_then(|path| {
        match load_texture_image(path) {
            Ok(image) => {
                info!("loaded cursor texture from {}", path.display());
                Some(images.add(image))
            }
            Err(error) => {
                warn!("failed to load cursor texture: {error:#}");
                None
            }
        }
    });

    let [r, g, b] = app_config.cursor.model.color;
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb_u8(r, g, b),
        base_color_texture,
        emissive: LinearRgba::rgb(0.35, 0.35, 0.35),
        metallic: 0.0,
        perceptual_roughness: 0.28,
        reflectance: 0.6,
        cull_mode: None,
        ..default()
    });

    match load_object_source(app_config.cursor.model.path.as_path()) {
        Ok((source, ObjectSource::Obj(loaded_meshes))) if !loaded_meshes.is_empty() => {
            info!(
                "loaded cursor model from {} ({} mesh parts)",
                source,
                loaded_meshes.len()
            );
            commands.entity(root).with_children(|parent| {
                for mesh in loaded_meshes {
                    parent.spawn((
                        Mesh3d(meshes.add(mesh)),
                        MeshMaterial3d(material.clone()),
                        Transform::default(),
                    ));
                }
            });
        }
        Ok((source, ObjectSource::Gltf(asset_path))) => {
            info!("loading cursor model from {}", source);
            commands.entity(root).with_children(|parent| {
                parent.spawn(WorldAssetRoot(
                    asset_server.load(GltfAssetLabel::Scene(0).from_asset(asset_path)),
                ));
            });
        }
        Ok((source, ObjectSource::Stl(mesh))) => {
            info!("loaded cursor model from {source}");
            commands.entity(root).with_children(|parent| {
                parent.spawn((Mesh3d(meshes.add(mesh)), MeshMaterial3d(material.clone())));
            });
        }
        Err(error) => {
            warn!("failed to resolve cursor model: {error:#}");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
        _ => {
            warn!("no cursor model found; using cube cursor fallback");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
    }
}

/// Loads a base-color texture image from a path into a Bevy [`Image`].
///
/// # Errors
///
/// Returns an error if the file cannot be read or decoded.
fn load_texture_image(path: &Path) -> anyhow::Result<Image> {
    let path = expand_path(path);
    let bytes =
        std::fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let dynamic = image::load_from_memory(&bytes)
        .with_context(|| format!("failed to decode texture {}", path.display()))?;
    // Normalize to 8-bit RGBA so the texture uses a widely supported GPU format.
    // Decoded 16-bit images would otherwise need the TEXTURE_FORMAT_16BIT_NORM feature.
    let rgba = image::DynamicImage::ImageRgba8(dynamic.into_rgba8());
    Ok(Image::from_dynamic(
        rgba,
        true,
        RenderAssetUsages::default(),
    ))
}

/// Loads an object source from a path.
///
/// # Errors
///
/// Returns an error if the asset cannot be resolved or parsed.
pub fn load_object_source(path: &Path) -> anyhow::Result<(String, ObjectSource)> {
    load_object_source_with_options(path, ObjectLoadOptions::default())
}

/// Loads an object source from a path with explicit load options.
///
/// # Errors
///
/// Returns an error if the asset cannot be resolved or parsed.
pub fn load_object_source_with_options(
    path: &Path,
    options: ObjectLoadOptions,
) -> anyhow::Result<(String, ObjectSource)> {
    let expanded_path = expand_path(path);
    let path = expanded_path.as_path();
    if path.exists() {
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .unwrap_or_default();

        return match extension.as_str() {
            "stl" => load_stl_meshes_from_path(path)
                .map(|mesh| (path.display().to_string(), ObjectSource::Stl(mesh))),
            "obj" => load_obj_meshes_from_path(path, options.normalize)
                .map(|meshes| (path.display().to_string(), ObjectSource::Obj(meshes))),
            "glb" | "gltf" => {
                let bytes = std::fs::read(path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .filter(|stem| !stem.is_empty())
                    .unwrap_or("external");
                let sanitized = stem
                    .chars()
                    .map(|c| match c {
                        'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
                        _ => '_',
                    })
                    .collect::<String>();
                let candidate = format!("objects/external/{sanitized}.{extension}");
                let asset_file = runtime_asset_root().join(&candidate);
                std::fs::create_dir_all(
                    asset_file
                        .parent()
                        .context("scene asset path has no parent directory")?,
                )?;
                std::fs::write(&asset_file, &bytes)
                    .with_context(|| format!("failed to materialize scene {}", path.display()))?;
                Ok((path.display().to_string(), ObjectSource::Gltf(candidate)))
            }
            _ => bail!("unsupported object format for {}", path.display()),
        };
    }

    let candidate = object_asset_path(path)?;
    let extension = Path::new(&candidate)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .unwrap_or_default();

    if let Some(file_name) = Path::new(&candidate)
        .file_name()
        .and_then(|name| name.to_str())
        && let Some(file) = EmbeddedObjects::get(file_name)
    {
        return match extension.as_str() {
            "stl" => load_stl_meshes_from_bytes(&file.data)
                .map(|mesh| (format!("embedded:{file_name}"), ObjectSource::Stl(mesh))),
            "obj" => load_obj_meshes_from_bytes(file_name, &file.data, options.normalize)
                .map(|meshes| (format!("embedded:{file_name}"), ObjectSource::Obj(meshes))),
            "glb" | "gltf" => {
                let asset_path =
                    ensure_scene_asset_path(&candidate, Some((file_name, &file.data)))?;
                Ok((
                    format!("embedded:{file_name}"),
                    ObjectSource::Gltf(asset_path),
                ))
            }
            _ => bail!("unsupported object format for {}", candidate),
        };
    }

    match extension.as_str() {
        "stl" => load_stl_meshes_from_path(runtime_asset_root().join(&candidate).as_path())
            .or_else(|_| load_stl_meshes_from_path(path))
            .map(|mesh| (candidate.clone(), ObjectSource::Stl(mesh))),
        "obj" => load_obj_meshes_from_path(
            runtime_asset_root().join(&candidate).as_path(),
            options.normalize,
        )
        .or_else(|_| load_obj_meshes_from_path(path, options.normalize))
        .map(|meshes| (candidate.clone(), ObjectSource::Obj(meshes))),
        "glb" | "gltf" => {
            let asset_path = ensure_scene_asset_path(&candidate, None)?;
            Ok((candidate, ObjectSource::Gltf(asset_path)))
        }
        _ => bail!("unsupported object format for {}", candidate),
    }
}

/// Loads an object source from inline bytes.
///
/// # Errors
///
/// Returns an error if the payload cannot be parsed or materialized.
pub fn load_object_source_from_bytes(
    format: &str,
    name: Option<&str>,
    bytes: &[u8],
) -> anyhow::Result<(String, ObjectSource)> {
    load_object_source_from_bytes_with_options(format, name, bytes, ObjectLoadOptions::default())
}

/// Loads an object source from inline bytes with explicit load options.
///
/// # Errors
///
/// Returns an error if the payload cannot be parsed or materialized.
pub fn load_object_source_from_bytes_with_options(
    format: &str,
    name: Option<&str>,
    bytes: &[u8],
    options: ObjectLoadOptions,
) -> anyhow::Result<(String, ObjectSource)> {
    let display_name = name.unwrap_or(match format {
        "obj" => "payload.obj",
        "stl" => "payload.stl",
        "glb" | "gltf" => "payload.glb",
        _ => "payload",
    });

    let payload_name = format!("payload:{display_name}");

    match format {
        "stl" => {
            load_stl_meshes_from_bytes(bytes).map(|mesh| (payload_name, ObjectSource::Stl(mesh)))
        }
        "obj" => load_obj_meshes_from_bytes(display_name, bytes, options.normalize)
            .map(|meshes| (payload_name, ObjectSource::Obj(meshes))),
        "glb" | "gltf" => {
            // Bevy scene loading still goes through the asset server, so payload-backed GLB/GLTF
            // assets need to be materialized under the asset root before they can be instantiated.
            let extension = if format == "gltf" { "gltf" } else { "glb" };
            let stem = Path::new(display_name)
                .file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .unwrap_or("payload");
            let sanitized = stem
                .chars()
                .map(|c| match c {
                    'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
                    _ => '_',
                })
                .collect::<String>();
            let candidate = format!("objects/rgp/{sanitized}.{extension}");
            let asset_path = ensure_scene_asset_path(&candidate, Some((display_name, bytes)))?;
            Ok((payload_name, ObjectSource::Gltf(asset_path)))
        }
        _ => bail!("unsupported object format for {}", display_name),
    }
}

fn ensure_scene_asset_path(
    candidate: &str,
    embedded: Option<(&str, &[u8])>,
) -> anyhow::Result<String> {
    let asset_file = runtime_asset_root().join(candidate);
    if !asset_file.exists() {
        if let Some((name, bytes)) = embedded {
            std::fs::create_dir_all(
                asset_file
                    .parent()
                    .context("scene asset path has no parent directory")?,
            )?;
            std::fs::write(&asset_file, bytes)
                .with_context(|| format!("failed to restore embedded scene {}", name))?;
        } else {
            bail!("asset not found: {}", asset_file.display());
        }
    }

    Ok(candidate.to_string())
}

fn object_asset_path(path: &Path) -> anyhow::Result<String> {
    let components = path.components().collect::<Vec<_>>();
    if let Some(index) = components
        .iter()
        .position(|component| matches!(component, Component::Normal(part) if *part == "assets"))
    {
        let relative = components[index + 1..]
            .iter()
            .filter_map(|component| match component {
                Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if !relative.is_empty() {
            return Ok(relative.join("/"));
        }
    }

    if path.is_absolute() {
        bail!(
            "absolute path is outside the asset root: {}",
            path.display()
        );
    }

    let mut candidate = PathBuf::from(path);
    if candidate.components().count() == 1 {
        candidate = Path::new("objects").join(candidate);
    }

    let candidate = candidate
        .to_str()
        .context("asset path is not valid UTF-8")?
        .replace('\\', "/");
    Ok(candidate
        .strip_prefix("assets/")
        .unwrap_or(&candidate)
        .to_string())
}

fn load_stl_meshes_from_path(path: &Path) -> anyhow::Result<Mesh> {
    let data = std::fs::read(path)?;
    load_stl_meshes_from_bytes(&data)
}

fn load_stl_meshes_from_bytes(bytes: &[u8]) -> anyhow::Result<Mesh> {
    let mut c = Cursor::new(bytes);
    let stl = stl_io::read_stl(&mut c)?;

    // credit: bevy_stl (MIT)
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );

    let vertex_count = stl.faces.len() * 3;

    let mut positions = Vec::with_capacity(vertex_count);
    let mut normals = Vec::with_capacity(vertex_count);
    let mut indices = Vec::with_capacity(vertex_count);

    for (i, face) in stl.faces.iter().enumerate() {
        for j in 0..3 {
            let vertex = stl.vertices[face.vertices[j]];
            positions.push([vertex[0], vertex[1], vertex[2]]);
            normals.push([face.normal[0], face.normal[1], face.normal[2]]);
            indices.push((i * 3 + j) as u32);
        }
    }

    let uvs = vec![[0.0, 0.0]; vertex_count];

    mesh.insert_attribute(
        Mesh::ATTRIBUTE_POSITION,
        VertexAttributeValues::Float32x3(positions),
    );
    mesh.insert_attribute(
        Mesh::ATTRIBUTE_NORMAL,
        VertexAttributeValues::Float32x3(normals),
    );
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, VertexAttributeValues::Float32x2(uvs));
    mesh.insert_indices(Indices::U32(indices));
    // appropriated code over

    Ok(mesh)
}

fn load_obj_meshes_from_path(path: &Path, normalize: bool) -> anyhow::Result<Vec<Mesh>> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_lines: true,
        ignore_points: true,
    };
    let (models, _) = tobj::load_obj(path, &options)
        .with_context(|| format!("failed to read {}", path.display()))?;
    build_meshes(models, path.display().to_string(), normalize)
}

fn load_obj_meshes_from_bytes(
    name: &str,
    bytes: &[u8],
    normalize: bool,
) -> anyhow::Result<Vec<Mesh>> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_lines: true,
        ignore_points: true,
    };
    let (models, _) = tobj::load_obj_buf(&mut Cursor::new(bytes), &options, |_path| {
        Ok((Vec::new(), Default::default()))
    })
    .with_context(|| format!("failed to read embedded {name}"))?;
    build_meshes(models, format!("embedded:{name}"), normalize)
}

fn build_meshes(
    models: Vec<tobj::Model>,
    source: String,
    normalize: bool,
) -> anyhow::Result<Vec<Mesh>> {
    let mut output = Vec::new();
    for model in models {
        let source_mesh = model.mesh;
        if source_mesh.positions.is_empty() {
            continue;
        }

        let mut positions = Vec::<[f32; 3]>::with_capacity(source_mesh.positions.len() / 3);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for pos in source_mesh.positions.chunks_exact(3) {
            let point = Vec3::new(pos[0], pos[1], pos[2]);
            min = min.min(point);
            max = max.max(point);
            positions.push([point.x, point.y, point.z]);
        }

        if normalize {
            let center = (min + max) * 0.5;
            let extent = max - min;
            let max_extent = extent.max_element().max(1e-6);
            for p in &mut positions {
                p[0] = (p[0] - center.x) / max_extent;
                p[1] = (p[1] - center.y) / max_extent;
                p[2] = (p[2] - center.z) / max_extent;
            }
        }

        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        let position_count = positions.len();
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

        if !source_mesh.vertex_color.is_empty() {
            let colors = source_mesh
                .vertex_color
                .chunks_exact(3)
                .map(|color| [color[0], color[1], color[2], 1.0])
                .collect::<Vec<[f32; 4]>>();
            if colors.len() == source_mesh.positions.len() / 3 {
                mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, colors);
            }
        }

        if !source_mesh.normals.is_empty() {
            let normals = source_mesh
                .normals
                .chunks_exact(3)
                .map(|normal| [normal[0], normal[1], normal[2]])
                .collect::<Vec<[f32; 3]>>();
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        }

        if !source_mesh.texcoords.is_empty() {
            let uvs = source_mesh
                .texcoords
                .chunks_exact(2)
                .map(|uv| [uv[0], 1.0 - uv[1]])
                .collect::<Vec<[f32; 2]>>();
            if uvs.len() == position_count {
                mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
            }
        }

        mesh.insert_indices(Indices::U32(source_mesh.indices));
        output.push(mesh);
    }

    ensure!(!output.is_empty(), "no mesh content inside {source}");
    Ok(output)
}
