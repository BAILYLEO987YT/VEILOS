// dx2's shaders (G5f1b), compiled by fxc into build/dx2_*.h: the entry
// point's prefix names the profile (vs40_ is vs_4_0, ps41_ ps_4_1).

cbuffer Xf : register(b0)
{
    float4x4 m;
    float4 tint;
    float4 misc;
};

cbuffer Look : register(b1)
{
    float4 colors[4];
    float4 scale;
};

struct VIn
{
    float3 pos : POSITION;
    float4 col : COLOR;
    float2 uv : TEXCOORD0;
};

struct VOut
{
    float4 pos : SV_Position;
    float4 col : COLOR;
    float2 uv : TEXCOORD0;
};

VOut vs40_main(VIn i)
{
    VOut o;
    o.pos = mul(float4(i.pos, 1), m);
    o.col = i.col * tint;
    o.uv = i.uv;
    return o;
}

// Instanced: a per-instance offset and the instance's number.
VOut vs40_inst(VIn i, float2 off : OFFSET, uint id : SV_InstanceID, uint vid : SV_VertexID)
{
    VOut o;
    o.pos = float4(i.pos.xy * 0.4 + off, i.pos.z, 1);
    o.col = colors[id & 3] * (0.6 + 0.1 * (vid & 3));
    o.uv = i.uv;
    return o;
}

float4 ps40_color(VOut i) : SV_Target
{
    return i.col;
}

Texture2D t0 : register(t0);
SamplerState s0 : register(s0);

float4 ps40_tex(VOut i) : SV_Target
{
    return t0.Sample(s0, i.uv) * i.col;
}

float4 ps40_math(VOut i) : SV_Target
{
    float2 p = i.uv;
    float v = sin(p.x * 12.0) * cos(p.y * 9.0);
    float3 c = lerp(float3(0.1, 0.2, 0.6), float3(0.9, 0.7, 0.2), saturate(v * 0.5 + 0.5));
    c += pow(frac(p.x * 3.0 + p.y * 2.0), 4.0) * 0.3;
    c.b += sqrt(p.x) * 0.2 - exp2(-p.y * 4.0) * 0.1 + log2(1.0 + p.y) * 0.1;
    c.g = max(c.g, rsqrt(4.0 + p.x * 12.0));
    return float4(c, 1);
}

float4 ps40_loop(VOut i) : SV_Target
{
    float2 c = i.uv * float2(2.6, 2.4) - float2(2.0, 1.2);
    float2 z = 0;
    int n;
    [loop] for (n = 0; n < 24; n++) {
        z = float2(z.x * z.x - z.y * z.y, 2.0 * z.x * z.y) + c;
        if (dot(z, z) > 4.0)
            break;
    }
    uint k = (uint)n;
    uint r = (k * 37u) & 255u, g = (k * 11u) & 255u, b = ((k ^ 0x15u) * 10u) & 255u;
    if (n == 24)
        return float4(0.05, 0.05, 0.1, 1);
    return float4(r / 255.0, g / 255.0, b / 255.0, 1);
}

float4 ps40_pos(VOut i) : SV_Target
{
    float2 d = i.uv - 0.5;
    clip(0.2 - dot(d, d));
    uint2 q = (uint2)i.pos.xy;
    float c = ((q.x ^ q.y) & 8u) ? 1.0 : 0.3;
    return float4(c, i.col.g, 1 - c, 1);
}

Texture2D<float> sm : register(t0);
SamplerComparisonState sc : register(s0);

float4 ps41_shadow(VOut i) : SV_Target
{
    float l = sm.SampleCmpLevelZero(sc, i.uv, i.col.r);
    float d = sm.Load(int3(i.uv * 64.0, 0));
    return float4(l, l * 0.5 + d * 0.5, 0.2, 1);
}

struct FIn
{
    float4 pos : SV_Position;
    nointerpolation float4 col : COLOR;
    float2 uv : TEXCOORD0;
};

float4 ps40_flat(FIn i, bool front : SV_IsFrontFace) : SV_Target
{
    return front ? i.col : i.col.bgra * scale;
}

// Constant buffers indexed by a register, and a lookup table.
static const float4 table[4] = { float4(1, 0, 0, 1), float4(0, 1, 0, 1), float4(0, 0, 1, 1), float4(1, 1, 0, 1) };

float4 ps40_index(VOut i) : SV_Target
{
    uint x = (uint)(i.uv.x * 4.0) & 3u, y = (uint)(i.uv.y * 4.0) & 3u;
    return colors[x] * 0.7 + table[y] * 0.3;
}
