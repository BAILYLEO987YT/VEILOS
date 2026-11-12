// G5e1 test shaders: vertex shaders feeding Direct3D 9's fixed-function
// pixel pipeline. build.sh compiles each entry point with fxc into a header
// (build/d3d2_<entry>.h); the profile is in the entry's name.

row_major float4x4 wvp : register(c0);
row_major float4x4 world : register(c4);
float4 light_dir : register(c8);
float4 light_col : register(c9);
float4 ambient : register(c10);
float4 params : register(c11);
float4 eye : register(c12);
float4 lights[4] : register(c16);
float4 colors[4] : register(c20);
float4x3 bones[4] : register(c32);
int count : register(c14) : register(i0);
bool use_spec : register(b0);
bool use_red : register(b1);

struct Out {
    float4 pos : POSITION;
    float4 col : COLOR0;
    float4 spec : COLOR1;
    float2 uv : TEXCOORD0;
};

// vs_1_1: diffuse and specular lighting, written out.
Out vs11_lit(float4 p : POSITION, float3 n : NORMAL)
{
    Out o;
    float3 wn = normalize(mul(n, (float3x3)world));
    float3 wp = mul(p, world).xyz;
    float3 v = normalize(eye.xyz - wp);
    float3 h = normalize(v - light_dir.xyz);
    float4 l = lit(dot(wn, -light_dir.xyz), dot(wn, h), 24);
    o.pos = mul(p, wvp);
    o.col = saturate(ambient + light_col * l.y);
    o.spec = saturate(l.z * float4(0.8, 0.8, 0.6, 1));
    o.uv = 0;
    return o;
}

// vs_2_0: a loop over lights (an integer constant), and a static branch.
Out vs20_loop(float4 p : POSITION, float3 n : NORMAL)
{
    Out o;
    float3 wn = normalize(mul(n, (float3x3)world));
    float4 c = ambient;
    [loop] for (int i = 0; i < count; i++)
        c += colors[i] * max(0, dot(wn, lights[i].xyz));
    if (use_spec)
        c += float4(0.2, 0.2, 0.2, 0) * pow(max(0, wn.z), 8);
    o.pos = mul(p, wvp);
    o.col = saturate(c);
    o.spec = 0;
    o.uv = 0;
    return o;
}

// vs_2_0: a matrix palette picked by an index (relative addressing).
Out vs20_skin(float4 p : POSITION, float4 w : BLENDWEIGHT, float4 idx : BLENDINDICES, float4 c : COLOR0)
{
    Out o;
    int4 i = (int4)(idx * 255.001);
    float3 q = mul(p, bones[i.x]) * w.x + mul(p, bones[i.y]) * (1 - w.x);
    o.pos = mul(float4(q, 1), wvp);
    o.col = c;
    o.spec = 0;
    o.uv = 0;
    return o;
}

// vs_2_0: a wave from sin, and colours from frac, exp2, log2 and pow.
Out vs20_wave(float4 p : POSITION)
{
    Out o;
    float4 q = p;
    q.y += sin(p.x * params.x + params.y) * 0.25;
    o.pos = mul(q, wvp);
    o.col = float4(frac(p.x * 2.5), exp2(p.z - 1) * 0.5, log2(p.x + 2.1) * 0.4, 1);
    o.spec = pow(abs(q.y), 2).xxxx;
    o.uv = 0;
    return o;
}

struct FogOut {
    float4 pos : POSITION;
    float4 col : COLOR0;
    float fog : FOG;
    float size : PSIZE;
};

// vs_1_1: fog worked out in the shader.
FogOut vs11_fog(float4 p : POSITION, float4 c : COLOR0)
{
    FogOut o;
    o.pos = mul(p, wvp);
    o.col = c;
    o.fog = saturate((params.w - o.pos.w) / (params.w - params.z));
    o.size = 1;
    return o;
}

// vs_1_1: point sizes from the shader.
FogOut vs11_points(float4 p : POSITION, float4 c : COLOR0)
{
    FogOut o;
    o.pos = mul(p, wvp);
    o.col = c;
    o.fog = 1;
    o.size = 2 + c.a * 9;
    return o;
}

// vs_2_0: texture coordinates made from the position.
Out vs20_texgen(float4 p : POSITION)
{
    Out o;
    o.pos = mul(p, wvp);
    o.col = 1;
    o.spec = 0;
    o.uv = mul(p, world).xz * params.xy + params.zw;
    return o;
}

// vs_2_a: dynamic branches and a loop that breaks early.
Out vs2a_dyn(float4 p : POSITION, float3 n : NORMAL)
{
    Out o;
    float4 c = float4(0.1, 0.1, 0.1, 1);
    [branch] if (n.x > 0.2)
        c.r = 0.9;
    else
        c.b = 0.9;
    [loop] for (int i = 0; i < 8; i++) {
        if (i * 0.25 > n.y + 1) break;
        c.g += 0.06;
    }
    o.pos = mul(p, wvp);
    o.col = saturate(c);
    o.spec = 0;
    o.uv = 0;
    return o;
}

// vs_2_0: plain transform (the clip plane test and the streams test).
Out vs20_plain(float4 p : POSITION, float4 c : COLOR0, float2 t : TEXCOORD0)
{
    Out o;
    o.pos = mul(p, wvp);
    o.col = c;
    o.spec = 0;
    o.uv = t;
    return o;
}

// vs_2_0: normalize, cross, sign, abs, lerp, min, max, step.
Out vs20_misc(float4 p : POSITION, float3 n : NORMAL)
{
    Out o;
    float3 a = normalize(n + float3(0.3, 0.1, 0));
    float3 x = cross(a, float3(0, 1, 0));
    float4 c;
    c.r = abs(x.x) + 0.2 * sign(n.z);
    c.g = lerp(0.2, 0.9, step(0.3, a.y));
    c.b = max(min(a.z * 0.8 + 0.3, 0.9), 0.1);
    c.a = 1;
    o.pos = mul(p, wvp);
    o.col = saturate(c);
    o.spec = 0;
    o.uv = 0;
    return o;
}

// vs_2_0: a subroutine called on a constant (call, callnz, ret).
Out vs20_call(float4 p : POSITION, float3 n : NORMAL)
{
    Out o;
    float4 c = float4(0.3, 0.3, 0.3, 1);
    [call] if (use_red)
        c = float4(0.9, 0.2 + n.y * 0.3, 0.2, 1);
    else
        c = float4(0.2, 0.3 + n.y * 0.3, 0.9, 1);
    o.pos = mul(p, wvp);
    o.col = saturate(c);
    o.spec = 0;
    o.uv = 0;
    return o;
}
