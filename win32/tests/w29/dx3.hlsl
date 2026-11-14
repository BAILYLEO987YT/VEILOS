// dx3's shaders (G5f2a): cube, 3D and array textures. Compiled by fxc
// into build/dx3_*.h (vs40_ is vs_4_0, ps41_ ps_4_1).

cbuffer P : register(b0)
{
    float4 k;
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
    o.pos = float4(i.pos, 1);
    o.col = i.col;
    o.uv = i.uv;
    return o;
}

SamplerState s0 : register(s0);
TextureCube cube : register(t0);
Texture3D vol : register(t1);
Texture2DArray arr : register(t2);
Texture1DArray arr1 : register(t3);
TextureCubeArray cubes : register(t4);

// A direction for each pixel: longitude across, latitude down.
float3 dir(float2 uv)
{
    float lon = (uv.x - 0.5) * 6.2831853, lat = (0.5 - uv.y) * 3.1415926;
    return float3(cos(lat) * sin(lon), sin(lat), cos(lat) * cos(lon));
}

float4 ps40_cube(VOut i) : SV_Target
{
    return cube.SampleLevel(s0, dir(i.uv), 0);
}

float4 ps40_cubelod(VOut i) : SV_Target
{
    return cube.SampleLevel(s0, dir(i.uv), k.x);
}

float4 ps40_vol(VOut i) : SV_Target
{
    return vol.SampleLevel(s0, float3(i.uv, i.uv.x * 0.8 + 0.1), k.y);
}

float4 ps40_volload(VOut i) : SV_Target
{
    int3 p = int3(i.uv * 16.0, 0);
    p.z = (int)(i.uv.y * 8.0) ^ (p.x & 3);
    return vol.Load(int4(p, 0));
}

float4 ps40_arr(VOut i) : SV_Target
{
    return arr.SampleLevel(s0, float3(i.uv * 2.0, floor(i.uv.x * 2.0) + 2.0 * floor(i.uv.y * 2.0)), 0);
}

float4 ps40_arrload(VOut i) : SV_Target
{
    int2 p = int2(i.uv * 32.0);
    return arr.Load(int4(p, (p.x + p.y) >> 3, 0));
}

float4 ps40_arr1(VOut i) : SV_Target
{
    return arr1.SampleLevel(s0, float2(i.uv.x, i.uv.y * 4.0), 0);
}

float4 ps40_dims(VOut i) : SV_Target
{
    uint w, h, n, d, l, ew, eh, ed, el;
    cube.GetDimensions(0, w, h, l);
    vol.GetDimensions(1, ew, eh, ed, el);
    arr.GetDimensions(0, d, n, n, el);
    float4 a = float4(w / 64.0, l / 8.0, ew / 16.0, ed / 8.0);
    float4 b = float4(d / 64.0, n / 8.0, el / 8.0, eh / 16.0);
    return i.uv.x < 0.5 ? a : b;
}

float4 ps41_cubes(VOut i) : SV_Target
{
    return cubes.SampleLevel(s0, float4(dir(i.uv), i.uv.y < 0.5 ? 0 : 1), 0);
}
