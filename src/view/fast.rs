// SPDX-License-Identifier: GPL-3.0-or-later
//! The display of a body made from its dimensions (Fast Mesh tab): its panels by part, and with the interior
//! surface.

use super::{
    conversion::{Geometry, Panels, Surface, mesh_view},
    viewport::MeshView,
};
use crate::{conversion::DEFAULT_POTENTIAL_PANEL_SIZE_M, engine::primitives::Made};
use anyhow::Result;
use std::path::Path;

/// The views of a made body: the body alone, and with its interior surface when it has one. Each part is a
/// group named as the part.
pub fn views(made: &Made) -> Result<(MeshView, Option<MeshView>)> {
    let surface = |vertices: &[[[f64; 3]; 4]], owners: Vec<i32>| -> Result<Surface> {
        let count = vertices.len();
        Surface::new(
            Geometry::Panels(Panels::from_vertices(vertices.to_vec())),
            owners,
            Vec::new(),
            None,
            vec![0; count],
            DEFAULT_POTENTIAL_PANEL_SIZE_M,
        )
    };
    let body = surface(&made.body.vertices, made.body.faces.clone())?;
    let interior_tag = made.parts.len() as i32 + 1;
    let interior = made
        .interior
        .as_ref()
        .map(|i| surface(&i.vertices, vec![interior_tag; i.vertices.len()]))
        .transpose()?;
    let source = Path::new(&made.body.label);
    let version = env!("CARGO_PKG_VERSION");
    let name = |mut view: MeshView| {
        for face in &mut view.scene.faces {
            if let Some(part) = made.parts.get(face.tag as usize - 1) {
                face.kind = (*part).to_owned();
            }
        }
        view
    };
    let alone = name(mesh_view(
        source,
        version,
        "Part",
        (&body, None),
        None,
        interior_tag,
    )?);
    let with_interior = interior
        .as_ref()
        .map(|i| {
            mesh_view(
                source,
                version,
                "Part",
                (&body, None),
                Some((i, None)),
                interior_tag,
            )
        })
        .transpose()?
        .map(name);
    Ok((alone, with_interior))
}
