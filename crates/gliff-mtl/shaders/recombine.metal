// AVC444 recombine: main + aux NV12 -> BGRA, with the YUV->RGB convert
// folded in. Transliteration of gliff-vk's recombine.comp; the CPU
// reference it must match is crates/gliff-mtl/src/display.rs, which is
// checked against gliff-proto::color. With has_aux == 0 the main chroma
// is duplicated over each 2x2 block (the Single420 path). Each source
// pixel is written as a zoom x zoom block, so the display side shows the
// frame at that integer scale without resampling.
// `dst` is zoom * width by zoom * height.
#include <metal_stdlib>
using namespace metal;

struct Params {
    uint width;
    uint height;
    uint has_aux;
    uint zoom;
};

// BT.709 limited range, matching gliff-vk/shaders/common.glsl.
static inline float3 to_rgb(float y8, float u8, float v8) {
    float y = y8 * 255.0f - 16.0f;
    float cb = u8 * 255.0f - 128.0f;
    float cr = v8 * 255.0f - 128.0f;
    float3 rgb = float3(1.1644f * y + 1.7927f * cr,
                        1.1644f * y - 0.2132f * cb - 0.5329f * cr,
                        1.1644f * y + 2.1124f * cb) / 255.0f;
    return clamp(rgb, 0.0f, 1.0f);
}

static inline void store(texture2d<float, access::write> dst, uint2 at,
                         float3 rgb, uint zoom) {
    // Writes are logical RGBA: the BGRA8Unorm format maps the components
    // onto its own byte order, so no manual swap (unlike the GLSL
    // original, whose image was a raw rgba8 view of a BGRX buffer).
    float4 px = float4(rgb, 1.0f);
    uint2 base = at * zoom;
    for (uint j = 0; j < zoom; j++) {
        for (uint i = 0; i < zoom; i++) {
            dst.write(px, base + uint2(i, j));
        }
    }
}

kernel void recombine(texture2d<float, access::read> main_y [[texture(0)]],
                      texture2d<float, access::read> main_uv [[texture(1)]],
                      texture2d<float, access::read> aux_y [[texture(2)]],
                      texture2d<float, access::read> aux_uv [[texture(3)]],
                      texture2d<float, access::write> dst [[texture(4)]],
                      constant Params &p [[buffer(0)]],
                      uint2 gid [[thread_position_in_grid]]) {
    uint x = 2 * gid.x;
    uint y = 2 * gid.y;
    if (x >= p.width || y >= p.height) {
        return;
    }
    float y00 = main_y.read(uint2(x, y)).r;
    float y10 = main_y.read(uint2(x + 1, y)).r;
    float y01 = main_y.read(uint2(x, y + 1)).r;
    float y11 = main_y.read(uint2(x + 1, y + 1)).r;
    float2 c00 = main_uv.read(uint2(gid.x, gid.y)).rg;
    float2 c10 = c00, c01 = c00, c11 = c00;
    if (p.has_aux != 0) {
        uint half_h = p.height / 2;
        c10 = aux_uv.read(uint2(gid.x, gid.y)).rg;
        c01 = float2(aux_y.read(uint2(x, gid.y)).r,
                     aux_y.read(uint2(x, half_h + gid.y)).r);
        c11 = float2(aux_y.read(uint2(x + 1, gid.y)).r,
                     aux_y.read(uint2(x + 1, half_h + gid.y)).r);
    }
    store(dst, uint2(x, y), to_rgb(y00, c00.r, c00.g), p.zoom);
    store(dst, uint2(x + 1, y), to_rgb(y10, c10.r, c10.g), p.zoom);
    store(dst, uint2(x, y + 1), to_rgb(y01, c01.r, c01.g), p.zoom);
    store(dst, uint2(x + 1, y + 1), to_rgb(y11, c11.r, c11.g), p.zoom);
}
