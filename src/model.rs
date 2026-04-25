use std::io::Cursor;
use std::path::Path;

use anyhow::{Context, ensure};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use rust_embed::RustEmbed;

use crate::config::{AppConfig, CURSOR_DEPTH};

#[derive(RustEmbed)]
#[folder = "assets/objects/"]
struct EmbeddedObjects;

#[derive(Component)]
pub struct CursorModel;

pub fn spawn_cursor_model(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    app_config: &AppConfig,
) {
    let root = commands
        .spawn((
            CursorModel,
            Transform::from_xyz(0.0, 0.0, CURSOR_DEPTH),
            Visibility::Visible,
        ))
        .id();
    let material = materials.add(StandardMaterial {
        base_color: Color::srgb_u8(255, 255, 255),
        emissive: LinearRgba::rgb(0.35, 0.35, 0.35),
        metallic: 0.0,
        perceptual_roughness: 0.28,
        reflectance: 0.6,
        cull_mode: None,
        ..default()
    });

    let configured_path = app_config.cursor.model.path.as_path();
    let maybe_meshes = if let Some(file_name) = configured_path.file_name().and_then(|name| name.to_str())
        && let Some(file) = EmbeddedObjects::get(file_name)
    {
        Some(
            load_obj_meshes_from_bytes(file_name, &file.data)
                .map(|meshes| (format!("embedded:{file_name}"), meshes)),
        )
    } else {
        Some(
            load_obj_meshes_from_path(configured_path)
                .map(|meshes| (configured_path.display().to_string(), meshes)),
        )
    };

    match maybe_meshes {
        Some(Ok((source, loaded_meshes))) if !loaded_meshes.is_empty() => {
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
        Some(Err(error)) => {
            warn!("failed to load cursor OBJ model: {error:#}");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
        _ => {
            warn!("no cursor OBJ model found; using cube cursor fallback");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
    }
}

fn load_obj_meshes_from_path(path: &Path) -> anyhow::Result<Vec<Mesh>> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_lines: true,
        ignore_points: true,
        ..default()
    };
    let (models, _) = tobj::load_obj(path, &options)
        .with_context(|| format!("failed to read {}", path.display()))?;
    build_meshes(models, path.display().to_string())
}

fn load_obj_meshes_from_bytes(name: &str, bytes: &[u8]) -> anyhow::Result<Vec<Mesh>> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_lines: true,
        ignore_points: true,
        ..default()
    };
    let (models, _) = tobj::load_obj_buf(
        &mut Cursor::new(bytes),
        &options,
        |_path| Ok((Vec::new(), Default::default())),
    )
    .with_context(|| format!("failed to read embedded {name}"))?;
    build_meshes(models, format!("embedded:{name}"))
}

fn build_meshes(models: Vec<tobj::Model>, source: String) -> anyhow::Result<Vec<Mesh>> {
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

        let center = (min + max) * 0.5;
        let extent = max - min;
        let max_extent = extent.max_element().max(1e-6);
        for p in &mut positions {
            p[0] = (p[0] - center.x) / max_extent;
            p[1] = (p[1] - center.y) / max_extent;
            p[2] = (p[2] - center.z) / max_extent;
        }

        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::default(),
        );
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);

        if !source_mesh.normals.is_empty() {
            let normals = source_mesh
                .normals
                .chunks_exact(3)
                .map(|normal| [normal[0], normal[1], normal[2]])
                .collect::<Vec<[f32; 3]>>();
            mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        }

        mesh.insert_indices(Indices::U32(source_mesh.indices));
        output.push(mesh);
    }

    ensure!(
        !output.is_empty(),
        "no mesh content inside {source}",
    );
    Ok(output)
}
