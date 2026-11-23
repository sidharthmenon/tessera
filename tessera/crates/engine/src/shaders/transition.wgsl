// Mixes a transition's outgoing (t0) into its incoming (t1) screen texture, both premultiplied.

struct U {
    kind: u32,  // 0 fade, 1 wipe left, 2 wipe right
    p: f32,     // progress 0..1
};

@group(0) @binding(0) var t0: texture_2d<f32>;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
var<immediate> u: U;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    var c = array(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    var o: VO;
    o.pos = vec4(c[i].x * 2.0 - 1.0, 1.0 - c[i].y * 2.0, 0.0, 1.0);
    o.uv = c[i];
    return o;
}

@fragment fn fs(v: VO) -> @location(0) vec4<f32> {
    let a = textureSample(t0, s, v.uv);
    let b = textureSample(t1, s, v.uv);
    switch u.kind {
        case 1u: { return select(a, b, v.uv.x < u.p); }        // hard edge at p·width, incoming on the left
        case 2u: { return select(a, b, v.uv.x >= 1.0 - u.p); } // incoming on the right
        default: { return mix(a, b, u.p); }
    }
}
