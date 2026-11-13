// SPDX-License-Identifier: GPL-3.0-or-later
//! Original low-order panel geometry and export: normals, convexity, warp, winding, volumes, HBG/GDF/STL text.

use anyhow::{Result, ensure};
use glam::DVec3;
use serde_json::{Value, json};
use std::fmt::Write;

use crate::conversion::{HBG_VERSION, Symmetry};

/// A panel: four vertices (m); a triangle repeats its third vertex as the fourth.
pub type Panel = [[f64; 3]; 4];

/// The unnormalized panel normal, the cross product of the diagonals (twice the area vector of a flat quad).
pub fn normal(panel: &Panel) -> DVec3 {
    let p = panel.map(DVec3::from_array);
    (p[2] - p[0]).cross(p[3] - p[1])
}

/// Whether the panel, projected on the plane normal to it, is strictly convex with no collapsed edge.
pub fn convex(panel: &Panel) -> bool {
    let p = panel.map(DVec3::from_array);
    let n = normal(panel).normalize_or_zero();
    if !n.is_finite() || n == DVec3::ZERO {
        return false;
    }
    let edges: [DVec3; 4] = std::array::from_fn(|i| {
        let edge = p[(i + 1) % 4] - p[i];
        edge - edge.dot(n) * n
    });
    let lengths = edges.map(DVec3::length);
    let minimum = lengths.into_iter().fold(f64::INFINITY, f64::min);
    let maximum = lengths.into_iter().fold(0., f64::max);
    if minimum <= 1e-12 * maximum {
        return false;
    }
    let sines: [f64; 4] = std::array::from_fn(|i| {
        (edges[i] / lengths[i])
            .cross(edges[(i + 1) % 4] / lengths[(i + 1) % 4])
            .dot(n)
    });
    sines.iter().all(|&s| s > 1e-3) || sines.iter().all(|&s| s < -1e-3)
}

/// The largest distance (m) of a vertex from the plane through the centroid normal to the panel.
pub fn warp(panel: &Panel) -> Result<f64> {
    let n = normal(panel);
    ensure!(
        n.is_finite() && n.length() > 0.,
        "Collapsed/non-finite panel normal"
    );
    let n = n.normalize();
    let points = panel.map(DVec3::from_array);
    let centre = points.into_iter().sum::<DVec3>() / 4.;
    Ok(points
        .into_iter()
        .map(|p| (p - centre).dot(n).abs())
        .fold(0., f64::max))
}

/// The panel with its winding reversed (a triangle stays a triangle).
pub fn reversed(panel: &Panel) -> Panel {
    if panel[2] == panel[3] {
        [panel[0], panel[2], panel[1], panel[1]]
    } else {
        [panel[0], panel[3], panel[2], panel[1]]
    }
}

/// Low-order panels of a body: the vertices, the CAD face of each panel, the symmetry planes and a label.
pub struct Panels {
    pub vertices: Vec<Panel>,
    pub faces: Vec<i32>,
    pub symmetry: Symmetry,
    pub label: String,
}

impl Panels {
    /// Check the panels before export: one positive face tag per panel, finite coordinates, a non-zero normal and
    /// convex quadrilaterals.
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.vertices.is_empty() && self.vertices.len() == self.faces.len(),
            "Panel/face count mismatch or empty mesh"
        );
        ensure!(
            self.faces.iter().all(|&tag| tag > 0),
            "Invalid prepared face identity"
        );
        for p in &self.vertices {
            ensure!(
                p.iter().flatten().all(|c| c.is_finite()),
                "Non-finite panel coordinates"
            );
            let n = normal(p);
            ensure!(
                n.is_finite() && n.length().is_finite() && n.length() > 0.,
                "Collapsed/non-finite panel normal"
            );
            ensure!(
                p[2] == p[3] || convex(p),
                "Non-convex panel requires repair/splitting"
            );
        }
        Ok(())
    }

    /// Three divergence-theorem volumes; panel winding points into the fluid.
    pub fn volumes(&self) -> Result<[f64; 3]> {
        self.validate()?;
        let mut volume = DVec3::ZERO;
        for [a, b, c] in [[0, 1, 2], [0, 2, 3]] {
            for panel in &self.vertices {
                let p = panel.map(DVec3::from_array);
                let area = (p[b] - p[a]).cross(p[c] - p[a]) * 0.5;
                volume += (p[a] + p[b] + p[c]) / 3. * area;
            }
        }
        Ok((volume * self.symmetry.images()).to_array())
    }

    /// Serialize original panel coordinates; display geometry is never used.
    pub fn to_hbg(&self) -> Result<Value> {
        self.validate()?;
        Ok(
            json!({"format":"hbg", "version":HBG_VERSION, "kind":"panels", "label":self.label,
            "symmetry":self.symmetry.name(), "vertices":self.vertices,
            "info":{"generator":{"name":"cad2bem", "version":env!("CARGO_PKG_VERSION"), "backend":"rust"}}}),
        )
    }

    /// The whole surface as panels: the stored ones and their mirror images in the symmetry planes (an image in
    /// one plane turned so that its normal still points into the fluid).
    pub fn whole(&self) -> Vec<Panel> {
        let mirrors: &[[f64; 2]] = match self.symmetry {
            Symmetry::None => &[[1., 1.]],
            Symmetry::Yz => &[[1., 1.], [-1., 1.]],
            Symmetry::Xz => &[[1., 1.], [1., -1.]],
            Symmetry::XzAndYz => &[[1., 1.], [-1., 1.], [1., -1.], [-1., -1.]],
        };
        let mut out = Vec::with_capacity(self.vertices.len() * mirrors.len());
        for &[sx, sy] in mirrors {
            for panel in &self.vertices {
                let image = panel.map(|[x, y, z]| [sx * x, sy * y, z]);
                out.push(if sx * sy < 0. {
                    reversed(&image)
                } else {
                    image
                });
            }
        }
        out
    }

    /// ASCII STL: the whole surface (`whole`) as triangles, a quad as two, the facet normals into the fluid
    /// (out of the body); the interior surface, when given, as a second solid.
    pub fn to_stl(&self, interior: Option<&Panels>) -> Result<String> {
        self.validate()?;
        let name: String = self
            .label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let mut text = String::new();
        let solid = |text: &mut String, name: &str, panels: &[Panel]| -> Result<()> {
            writeln!(text, "solid {name}")?;
            for panel in panels {
                let p = panel.map(DVec3::from_array);
                let triangles: &[[usize; 3]] = if panel[2] == panel[3] {
                    &[[0, 1, 2]]
                } else {
                    &[[0, 1, 2], [0, 2, 3]]
                };
                for &[a, b, c] in triangles {
                    let n = (p[b] - p[a]).cross(p[c] - p[a]).normalize_or_zero();
                    writeln!(text, "facet normal {:e} {:e} {:e}", n.x, n.y, n.z)?;
                    writeln!(text, "outer loop")?;
                    for k in [a, b, c] {
                        writeln!(
                            text,
                            "vertex {:.11e} {:.11e} {:.11e}",
                            p[k].x, p[k].y, p[k].z
                        )?;
                    }
                    writeln!(text, "endloop\nendfacet")?;
                }
            }
            writeln!(text, "endsolid {name}")?;
            Ok(())
        };
        let body = if name.is_empty() {
            "body".to_owned()
        } else {
            name
        };
        solid(&mut text, &body, &self.whole())?;
        if let Some(interior) = interior {
            interior.validate()?;
            solid(&mut text, "interior_surface", &interior.whole())?;
        }
        Ok(text)
    }

    /// The GDF text of the panels (checked first).
    pub fn to_gdf(&self) -> Result<String> {
        self.validate()?;
        let title = self.label.split_whitespace().collect::<Vec<_>>().join(" ");
        let title: String = title
            .chars()
            .map(|c| if c.is_ascii() { c } else { '?' })
            .collect();
        let x = u8::from(matches!(self.symmetry, Symmetry::Yz | Symmetry::XzAndYz));
        let y = u8::from(matches!(self.symmetry, Symmetry::Xz | Symmetry::XzAndYz));
        let mut text = format!(
            "cad2bem {}: {title}\n1 9.80665\n{x} {y}\n{}\n",
            env!("CARGO_PKG_VERSION"),
            self.vertices.len()
        );
        for [x, y, z] in self.vertices.iter().flatten() {
            writeln!(text, "{x:.11e} {y:.11e} {z:.11e}")?;
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stl_makes_the_symmetry_images_with_normals_into_the_fluid() {
        // A quarter of a horizontal square at z = -1, normal down (out of the body above it).
        let panels = Panels {
            vertices: vec![[[0., 0., -1.], [0., 1., -1.], [1., 1., -1.], [1., 0., -1.]]],
            faces: vec![1],
            symmetry: Symmetry::XzAndYz,
            label: "square".into(),
        };
        assert!(normal(&panels.vertices[0]).z < 0.);
        let whole = panels.whole();
        assert_eq!(whole.len(), 4);
        assert!(
            whole.iter().all(|p| normal(p).z < 0.),
            "images keep the side"
        );
        let text = panels.to_stl(None).unwrap();
        assert!(text.starts_with("solid square\n") && text.ends_with("endsolid square\n"));
        assert_eq!(text.matches("facet normal").count(), 8);
        assert!(
            text.lines()
                .filter(|l| l.starts_with("facet normal"))
                .all(|l| l.ends_with("-1e0"))
        );
    }

    #[test]
    fn translated_closed_box_has_three_equal_signed_volumes() {
        let v = [
            [0., 0., 0.],
            [2., 0., 0.],
            [2., 3., 0.],
            [0., 3., 0.],
            [0., 0., 4.],
            [2., 0., 4.],
            [2., 3., 4.],
            [0., 3., 4.],
        ];
        let vertices: Vec<Panel> = [
            [0, 3, 2, 1],
            [4, 5, 6, 7],
            [0, 1, 5, 4],
            [1, 2, 6, 5],
            [2, 3, 7, 6],
            [3, 0, 4, 7],
        ]
        .map(|f| {
            f.map(|i| {
                let [x, y, z] = v[i];
                [x + 13., y - 17., z + 5.]
            })
        })
        .to_vec();
        let mut panels = Panels {
            vertices,
            faces: vec![1, 2, 3, 4, 5, 6],
            symmetry: Symmetry::None,
            label: "box".into(),
        };
        for v in panels.volumes().unwrap() {
            assert!((v - 24.).abs() < 1e-12);
        }
        panels.vertices.iter_mut().for_each(|p| *p = reversed(p));
        for v in panels.volumes().unwrap() {
            assert!((v + 24.).abs() < 1e-12);
        }
    }

    #[test]
    fn triangle_winding_warp_and_wire_symmetry_are_preserved() {
        let triangle = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.], [0., 1., 0.]];
        assert_eq!(reversed(&reversed(&triangle)), triangle);
        assert!(normal(&triangle).dot(normal(&reversed(&triangle))) < 0.);
        assert_eq!(warp(&triangle).unwrap(), 0.);
        let panels = Panels {
            vertices: vec![triangle],
            faces: vec![7],
            symmetry: Symmetry::Xz,
            label: "tri\nname".into(),
        };
        assert_eq!(panels.to_hbg().unwrap()["symmetry"], "xz");
        assert_eq!(panels.to_gdf().unwrap().lines().nth(2), Some("0 1"));
        assert_eq!(panels.to_gdf().unwrap().lines().count(), 8);
        let bowtie = [[0., 0., 0.], [1., 1., 0.], [1., 0., 0.], [0., 1., 0.]];
        assert!(!convex(&bowtie));
        assert!(warp(&bowtie).is_err());
    }
}
