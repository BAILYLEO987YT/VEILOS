// d3d5's shaders (G5f2a): cube maps read by Direct3D 9 pixel shaders.
// Compiled by fxc into build/d3d5_*.h (vs20_ is vs_2_0, ps30_ ps_3_0).

struct VOut
{
    float4 pos : POSITION;
    float2 uv : TEXCOORD0;
};

VOut vs20_main(float4 pos : POSITION, float2 uv : TEXCOORD0)
{
    VOut o;
    o.pos = pos;
    o.uv = uv;
    return o;
}

samplerCUBE env : register(s0);
float4 k : register(c0);

float3 dir(float2 uv)
{
    float lon = (uv.x - 0.5) * 6.2831853, lat = (0.5 - uv.y) * 3.1415926;
    return float3(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

float4 ps20_cube(float2 uv : TEXCOORD0) : COLOR
{
    return texCUBE(env, dir(uv));
}

float4 ps30_lod(float2 uv : TEXCOORD0) : COLOR
{
    return texCUBElod(env, float4(dir(uv), k.x));
}
