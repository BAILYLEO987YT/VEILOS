// dx6's shaders (G5f2d): tessellation, hull and domain shaders. Compiled
// by fxc into build/dx6_*.h (hs50_ is hs_5_0, ds50_ ds_5_0).

cbuffer F : register(b0)
{
    float4 edges;
    float4 inside;
};

struct CP
{
    float3 pos : POSITION;
    float4 col : COLOR;
};

struct VIn
{
    float3 pos : POSITION;
    float4 col : COLOR;
    float2 uv : TEXCOORD0;
};

CP vs50_main(VIn i)
{
    CP o;
    o.pos = i.pos;
    o.col = i.col;
    return o;
}

struct TriPC
{
    float e[3] : SV_TessFactor;
    float i : SV_InsideTessFactor;
};

TriPC tri_pc(InputPatch<CP, 3> p)
{
    TriPC o;
    o.e[0] = edges.x;
    o.e[1] = edges.y;
    o.e[2] = edges.z;
    o.i = inside.x;
    return o;
}

[domain("tri")]
[partitioning("integer")]
[outputtopology("triangle_cw")]
[outputcontrolpoints(3)]
[patchconstantfunc("tri_pc")]
CP hs50_tri(InputPatch<CP, 3> p, uint i : SV_OutputControlPointID)
{
    CP o = p[i];
    o.col = p[i].col * 0.9 + 0.1;
    return o;
}

[domain("tri")]
[partitioning("fractional_odd")]
[outputtopology("triangle_cw")]
[outputcontrolpoints(3)]
[patchconstantfunc("tri_pc")]
CP hs50_trifrac(InputPatch<CP, 3> p, uint i : SV_OutputControlPointID)
{
    return p[i];
}

struct QuadPC
{
    float e[4] : SV_TessFactor;
    float i[2] : SV_InsideTessFactor;
};

QuadPC quad_pc(InputPatch<CP, 4> p)
{
    QuadPC o;
    o.e[0] = edges.x;
    o.e[1] = edges.y;
    o.e[2] = edges.z;
    o.e[3] = edges.w;
    o.i[0] = inside.x;
    o.i[1] = inside.y;
    return o;
}

[domain("quad")]
[partitioning("integer")]
[outputtopology("triangle_ccw")]
[outputcontrolpoints(4)]
[patchconstantfunc("quad_pc")]
CP hs50_quad(InputPatch<CP, 4> p, uint i : SV_OutputControlPointID)
{
    return p[i];
}

struct V
{
    float4 pos : SV_Position;
    float4 col : COLOR;
    float3 bary : TEXCOORD0;
};

[domain("tri")]
V ds50_tri(TriPC pc, float3 b : SV_DomainLocation, const OutputPatch<CP, 3> p)
{
    V o;
    float3 pos = p[0].pos * b.x + p[1].pos * b.y + p[2].pos * b.z;
    // A bulge, so where the points fall shows.
    pos.xy *= 1.0 + 0.6 * b.x * b.y * b.z;
    o.pos = float4(pos, 1);
    o.col = p[0].col * b.x + p[1].col * b.y + p[2].col * b.z;
    o.bary = b;
    return o;
}

[domain("quad")]
V ds50_quad(QuadPC pc, float2 uv : SV_DomainLocation, const OutputPatch<CP, 4> p)
{
    V o;
    float3 a = lerp(p[0].pos, p[1].pos, uv.x), b = lerp(p[3].pos, p[2].pos, uv.x);
    float3 pos = lerp(a, b, uv.y);
    pos.xy *= 1.0 + 0.5 * uv.x * (1.0 - uv.x) * uv.y * (1.0 - uv.y);
    o.pos = float4(pos, 1);
    o.col = lerp(lerp(p[0].col, p[1].col, uv.x), lerp(p[3].col, p[2].col, uv.x), uv.y);
    o.bary = float3(uv, pc.i[0] / 16.0);
    return o;
}

// The bary coordinates as lines: a wireframe of the tessellation, drawn
// as colour bands, so the triangles' shapes show.
float4 ps50_main(V i) : SV_Target
{
    float m = min(min(i.bary.x, i.bary.y), i.bary.z);
    return float4(i.col.rgb * (m < 0.02 ? 0.3 : 1.0), 1);
}

float4 ps50_quad(V i) : SV_Target
{
    float2 f = frac(i.bary.xy * 4.0);
    return float4(i.col.rgb * ((f.x < 0.08 || f.y < 0.08) ? 0.3 : 1.0), 1);
}
