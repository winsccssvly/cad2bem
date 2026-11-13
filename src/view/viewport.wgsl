// SPDX-License-Identifier: GPL-3.0-or-later

struct Uniforms {
    matrix: mat4x4<f32>,
    flags: vec4<u32>,
    // x: the face under the mouse (lighter).
    hover: vec4<u32>,
};
@group(0) @binding(0) var<uniform> uniforms: Uniforms;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) normal: vec3<f32>,
    @location(1) barycentric: vec3<f32>,
    @location(2) @interpolate(flat) face: u32,
    // The check level (0 ok, 1 warn, 2 fail), plus 4 on the interior surface.
    @location(3) warned: f32,
    @location(4) @interpolate(flat) edges: vec3<f32>,
};

@vertex
fn vs_main(@location(0) position: vec3<f32>, @location(1) normal: vec3<f32>,
           @location(2) barycentric: vec3<f32>, @location(3) face: u32, @location(4) warned: f32,
           @location(5) edges: vec3<f32>) -> VertexOut {
    var out: VertexOut;
    out.position = uniforms.matrix * vec4(position, 1.0);
    out.normal = normal;
    out.barycentric = barycentric;
    out.face = face;
    out.warned = warned;
    out.edges = edges;
    return out;
}

@fragment
fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
    let normal = normalize(input.normal);
    let lighting = 0.4 + 0.6 * abs(dot(normal, normalize(vec3(0.4, -0.6, 1.0))));
    // Wetted body and interior surface (WETTED_COLOUR, INTERIOR_COLOUR in viewport.rs).
    let interior = input.warned > 3.5;
    let level = select(input.warned, input.warned - 4.0, interior);
    var color = select(vec3(0.56, 0.68, 0.77), vec3(0.47, 0.78, 0.52), interior);
    if uniforms.flags.z != 0u && level > 0.5 {
        color = vec3(0.96, 0.51, 0.17);
        if level > 1.5 {
            color = vec3(0.86, 0.16, 0.16);
        }
    }
    if input.face == uniforms.hover.x {
        color = mix(color, vec3(0.75, 0.92, 1.0), 0.45);
    }
    if input.face == uniforms.flags.x {
        color = vec3(0.18, 0.83, 0.83);
    }
    color *= lighting;
    let pixels = input.barycentric / max(fwidth(input.barycentric), vec3(0.00001));
    if uniforms.flags.w != 0u {
        // Lines between potential spans: weight 0.6.
        let span = select(vec3(100000.0), pixels, (input.edges > vec3(0.5)) & (input.edges < vec3(0.9)));
        let line = smoothstep(0.1, 0.9, min(span.x, min(span.y, span.z)));
        color = mix(vec3(0.97, 0.97, 0.99), color, line);
    }
    if uniforms.flags.y != 0u {
        let distance = select(vec3(100000.0), pixels, input.edges > vec3(0.9));
        let edge = smoothstep(0.2, 1.2, min(distance.x, min(distance.y, distance.z)));
        color = mix(vec3(0.06, 0.09, 0.12), color, edge);
    }
    return vec4(color, 1.0);
}
