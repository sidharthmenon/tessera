// One layer drawn into a screen texture: sample/generate → effects → premultiplied out.

struct U {
    rect: vec4<f32>,    // layer rect in screen px (x, y, w, h)
    color: vec4<f32>,   // Solid pattern, straight alpha
    screen: vec2<f32>,  // screen size in px
    kind: u32,          // 0 rgba, 1 yuv (nv12/p010), 2 bars, 3 grid, 4 gradient, 5 solid, 6 ayuv64, 7 rgbx
    flags: u32,         // bit 0 premultiplied, bit 1 full range
    opacity: f32,
    brightness: f32,
    contrast: f32,
    saturation: f32,
    yuv_scale: f32,     // normalized → 8-bit code values: 255 (8-bit), 256 (MSB-aligned 16-bit)
    rotation: f32,      // radians clockwise about the rect's centre
};

@group(0) @binding(0) var t0: texture_2d<f32>;
@group(0) @binding(1) var t1: texture_2d<f32>;
@group(0) @binding(2) var s: sampler;
@group(0) @binding(3) var<uniform> u: U;

struct VO { @builtin(position) pos: vec4<f32>, @location(0) uv: vec2<f32> };

@vertex fn vs(@builtin(vertex_index) i: u32) -> VO {
    var c = array(vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 0.0), vec2(1.0, 1.0));
    // Rotated in pixel space, before NDC, so a rotated layer keeps its shape on any screen.
    let d = (c[i] - 0.5) * u.rect.zw;
    let k = vec2(cos(u.rotation), sin(u.rotation));
    let p = u.rect.xy + u.rect.zw * 0.5 + vec2(d.x * k.x - d.y * k.y, d.x * k.y + d.y * k.x);
    var o: VO;
    o.pos = vec4(p.x / u.screen.x * 2.0 - 1.0, 1.0 - p.y / u.screen.y * 2.0, 0.0, 1.0);
    o.uv = c[i];
    return o;
}

// BT.709 Y'CbCr (normalized samples) → R'G'B'; limited range is Y 16–235, CbCr 16–240.
fn yuv(ys: f32, cbs: f32, crs: f32) -> vec3<f32> {
    var y = ys;
    var cb = cbs - 0.5;
    var cr = crs - 0.5;
    if (u.flags & 2u) == 0u {
        y = (ys * u.yuv_scale - 16.0) / 219.0;
        cb = (cbs * u.yuv_scale - 128.0) / 224.0;
        cr = (crs * u.yuv_scale - 128.0) / 224.0;
    }
    return vec3(y + 1.5748 * cr, y - 0.1873 * cb - 0.4681 * cr, y + 1.8556 * cb);
}

// 8 full-intensity bars in SMPTE order.
fn bars(x: f32) -> vec4<f32> {
    var c = array(
        vec3(1.0, 1.0, 1.0), vec3(1.0, 1.0, 0.0), vec3(0.0, 1.0, 1.0), vec3(0.0, 1.0, 0.0),
        vec3(1.0, 0.0, 1.0), vec3(1.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0), vec3(0.0, 0.0, 0.0),
    );
    return vec4(c[min(u32(x * 8.0), 7u)], 1.0);
}

// 1 px white lines every 64 screen px + a center cross, on black.
fn grid(pos: vec2<f32>) -> vec4<f32> {
    let p = floor(pos);
    let line = p.x % 64.0 == 0.0 || p.y % 64.0 == 0.0 || any(abs(pos - u.screen * 0.5) < vec2(1.0));
    return vec4(vec3(select(0.0, 1.0, line)), 1.0);
}

@fragment fn fs(v: VO) -> @location(0) vec4<f32> {
    let a = textureSample(t0, s, v.uv);
    let b = textureSample(t1, s, v.uv);
    var c: vec4<f32>;
    switch u.kind {
        case 0u: { c = a; }
        case 1u: { c = vec4(yuv(a.r, b.r, b.g), 1.0); }
        case 6u: { c = vec4(yuv(a.g, a.b, a.a), a.r); } // packed A Y Cb Cr, straight alpha
        case 7u: { c = vec4(a.rgb, 1.0); }               // fourth byte undefined: opaque
        case 2u: { c = bars(v.uv.x); }
        case 3u: { c = grid(v.pos.xy); }
        case 4u: { c = vec4(vec3(v.uv.x), 1.0); }
        default: { c = u.color; }
    }
    if (u.flags & 1u) != 0u && c.a > 0.0 {
        c = vec4(c.rgb / c.a, c.a);
    }
    var rgb = (c.rgb - 0.5) * u.contrast + 0.5 + u.brightness;
    rgb = mix(vec3(dot(rgb, vec3(0.2126, 0.7152, 0.0722))), rgb, u.saturation);
    rgb = clamp(rgb, vec3(0.0), vec3(1.0));
    let alpha = clamp(c.a * u.opacity, 0.0, 1.0);
    return vec4(rgb * alpha, alpha);
}
