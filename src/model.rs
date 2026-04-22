use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, ensure};
use bevy::asset::RenderAssetUsages;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::config::CURSOR_DEPTH;

#[derive(Component)]
pub struct CursorModel;

pub fn spawn_cursor_model(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
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
        ..default()
    });

    let maybe_obj_path = discover_obj_model_path();
    let maybe_meshes = maybe_obj_path
        .as_ref()
        .map(|path| load_obj_meshes(path).map(|loaded| (path, loaded)));

    match maybe_meshes {
        Some(Ok((path, loaded_meshes))) if !loaded_meshes.is_empty() => {
            info!(
                "loaded showcase model from {} ({} mesh parts)",
                path.display(),
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
            warn!("failed to load OBJ model from model/: {error:#}");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
        _ => {
            warn!("no OBJ model found in model/; using cube cursor fallback");
            commands.entity(root).with_children(|parent| {
                parent.spawn((
                    Mesh3d(meshes.add(Cuboid::new(1.0, 1.0, 1.0))),
                    MeshMaterial3d(material),
                ));
            });
        }
    }
}

fn discover_obj_model_path() -> Option<PathBuf> {
    let entries = fs::read_dir("model").ok()?;
    let mut candidates = Vec::new();

    for entry in entries {
        let entry = entry.ok()?;
        let path = entry.path();
        let is_obj = path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("obj"))
            .unwrap_or(false);
        if is_obj {
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            candidates.push((path, modified));
        }
    }

    candidates
        .into_iter()
        .max_by_key(|(_, modified)| *modified)
        .map(|(path, _)| path)
}

fn load_obj_meshes(path: &Path) -> anyhow::Result<Vec<Mesh>> {
    let options = tobj::LoadOptions {
        triangulate: true,
        single_index: true,
        ignore_lines: true,
        ignore_points: true,
        ..default()
    };
    let (models, _) = tobj::load_obj(path, &options)
        .with_context(|| format!("failed to read {}", path.display()))?;

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
        "no mesh content inside {}",
        path.display()
    );
    Ok(output)
}
