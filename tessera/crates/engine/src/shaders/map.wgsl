// Copies a uv rect of a (premultiplied) screen texture into a px rect of the target, × gain.

struct U {
    src: vec4<f32>,     // uv rect in the screen texture (x, y, w, h)
    dst: vec4<f32>,     // px rect in the target
    size: vec2<f32>,    // target size in px
    gain: f32,
};

@group(0) @binding(0) var t0: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var<uniform> u: U;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    var c = array(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    let p = u.dst.xy + c[i] * u.dst.zw;
    var o: VO;
    o.pos = vec4(p.x / u.size.x * 2.0 - 1.0, 1.0 - p.y / u.size.y * 2.0, 0.0, 1.0);
    o.uv = u.src.xy + c[i] * u.src.zw;
    return o;
}

// Shrinking (the preview layout, a big screen on a small output) averages a grid of bilinear taps,
// each covering 2×2 texels, over the pixel's footprint: a box filter, so 1 px detail averages in
// instead of falling between samples (there are no mips). 1:1 and enlarging take one tap.
@fragment fn fs(v: VO) -> @location(0) vec4<f32> {
    let uv_per_px = abs(u.src.zw / u.dst.zw);
    let n = clamp(ceil(uv_per_px * vec2<f32>(textureDimensions(t0)) / 2.0), vec2(1.0), vec2(16.0));
    let step = uv_per_px / n;
    var c = vec4(0.0);
    for (var y = 0.0; y < n.y; y += 1.0) {
        for (var x = 0.0; x < n.x; x += 1.0) {
            c += textureSampleLevel(t0, s, v.uv + (vec2(x, y) + 0.5 - n / 2.0) * step, 0.0);
        }
    }
    c /= n.x * n.y;
    return vec4(c.rgb * u.gain, c.a);
}
