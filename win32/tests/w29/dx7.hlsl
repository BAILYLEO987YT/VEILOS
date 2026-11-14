// dx7's shaders (G5f2e): deferred contexts and command lists. Compiled by
// fxc into build/dx7_*.h (vs50_ is vs_5_0, ps50_ ps_5_0, cs50_ cs_5_0).

cbuffer Tint : register(b0)
{
    float4 tint;
};

struct VIn
{
    float3 pos : POSITION;
    float4 col : COLOR;
};

struct V
{
    float4 pos : SV_Position;
    float4 col : COLOR;
};

V vs50_main(VIn i)
{
    V o;
    o.pos = float4(i.pos, 1);
    o.col = i.col;
    return o;
}

float4 ps50_main(V i) : SV_Target
{
    return i.col * tint;
}

Texture2D<float4> shown : register(t0);

float4 ps50_show(V i) : SV_Target
{
    return shown.Load(int3(i.pos.xy, 0) % 64);
}

RWTexture2D<float4> img : register(u0);

[numthreads(8, 8, 1)]
void cs50_rings(uint3 id : SV_DispatchThreadID)
{
    float2 d = (float2)id.xy - 31.5;
    float r = sqrt(dot(d, d));
    img[id.xy] = float4(frac(r / 8.0), tint.y, 1.0 - frac(r / 8.0), 1);
}
