// Shared vertex format for the warp mesh, waves, shapes, borders and motion vectors.
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
}

@group(0) @binding(0) var tex: texture_2d<f32>;
@group(0) @binding(1) var samp: sampler;

// Texture coordinates follow MilkDrop/WebGL (v = 0 at the bottom); wgpu stores row 0 at the top.
fn sample(uv: vec2<f32>) -> vec3<f32> {
    return textureSample(tex, samp, vec2(uv.x, 1.0 - uv.y)).rgb;
}

@vertex
fn vs_main(in: VsIn) -> VsOut {
    return VsOut(vec4(in.pos, 0.0, 1.0), in.uv, in.color);
}

@fragment
fn fs_color(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

@fragment
fn fs_tex(in: VsOut) -> @location(0) vec4<f32> {
    return vec4(sample(in.uv) * in.color.rgb, in.color.a);
}

// Composite pass: video echo, gamma and the four per-preset colour toggles.
struct Comp {
    echo_zoom: f32,
    echo_alpha: f32,
    echo_orient: f32,
    gamma: f32,
    brighten: f32,
    darken: f32,
    solarize: f32,
    invert: f32,
}

@group(0) @binding(2) var<uniform> comp: Comp;

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let x = f32(i32(i & 1u) * 4 - 1);
    let y = f32(i32(i >> 1u) * 4 - 1);
    return VsOut(vec4(x, y, 0.0, 1.0), vec2((x + 1.0) * 0.5, (y + 1.0) * 0.5), vec4(1.0));
}

@fragment
fn fs_comp(in: VsOut) -> @location(0) vec4<f32> {
    let orient_x = select(1.0, -1.0, (comp.echo_orient % 2.0) != 0.0);
    let orient_y = select(1.0, -1.0, comp.echo_orient >= 2.0);
    let uv_echo = (in.uv - 0.5) * (1.0 / comp.echo_zoom) * vec2(orient_x, orient_y) + 0.5;
    var ret = mix(sample(in.uv), sample(uv_echo), comp.echo_alpha) * comp.gamma;
    if comp.brighten != 0.0 { ret = sqrt(ret); }
    if comp.darken != 0.0 { ret = ret * ret; }
    if comp.solarize != 0.0 { ret = ret * (1.0 - ret) * 4.0; }
    if comp.invert != 0.0 { ret = 1.0 - ret; }
    return vec4(ret, 1.0);
}
