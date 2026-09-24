// Instanced quads: solid rectangles, coverage-mask glyphs and color glyphs.

struct Uniforms {
    screen_size: vec2<f32>,
    atlas_size: vec2<f32>,
}

@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(1) var mask_atlas: texture_2d<f32>;
@group(0) @binding(2) var color_atlas: texture_2d<f32>;
@group(0) @binding(3) var atlas_sampler: sampler;

const KIND_SOLID: u32 = 0u;
const KIND_MASK: u32 = 1u;
const KIND_COLOR: u32 = 2u;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    // Atlas region in texels: x, y, width, height.
    @location(2) uv: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) kind: u32,
}

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
}

@vertex
fn vs_main(@builtin(vertex_index) vertex: u32, inst: Instance) -> VertexOut {
    // Triangle strip: (0,0), (1,0), (0,1), (1,1).
    let corner = vec2<f32>(f32(vertex & 1u), f32(vertex >> 1u));
    let pixel = inst.pos + corner * inst.size;

    var out: VertexOut;
    out.position = vec4<f32>(
        pixel.x / u.screen_size.x * 2.0 - 1.0,
        1.0 - pixel.y / u.screen_size.y * 2.0,
        0.0,
        1.0,
    );
    out.uv = (inst.uv.xy + corner * inst.uv.zw) / u.atlas_size;
    out.color = inst.color;
    out.kind = inst.kind;
    return out;
}

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    let coverage = textureSampleLevel(mask_atlas, atlas_sampler, in.uv, 0.0).r;
    let color = textureSampleLevel(color_atlas, atlas_sampler, in.uv, 0.0);
    switch in.kind {
        case KIND_MASK: {
            return vec4<f32>(in.color.rgb, in.color.a * coverage);
        }
        case KIND_COLOR: {
            return color;
        }
        default: {
            return in.color;
        }
    }
}
