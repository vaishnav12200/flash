struct Viewport {
    size: vec2<f32>,
    _padding: vec2<f32>,
};

@group(0) @binding(0) var glyph_atlas: texture_2d<f32>;
@group(0) @binding(1) var glyph_sampler: sampler;
@group(0) @binding(2) var<uniform> viewport: Viewport;

struct VertexInput {
    @builtin(vertex_index) vertex_index: u32,
    @location(0) position: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_min: vec2<f32>,
    @location(3) uv_max: vec2<f32>,
    @location(4) color: vec4<f32>,
};

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vertex_main(input: VertexInput) -> VertexOutput {
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0),
        vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
    );
    let corner = corners[input.vertex_index];
    let pixel = input.position + corner * input.size;
    let clip = vec2(
        pixel.x / viewport.size.x * 2.0 - 1.0,
        1.0 - pixel.y / viewport.size.y * 2.0,
    );

    var output: VertexOutput;
    output.position = vec4(clip, 0.0, 1.0);
    output.uv = mix(input.uv_min, input.uv_max, corner);
    output.color = input.color;
    return output;
}

@fragment
fn fragment_main(input: VertexOutput) -> @location(0) vec4<f32> {
    let coverage = textureSample(glyph_atlas, glyph_sampler, input.uv).r;
    return vec4(input.color.rgb, input.color.a * coverage);
}
