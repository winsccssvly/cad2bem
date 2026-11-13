// SPDX-License-Identifier: GPL-3.0-or-later
//! Depth-tested native viewport and CAD-face picking in normalized display coordinates.

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context, Result};
use bytemuck::{Pod, Zeroable};
use eframe::{
    egui,
    egui_wgpu::{
        self,
        wgpu::{self, util::DeviceExt},
    },
};
use glam::{Mat4, Vec3};

use crate::cad::scene::{Mode, Quality, Scene};

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    barycentric: [f32; 3],
    face: u32,
    warned: f32,
    edges: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    matrix: [[f32; 4]; 4],
    flags: [u32; 4],
    hover: [u32; 4],
}

/// A scene prepared for the viewport: its display vertices and the triangles of poor quality.
pub struct MeshView {
    pub scene: Scene,
    pub warnings: Vec<(usize, Quality)>,
    positions: Vec<Vec3>,
    vertices: Arc<Vec<Vertex>>,
}

/// The kind of the faces of the interior surface in a scene.
pub const INTERIOR_KIND: &str = "Interior surface";
/// Added to a vertex's check level on the interior surface (the shader colours it apart).
const INTERIOR: f32 = 4.;
/// The colour of the body's wetted surface, as the shader draws it (unlit).
pub const WETTED_COLOUR: [f32; 3] = [0.56, 0.68, 0.77];
/// The colour of the interior surface, as the shader draws it (unlit).
pub const INTERIOR_COLOUR: [f32; 3] = [0.47, 0.78, 0.52];

/// How a frame draws a view: the selected and hovered faces, wireframe, quality colours and span lines.
pub struct Appearance {
    pub selected: Option<i32>,
    /// The face under the mouse, drawn lighter.
    pub hovered: Option<i32>,
    pub wire: bool,
    pub quality: bool,
    /// Draw the lines between the potential spans of spline patches.
    pub spans: bool,
}

impl MeshView {
    /// A view of a CAD scene (checked first).
    pub fn new(scene: Scene) -> Result<Self> {
        Self::prepare(scene, None, None)
    }

    /// Original panel edges, with display-only diagonals suppressed. No triangle-quality claims.
    /// `levels` per triangle: the check level of its patch or panel (0 ok, 1 warn, 2 fail).
    pub fn panels(scene: Scene, edges: Vec<[f32; 3]>, levels: Vec<u8>) -> Result<Self> {
        anyhow::ensure!(
            edges.len() == scene.triangles.len() && levels.len() == scene.triangles.len(),
            "Panel edge or level count mismatch"
        );
        Self::prepare(scene, Some(edges), Some(levels))
    }

    fn prepare(
        scene: Scene,
        edges: Option<Vec<[f32; 3]>>,
        levels: Option<Vec<u8>>,
    ) -> Result<Self> {
        scene.validate()?;
        let positions = scene.normalized_positions();
        // The CAD preview is drawn like CAD: shading smooth within each face and sharp across face boundaries,
        // and only the face boundaries outlined. A surface mesh shows its own flat triangles and their quality.
        let preview = edges.is_none() && scene.mode == Mode::Preview;
        let surface_mesh = edges.is_none() && scene.mode == Mode::SurfaceMesh;
        let edges = match edges {
            Some(edges) => Some(edges),
            None if preview => Some(face_outlines(&scene)),
            None => None,
        };
        let smooth = preview.then(|| face_normals(&scene, &positions));
        // The interior surface's faces have a colour of their own (`INTERIOR` added to their level).
        let interior: std::collections::HashSet<i32> = scene
            .faces
            .iter()
            .filter(|f| f.kind == INTERIOR_KIND)
            .map(|f| f.tag)
            .collect();
        let mut vertices = Vec::with_capacity(scene.triangles.len() * 3);
        let mut warnings = Vec::new();
        for (index, triangle) in scene.triangles.iter().enumerate() {
            let points = triangle.map(|i| positions[i as usize]);
            let flat = (points[1] - points[0])
                .cross(points[2] - points[0])
                .normalize_or_zero();
            let quality = scene.quality(index);
            let warned = surface_mesh && quality.warned();
            if warned {
                warnings.push((index, quality));
            }
            let level = levels.as_ref().map_or(u8::from(warned), |l| l[index]);
            let face = scene.face_tags[index];
            let level = f32::from(level)
                + if interior.contains(&face) {
                    INTERIOR
                } else {
                    0.
                };
            for corner in 0..3 {
                let mut barycentric = [0.; 3];
                barycentric[corner] = 1.;
                let normal = smooth
                    .as_ref()
                    .and_then(|s| s.get(&(face, triangle[corner])))
                    .map_or(flat, |n| n.normalize_or(flat));
                vertices.push(Vertex {
                    position: points[corner].to_array(),
                    normal: normal.to_array(),
                    barycentric,
                    face: face as u32,
                    warned: level,
                    edges: edges.as_ref().map_or([1.; 3], |e| e[index]),
                });
            }
        }
        warnings.sort_by(|a, b| a.1.min_angle.total_cmp(&b.1.min_angle));
        Ok(Self {
            scene,
            positions,
            vertices: Arc::new(vertices),
            warnings,
        })
    }

    /// The CAD face hit first by a ray.
    pub fn pick(&self, origin: Vec3, direction: Vec3) -> Option<i32> {
        self.hit(origin, direction).map(|(_, face)| face)
    }

    /// The nearest point where a ray hits the displayed surface, and its CAD face.
    pub fn hit(&self, origin: Vec3, direction: Vec3) -> Option<(Vec3, i32)> {
        self.scene
            .triangles
            .iter()
            .enumerate()
            .filter_map(|(i, triangle)| {
                ray_triangle(
                    origin,
                    direction,
                    triangle.map(|j| self.positions[j as usize]),
                )
                .map(|distance| (distance, i))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(distance, i)| (origin + distance * direction, self.scene.face_tags[i]))
    }

    /// Draw the view in `rect` with the camera; a new `revision` uploads the vertices again.
    pub fn paint(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        camera: &Camera,
        revision: u64,
        appearance: Appearance,
    ) {
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            Draw {
                revision,
                vertices: self.vertices.clone(),
                uniforms: Uniforms {
                    matrix: camera.matrix(rect.aspect_ratio()).to_cols_array_2d(),
                    flags: [
                        appearance.selected.map_or(u32::MAX, |f| f as u32),
                        u32::from(appearance.wire),
                        u32::from(appearance.quality),
                        u32::from(appearance.spans),
                    ],
                    hover: [appearance.hovered.map_or(u32::MAX, |f| f as u32), 0, 0, 0],
                },
            },
        ));
    }
}

/// A turntable camera around `target` (z up), in the normalized display coordinates of the model.
#[derive(Clone, Debug)]
pub struct Camera {
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    pub target: Vec3,
    /// The point an orbit turns about, fixed when the drag starts.
    pivot: Option<Vec3>,
    /// The surface point grabbed when a drag starts: a pan keeps it under the cursor, at its own depth.
    grab: Option<Vec3>,
}

/// Vertical field of view (radians).
const FIELD_OF_VIEW: f32 = 45. * std::f32::consts::PI / 180.;
/// Orbit speed (radians per point dragged).
const ORBIT: f32 = 0.008;
/// Zoom per point scrolled.
const ZOOM: f32 = 0.0015;

impl Default for Camera {
    fn default() -> Self {
        Self {
            yaw: -0.8,
            pitch: 0.45,
            distance: 5.2,
            target: Vec3::ZERO,
            pivot: None,
            grab: None,
        }
    }
}

impl Camera {
    /// Aim at the model centre and set the distance so that the whole view fits at this aspect ratio.
    pub fn fit(&mut self, view: &MeshView, aspect: f32) {
        self.target = Vec3::ZERO;
        let eye_direction = self.eye().normalize();
        let right = (-eye_direction).cross(Vec3::Z).normalize();
        let up = right.cross(-eye_direction);
        let vertical = (22.5_f32.to_radians()).tan();
        let horizontal = vertical * aspect.max(0.01);
        self.distance = view
            .positions
            .iter()
            .map(|p| {
                (right.dot(*p).abs() / horizontal).max(up.dot(*p).abs() / vertical)
                    + eye_direction.dot(*p)
            })
            .fold(0.1, f32::max)
            * 1.12;
    }

    fn eye(&self) -> Vec3 {
        self.target
            + self.distance
                * Vec3::new(
                    self.yaw.cos() * self.pitch.cos(),
                    self.yaw.sin() * self.pitch.cos(),
                    self.pitch.sin(),
                )
    }

    /// The view-projection matrix at this aspect ratio.
    pub fn matrix(&self, aspect: f32) -> Mat4 {
        // Clip planes follow the distance: a close view is not cut, a far one keeps depth precision.
        let near = (self.distance * 0.005).max(1e-6);
        let far = self.distance + 10.;
        Mat4::perspective_rh(FIELD_OF_VIEW, aspect.max(0.01), near, far)
            * Mat4::look_at_rh(self.eye(), self.target, Vec3::Z)
    }

    /// The ray (origin, unit direction) through a screen point of the viewport `rect`.
    pub fn ray(&self, rect: egui::Rect, point: egui::Pos2) -> (Vec3, Vec3) {
        let x = 2. * (point.x - rect.left()) / rect.width() - 1.;
        let y = 1. - 2. * (point.y - rect.top()) / rect.height();
        let inverse = self.matrix(rect.aspect_ratio()).inverse();
        let near = inverse.project_point3(Vec3::new(x, y, 0.));
        let far = inverse.project_point3(Vec3::new(x, y, 1.));
        (near, (far - near).normalize())
    }

    /// Left drag orbits about the surface point under the cursor where the drag started (the target when the
    /// cursor is off the model); right drag pans with the cursor; the wheel zooms toward the cursor.
    /// Motion is the cursor's, in points, not raw mouse counts.
    pub fn interact(&mut self, response: &egui::Response, view: &MeshView) {
        let rect = response.rect;
        let delta = response.drag_delta();
        // Left drag orbits, right drag pans; a left click (without dragging) selects or cuts.
        let orbiting = response.dragged_by(egui::PointerButton::Primary);
        let panning = response.dragged_by(egui::PointerButton::Secondary);
        if response.drag_started() {
            self.grab = response
                .interact_pointer_pos()
                .and_then(|point| {
                    let (origin, direction) = self.ray(rect, point);
                    view.hit(origin, direction)
                })
                .map(|(point, _)| point);
            self.pivot = self.grab;
        }
        if orbiting && delta != egui::Vec2::ZERO {
            self.orbit(-delta.x * ORBIT, delta.y * ORBIT);
        }
        if response.drag_stopped() {
            self.pivot = None;
            self.grab = None;
        }
        if panning && delta != egui::Vec2::ZERO {
            self.pan(rect, delta);
        }
        if response.hovered() {
            let scroll = response.ctx.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0. {
                let point = response.hover_pos().unwrap_or(rect.center());
                let (origin, direction) = self.ray(rect, point);
                let under = view.hit(origin, direction).map(|(p, _)| p);
                self.zoom(rect, point, scroll, under);
            }
        }
    }

    /// Move the view so that the grabbed surface point (or the scene at the target's depth, off the model)
    /// follows a cursor moved by `delta` points: close to the model the view moves as fast as the cursor.
    fn pan(&mut self, rect: egui::Rect, delta: egui::Vec2) {
        let (right, up) = self.screen_axes();
        let forward = (self.target - self.eye()).normalize();
        let depth = self
            .grab
            .map(|p| (p - self.eye()).dot(forward))
            .filter(|d| *d > 1e-6)
            .unwrap_or(self.distance);
        let units = 2. * depth * (FIELD_OF_VIEW / 2.).tan() / rect.height().max(1.);
        self.target += (-right * delta.x + up * delta.y) * units;
    }

    /// Zoom by a wheel `scroll` (points) toward the surface point `under` the cursor at `point` (or the scene
    /// point there at the target's depth, off the model), which stays in place on screen.
    fn zoom(&mut self, rect: egui::Rect, point: egui::Pos2, scroll: f32, under: Option<Vec3>) {
        let distance = (self.distance * (-scroll * ZOOM).exp()).clamp(1e-4, 50.);
        let (right, up) = self.screen_axes();
        let offset = (point - rect.center()) * self.units_per_point(rect);
        let under = under.unwrap_or(self.target + right * offset.x - up * offset.y);
        self.target = under + (self.target - under) * (distance / self.distance);
        self.distance = distance;
    }

    /// Turn the view by `yaw` about the vertical through the pivot and by `pitch` about the screen's
    /// horizontal axis; the pitch stops short of looking straight up or down.
    fn orbit(&mut self, yaw: f32, pitch: f32) {
        let pitch = (self.pitch + pitch).clamp(-1.55, 1.55) - self.pitch;
        let direction = (self.eye() - self.target).normalize();
        let axis = direction.cross(Vec3::Z).normalize_or_zero();
        let rotation = glam::Quat::from_rotation_z(yaw) * glam::Quat::from_axis_angle(axis, pitch);
        let pivot = self.pivot.unwrap_or(self.target);
        // The eye turns with the target about the pivot, so the angles change by exactly yaw and pitch.
        self.target = pivot + rotation * (self.target - pivot);
        self.yaw += yaw;
        self.pitch += pitch;
    }

    /// The screen's right and up directions in the scene.
    fn screen_axes(&self) -> (Vec3, Vec3) {
        let forward = (self.target - self.eye()).normalize();
        let right = forward.cross(Vec3::Z).normalize_or_zero();
        (right, right.cross(forward))
    }

    /// Scene units per screen point at the target's depth.
    fn units_per_point(&self, rect: egui::Rect) -> f32 {
        2. * self.distance * (FIELD_OF_VIEW / 2.).tan() / rect.height().max(1.)
    }

    /// Draw a scale bar (true at the depth of the view centre) and the x, y, z axes in the lower left corner.
    pub fn overlay(&self, painter: &egui::Painter, rect: egui::Rect, view: &MeshView) {
        let ink = egui::Color32::from_gray(40);
        let metres_per_point = f64::from(self.units_per_point(rect)) * view.scene.radius();
        if metres_per_point.is_finite() && metres_per_point > 0. {
            // The longest round length (1, 2 or 5 times a power of ten) that fits in 140 points.
            let most = metres_per_point * 140.;
            let power = 10f64.powf(most.log10().floor());
            let length = [5., 2., 1.]
                .into_iter()
                .map(|k| k * power)
                .find(|&l| l <= most)
                .unwrap_or(power);
            let width = (length / metres_per_point) as f32;
            let start = rect.left_bottom() + egui::vec2(16., -16.);
            let end = start + egui::vec2(width, 0.);
            let stroke = egui::Stroke::new(2_f32, ink);
            painter.line_segment([start, end], stroke);
            for x in [start, end] {
                painter.line_segment([x, x - egui::vec2(0., 6.)], stroke);
            }
            let label = if length >= 1. {
                format!("{} m", trim(length))
            } else {
                format!("{} mm", trim(length * 1000.))
            };
            painter.text(
                start + egui::vec2(width / 2., -8.),
                egui::Align2::CENTER_BOTTOM,
                label,
                egui::FontId::proportional(13.),
                ink,
            );
        }
        let (right, up) = self.screen_axes();
        let origin = rect.left_bottom() + egui::vec2(46., -72.);
        for (axis, name, colour) in [
            (Vec3::X, "x", egui::Color32::from_rgb(200, 40, 40)),
            (Vec3::Y, "y", egui::Color32::from_rgb(30, 140, 50)),
            (Vec3::Z, "z", egui::Color32::from_rgb(40, 80, 210)),
        ] {
            let tip = origin + egui::vec2(right.dot(axis), -up.dot(axis)) * 28.;
            painter.line_segment([origin, tip], egui::Stroke::new(2.5_f32, colour));
            painter.text(
                origin + (tip - origin) * 1.3,
                egui::Align2::CENTER_CENTER,
                name,
                egui::FontId::proportional(13.),
                colour,
            );
        }
    }
}

/// A length without trailing zeros.
fn trim(x: f64) -> String {
    let text = format!("{x:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// Per triangle, which edges (flag k: the edge opposite corner k) lie on the boundary of its CAD face: edges
/// that no other triangle of the same face shares.
fn face_outlines(scene: &Scene) -> Vec<[f32; 3]> {
    let key = |face: i32, a: u32, b: u32| (face, a.min(b), a.max(b));
    let mut count: HashMap<(i32, u32, u32), u32> = HashMap::new();
    for (t, &face) in scene.triangles.iter().zip(&scene.face_tags) {
        for k in 0..3 {
            *count
                .entry(key(face, t[(k + 1) % 3], t[(k + 2) % 3]))
                .or_default() += 1;
        }
    }
    scene
        .triangles
        .iter()
        .zip(&scene.face_tags)
        .map(|(t, &face)| {
            std::array::from_fn(|k| {
                f32::from(count[&key(face, t[(k + 1) % 3], t[(k + 2) % 3])] == 1)
            })
        })
        .collect()
}

/// Area-weighted vertex normals per CAD face and node: smooth within a face, separate across faces.
fn face_normals(scene: &Scene, positions: &[Vec3]) -> HashMap<(i32, u32), Vec3> {
    let mut normals: HashMap<(i32, u32), Vec3> = HashMap::new();
    for (t, &face) in scene.triangles.iter().zip(&scene.face_tags) {
        let p = t.map(|i| positions[i as usize]);
        let n = (p[1] - p[0]).cross(p[2] - p[0]);
        for &node in t {
            *normals.entry((face, node)).or_default() += n;
        }
    }
    normals
}

fn ray_triangle(origin: Vec3, direction: Vec3, p: [Vec3; 3]) -> Option<f32> {
    let a = p[1] - p[0];
    let b = p[2] - p[0];
    let h = direction.cross(b);
    let determinant = a.dot(h);
    if determinant.abs() < 1e-9 {
        return None;
    }
    let inverse = determinant.recip();
    let s = origin - p[0];
    let u = inverse * s.dot(h);
    if !(0. ..=1.).contains(&u) {
        return None;
    }
    let q = s.cross(a);
    let v = inverse * direction.dot(q);
    if v < 0. || u + v > 1. {
        return None;
    }
    let distance = inverse * b.dot(q);
    (distance > 0.).then_some(distance)
}

/// Create the viewport's shader and pipeline and register them with the egui wgpu renderer (wgpu only).
pub fn install(context: &eframe::CreationContext<'_>) -> Result<()> {
    let state = context
        .wgpu_render_state
        .as_ref()
        .context("A wgpu renderer is required")?;
    let device = &state.device;
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("CAD viewport"),
        source: wgpu::ShaderSource::Wgsl(include_str!("viewport.wgsl").into()),
    });
    let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("CAD viewport uniforms"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            count: None,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: std::num::NonZeroU64::new(std::mem::size_of::<Uniforms>() as u64),
            },
        }],
    });
    let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("CAD viewport layout"),
        bind_group_layouts: &[&layout],
        push_constant_ranges: &[],
    });
    let attributes = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3, 3 => Uint32, 4 => Float32, 5 => Float32x3];
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("CAD surface"),
        layout: Some(&pipeline_layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[wgpu::VertexBufferLayout {
                array_stride: std::mem::size_of::<Vertex>() as u64,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &attributes,
            }],
            compilation_options: Default::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(state.target_format.into())],
            compilation_options: Default::default(),
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: None,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: true,
            depth_compare: wgpu::CompareFunction::LessEqual,
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview: None,
        cache: None,
    });
    let uniform = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("CAD uniforms"),
        contents: bytemuck::bytes_of(&Uniforms::zeroed()),
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("CAD uniforms"),
        layout: &layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: uniform.as_entire_binding(),
        }],
    });
    let vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("Empty scene"),
        contents: bytemuck::bytes_of(&Vertex::zeroed()),
        usage: wgpu::BufferUsages::VERTEX,
    });
    state.renderer.write().callback_resources.insert(Gpu {
        pipeline,
        uniform,
        bind_group,
        vertex,
        revision: u64::MAX,
        count: 0,
    });
    Ok(())
}

struct Gpu {
    pipeline: wgpu::RenderPipeline,
    uniform: wgpu::Buffer,
    bind_group: wgpu::BindGroup,
    vertex: wgpu::Buffer,
    revision: u64,
    count: u32,
}

struct Draw {
    revision: u64,
    vertices: Arc<Vec<Vertex>>,
    uniforms: Uniforms,
}

impl egui_wgpu::CallbackTrait for Draw {
    fn prepare(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        _: &egui_wgpu::ScreenDescriptor,
        _: &mut wgpu::CommandEncoder,
        resources: &mut egui_wgpu::CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        let gpu = resources
            .get_mut::<Gpu>()
            .expect("Viewport resources installed at startup");
        if gpu.revision != self.revision {
            gpu.vertex = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("CAD triangle vertices"),
                contents: bytemuck::cast_slice(self.vertices.as_slice()),
                usage: wgpu::BufferUsages::VERTEX,
            });
            gpu.count = self.vertices.len() as u32;
            gpu.revision = self.revision;
        }
        queue.write_buffer(&gpu.uniform, 0, bytemuck::bytes_of(&self.uniforms));
        Vec::new()
    }

    fn paint(
        &self,
        _: egui::PaintCallbackInfo,
        pass: &mut wgpu::RenderPass<'static>,
        resources: &egui_wgpu::CallbackResources,
    ) {
        let gpu = resources
            .get::<Gpu>()
            .expect("Viewport resources installed at startup");
        pass.set_pipeline(&gpu.pipeline);
        pass.set_bind_group(0, &gpu.bind_group, &[]);
        pass.set_vertex_buffer(0, gpu.vertex.slice(..));
        pass.draw(0..gpu.count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picking_prefers_frontmost_triangle_from_either_side() {
        let low = [Vec3::ZERO, Vec3::X, Vec3::Y];
        let high = low.map(|v| v + Vec3::Z);
        let origin = Vec3::new(0.2, 0.2, 3.);
        assert!(
            ray_triangle(origin, -Vec3::Z, high).unwrap()
                < ray_triangle(origin, -Vec3::Z, low).unwrap()
        );
        assert!(ray_triangle(origin, -Vec3::Z, [low[2], low[1], low[0]]).is_some());
        assert!(ray_triangle(Vec3::new(2., 2., 3.), -Vec3::Z, low).is_none());
        assert!(ray_triangle(origin, Vec3::Z, low).is_none());
    }

    #[test]
    fn selected_identity_is_a_cad_face_not_a_triangle_index() {
        use crate::cad::scene::{Face, Mode, SCHEMA};
        let scene = Scene {
            open: None,
            schema: SCHEMA,
            source: "test.step".into(),
            sdk_version: "test".into(),
            mode: Mode::Preview,
            bounds_m: [0., 0., 0., 1., 1., 1.],
            positions_m: vec![
                [0., 0., 0.],
                [1., 0., 0.],
                [0., 1., 0.],
                [0., 0., 1.],
                [1., 0., 1.],
                [0., 1., 1.],
            ],
            triangles: vec![[0, 1, 2], [3, 4, 5]],
            face_tags: vec![17, 23],
            faces: vec![
                Face {
                    tag: 17,
                    kind: "Plane".into(),
                    area_m2: 0.5,
                    triangles: 1,
                    surface: None,
                    corners: Vec::new(),
                },
                Face {
                    tag: 23,
                    kind: "Plane".into(),
                    area_m2: 0.5,
                    triangles: 1,
                    surface: None,
                    corners: Vec::new(),
                },
            ],
            prepared: None,
        };
        let view = MeshView::new(scene).unwrap();
        assert_eq!(view.pick(Vec3::new(-0.5, -0.5, 3.), -Vec3::Z), Some(23));
        assert_eq!(view.pick(Vec3::new(-0.5, -0.5, -3.), Vec3::Z), Some(17));
        for aspect in [0.6, 1.8] {
            let mut camera = Camera::default();
            camera.fit(&view, aspect);
            for &point in &view.positions {
                let clip = camera.matrix(aspect).project_point3(point);
                assert!(clip.x.abs() < 1. && clip.y.abs() < 1. && (0. ..=1.).contains(&clip.z));
            }
        }
    }

    #[test]
    fn camera_centre_ray_points_at_target() {
        let camera = Camera::default();
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900., 600.));
        let (origin, direction) = camera.ray(rect, rect.center());
        assert!(direction.dot((camera.target - origin).normalize()) > 0.9999);
    }

    /// Where a scene point appears on screen, in points.
    fn screen(camera: &Camera, rect: egui::Rect, p: Vec3) -> egui::Pos2 {
        let ndc = camera.matrix(rect.aspect_ratio()).project_point3(p);
        egui::pos2(
            rect.left() + (ndc.x + 1.) / 2. * rect.width(),
            rect.top() + (1. - ndc.y) / 2. * rect.height(),
        )
    }

    #[test]
    fn navigation_keeps_the_point_under_the_cursor() {
        let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(900., 600.));
        let close = |a: egui::Pos2, b: egui::Pos2| (a - b).length() < 0.05;
        // Zoom in and out toward a cursor away from the centre.
        let mut camera = Camera::default();
        let cursor = egui::pos2(700., 150.);
        let (origin, direction) = camera.ray(rect, cursor);
        let depth = (camera.target - origin).dot((camera.target - camera.eye()).normalize())
            / direction.dot((camera.target - camera.eye()).normalize());
        let under = origin + depth * direction;
        for scroll in [120., -300., 40.] {
            camera.zoom(rect, cursor, scroll, None);
            assert!(close(screen(&camera, rect, under), cursor));
        }
        // Pan: a point at the target's depth moves with the cursor.
        let held = camera.target;
        let before = screen(&camera, rect, held);
        camera.pan(rect, egui::vec2(35., -20.));
        assert!(close(
            screen(&camera, rect, held),
            before + egui::vec2(35., -20.)
        ));
        // Zoom toward a surface point nearer than the target, then pan by it: it stays under the cursor.
        let (origin, direction) = camera.ray(rect, cursor);
        let surface = origin + 0.3 * (camera.target - origin).length() * direction;
        for scroll in [200., 200., -50.] {
            camera.zoom(rect, cursor, scroll, Some(surface));
            assert!(close(screen(&camera, rect, surface), cursor));
        }
        let before = screen(&camera, rect, surface);
        camera.grab = Some(surface);
        camera.pan(rect, egui::vec2(-40., 25.));
        assert!(close(
            screen(&camera, rect, surface),
            before + egui::vec2(-40., 25.)
        ));
        camera.grab = None;
        // Orbit about a pivot off the centre: the pivot stays put on screen.
        let pivot = Vec3::new(0.3, -0.2, 0.1);
        let fixed = screen(&camera, rect, pivot);
        camera.pivot = Some(pivot);
        camera.orbit(0.4, -0.25);
        assert!(close(screen(&camera, rect, pivot), fixed));
        assert!(camera.pitch.abs() <= 1.55);
    }
}
