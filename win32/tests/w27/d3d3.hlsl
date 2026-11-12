// G5e2 test shaders: pixel shaders (ps_2_0, ps_2_a, ps_3_0) with vertex
// shaders feeding them. build.sh compiles each entry point with fxc into a
// header (build/d3d3_<entry>.h); the profile is in the entry's name.

row_major float4x4 wvp : register(c0);
row_major float4x4 world : register(c4);
float4 eye : register(c8);
row_major float4x4 proj_tex : register(c12);

sampler2D tex0 : register(s0);
sampler2D tex1 : register(s1);

float4 pc0 : register(c0);
float4 pc1 : register(c1);
float4 pc2 : register(c2);
float4 lights[4] : register(c4);
float4 colors[4] : register(c8);
int count : register(c16) : register(i0);

struct V2 {
    float4 pos : POSITION;
    float4 col : COLOR0;
    float4 t0 : TEXCOORD0;
    float4 t1 : TEXCOORD1;
    float4 t2 : TEXCOORD2;
};

// -----| vertex shaders |-----

// Position, colour, texture coordinates, and the world normal and view
// vector in texcoord1 and texcoord2.
V2 vs20_main(float4 p : POSITION, float3 n : NORMAL, float4 c : COLOR0, float2 t : TEXCOORD0)
{
    V2 o;
    o.pos = mul(p, wvp);
    o.col = c;
    o.t0 = float4(t, 0, 1);
    o.t1 = float4(mul(n, (float3x3)world), 0);
    o.t2 = float4(eye.xyz - mul(p, world).xyz, 0);
    return o;
}

// Projective texture coordinates in texcoord0.
V2 vs20_proj(float4 p : POSITION, float3 n : NORMAL, float4 c : COLOR0, float2 t : TEXCOORD0)
{
    V2 o;
    o.pos = mul(p, wvp);
    o.col = c;
    o.t0 = mul(mul(p, world), proj_tex);
    o.t1 = float4(t, 0, 1);
    o.t2 = 0;
    return o;
}

struct FogV {
    float4 pos : POSITION;
    float4 col : COLOR0;
    float2 t0 : TEXCOORD0;
    float fog : FOG;
};

FogV vs20_fog(float4 p : POSITION, float3 n : NORMAL, float4 c : COLOR0, float2 t : TEXCOORD0)
{
    FogV o;
    o.pos = mul(p, wvp);
    o.col = c;
    o.t0 = t;
    o.fog = saturate((14 - o.pos.w) / 12);
    return o;
}

struct V3 {
    float4 pos : POSITION;
    float4 n : NORMAL;
    float4 c : COLOR0;
    float2 t : TEXCOORD3;
};

V3 vs30_main(float4 p : POSITION, float3 n : NORMAL, float4 c : COLOR0, float2 t : TEXCOORD0)
{
    V3 o;
    o.pos = mul(p, wvp);
    o.n = float4(mul(n, (float3x3)world), 1);
    o.c = c;
    o.t = t;
    return o;
}

// -----| pixel shaders |-----

// A texture times the colour.
float4 ps20_modulate(V2 i) : COLOR
{
    return tex2D(tex0, i.t0.xy) * i.col;
}

// Per-pixel lighting: a normalized normal, diffuse and a specular power.
float4 ps20_light(V2 i) : COLOR
{
    float3 n = normalize(i.t1.xyz);
    float3 v = normalize(i.t2.xyz);
    float3 l = normalize(pc0.xyz);
    float3 h = normalize(l + v);
    float d = saturate(dot(n, l));
    float s = pow(saturate(dot(n, h)), 24);
    return float4(pc1.rgb * d + pc2.rgb + s * 0.7, 1);
}

// Two textures mixed by a constant, alpha from the second.
float4 ps20_mix(V2 i) : COLOR
{
    float4 a = tex2D(tex0, i.t0.xy);
    float4 b = tex2D(tex1, i.t0.xy * 1.5);
    float4 c = lerp(a, b, pc0.x);
    c.a = b.a * pc0.y + 0.2;
    return c;
}

// texkill outside a ring, and cmp choosing a colour.
float4 ps20_kill(V2 i) : COLOR
{
    float2 d = i.t0.xy - 0.5;
    float r = dot(d, d);
    clip(0.2 - r);
    clip(r - 0.03);
    float4 c = (d.x * d.y >= 0) ? float4(0.9, 0.5, 0.1, 1) : float4(0.1, 0.6, 0.9, 1);
    return c * i.col;
}

// A dependent read: one texture's colour moves where the other is read.
float4 ps20_dependent(V2 i) : COLOR
{
    float4 off = tex2D(tex1, i.t0.xy);
    return tex2D(tex0, i.t0.xy + (off.rg - 0.5) * 0.2);
}

// A projected texture (texldp), modulated by the colour.
float4 ps20_proj(V2 i) : COLOR
{
    return tex2Dproj(tex0, i.t0) * i.col + float4(0.05, 0.05, 0.05, 0);
}

// A level-of-detail bias (texldb).
float4 ps20_bias(V2 i) : COLOR
{
    return tex2Dbias(tex0, float4(i.t0.xy, 0, pc0.x));
}

// Arithmetic: frac, exp2, log2, sin and cos, rcp, rsq, min, max.
float4 ps20_math(V2 i) : COLOR
{
    float x = i.t0.x, y = i.t0.y;
    float4 c;
    c.r = frac(x * 3.7) * 0.6 + exp2(y - 1) * 0.3;
    c.g = log2(x + 1.2) * 0.5 + sin(y * 6) * 0.2 + 0.2;
    c.b = min(max(cos(x * 5) * 0.4 + 0.4, 0.1), 0.9) * rsqrt(y + 0.8);
    c.a = 1;
    c.r = c.r / (1 + y);
    return saturate(c);
}

// Writes depth: a slope across the quad, so two of them cross.
float4 ps20_depth(V2 i, out float depth : DEPTH) : COLOR
{
    depth = i.t0.x * pc0.x + pc0.y;
    return i.col;
}

// Fixed-function fog after a ps_2_0 shader.
float4 ps20_fog(FogV i) : COLOR
{
    return tex2D(tex0, i.t0) * i.col;
}

// ps_3_0: a loop over lights by aL, vFace, vPos.
float4 ps30_main(V3 i, float face : VFACE, float2 vpos : VPOS) : COLOR
{
    float3 n = normalize(i.n.xyz) * (face > 0 ? 1 : -1);
    float4 c = float4(0.05, 0.05, 0.05, 1);
    [loop] for (int k = 0; k < count; k++)
        c.rgb += colors[k].rgb * saturate(dot(n, lights[k].xyz));
    float check = frac((floor(vpos.x / 4) + floor(vpos.y / 4)) * 0.5);
    c.rgb *= 0.75 + check * 0.5;
    return c * i.c;
}

// ps_3_0: a dynamic branch and an early break.
float4 ps30_branch(V3 i) : COLOR
{
    float4 c = float4(0.1, 0.1, 0.1, 1);
    [branch] if (i.t.x > 0.5)
        c.r = i.t.y;
    else
        c.b = i.t.y;
    [loop] for (int k = 0; k < 6; k++) {
        if (k * 0.2 > i.t.x) break;
        c.g += 0.15;
    }
    return c;
}
