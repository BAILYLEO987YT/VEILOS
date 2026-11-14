// dx4's shaders (G5f2b): compute shaders at feature level 11_0, with
// unordered access views, group shared memory, barriers and atomics.
// Compiled by fxc into build/dx4_*.h (cs50_ is cs_5_0, ps50_ ps_5_0).

cbuffer P : register(b0)
{
    uint4 n;
    float4 f;
};

// A texture written per thread: a pattern from the thread's ids.
RWTexture2D<float4> img : register(u0);

[numthreads(8, 8, 1)]
void cs50_pattern(uint3 id : SV_DispatchThreadID, uint3 g : SV_GroupID, uint3 l : SV_GroupThreadID, uint li : SV_GroupIndex)
{
    float2 p = (float2)id.xy / 64.0;
    float v = sin(p.x * 9.0 + f.x) * cos(p.y * 7.0);
    img[id.xy] = float4(v * 0.5 + 0.5, (float)((g.x + g.y) & 1) * 0.5 + p.y * 0.5, li / 64.0, 1);
}

// A structured buffer: each element squared and offset, a byte address
// buffer filled with its index pattern.
struct Item
{
    float a;
    uint b;
    float2 c;
};
RWStructuredBuffer<Item> items : register(u1);
RWByteAddressBuffer raw : register(u2);

[numthreads(64, 1, 1)]
void cs50_buffers(uint3 id : SV_DispatchThreadID)
{
    Item it = items[id.x];
    it.a = it.a * it.a + 1.0;
    it.b = it.b * 3u + id.x;
    it.c = float2(it.c.y, -it.c.x);
    items[id.x] = it;
    raw.Store2(id.x * 8, uint2(id.x * id.x, ~id.x));
}

// A reduction through group shared memory, with barriers.
groupshared float sums[128];
StructuredBuffer<float> values : register(t0);
RWStructuredBuffer<float> totals : register(u1);

[numthreads(128, 1, 1)]
void cs50_reduce(uint3 g : SV_GroupID, uint li : SV_GroupIndex)
{
    sums[li] = values[g.x * 128 + li];
    GroupMemoryBarrierWithGroupSync();
    [unroll] for (uint s = 64; s > 0; s >>= 1) {
        if (li < s)
            sums[li] += sums[li + s];
        GroupMemoryBarrierWithGroupSync();
    }
    if (li == 0)
        totals[g.x] = sums[0];
}

// A histogram by atomics, and the old values InterlockedAdd gives back.
Buffer<uint> keys : register(t1);
RWByteAddressBuffer hist : register(u2);
RWBuffer<uint> order : register(u3);
RWStructuredBuffer<uint> olds : register(u4);

[numthreads(32, 1, 1)]
void cs50_hist(uint3 id : SV_DispatchThreadID)
{
    uint k = keys[id.x] & 15u, old;
    hist.InterlockedAdd(k * 4, 1u, old);
    olds[id.x] = old;
    InterlockedMax(order[k], id.x);
    uint mn;
    hist.InterlockedMin(64 + k * 4, id.x + 1u, mn);
}

// A pixel shader at 5.0 reading what the compute shaders wrote.
Texture2D<float4> shown : register(t0);
SamplerState s0 : register(s0);

struct VOut
{
    float4 pos : SV_Position;
    float2 uv : TEXCOORD0;
};

VOut vs50_main(float3 pos : POSITION, float2 uv : TEXCOORD0)
{
    VOut o;
    o.pos = float4(pos, 1);
    o.uv = uv;
    return o;
}

float4 ps50_show(VOut i) : SV_Target
{
    return shown.Sample(s0, i.uv);
}
