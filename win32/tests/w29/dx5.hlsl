// dx5's shaders (G5f2c): geometry shaders and stream output. Compiled by
// fxc into build/dx5_*.h (vs40_ is vs_4_0, gs40_ gs_4_0, ps40_ ps_4_0).

struct V
{
    float4 pos : SV_Position;
    float4 col : COLOR;
    float2 uv : TEXCOORD0;
};

struct VIn
{
    float3 pos : POSITION;
    float4 col : COLOR;
    float2 uv : TEXCOORD0;
};

V vs40_main(VIn i)
{
    V o;
    o.pos = float4(i.pos, 1);
    o.col = i.col;
    o.uv = i.uv;
    return o;
}

// Turned and shrunk: what stream output keeps.
V vs40_turn(VIn i)
{
    V o;
    o.pos = float4(i.pos.y * 0.8, -i.pos.x * 0.8, i.pos.z, 1);
    o.col = i.col.bgra;
    o.uv = i.uv * 2.0;
    return o;
}

float4 ps40_main(V i) : SV_Target
{
    return i.col * float4(0.6 + 0.4 * frac(i.uv * 2.0), 1, 1);
}

// Each point a square, its corners' uv 0..1.
[maxvertexcount(4)]
void gs40_sprite(point V p[1], inout TriangleStream<V> s)
{
    float2 d[4] = { float2(-1, 1), float2(1, 1), float2(-1, -1), float2(1, -1) };
    [unroll] for (int i = 0; i < 4; i++) {
        V o = p[0];
        o.pos.xy += d[i] * 0.18;
        o.uv = d[i] * 0.5 + 0.5;
        s.Append(o);
    }
}

// Each triangle three smaller ones toward its corners, coloured by the
// primitive's number.
[maxvertexcount(9)]
void gs40_explode(triangle V t[3], uint id : SV_PrimitiveID, inout TriangleStream<V> s)
{
    float4 c = (t[0].pos + t[1].pos + t[2].pos) / 3.0;
    [unroll] for (int k = 0; k < 3; k++) {
        [unroll] for (int j = 0; j < 3; j++) {
            V o = t[j];
            o.pos = lerp(c, t[j].pos, 0.45) + (t[k].pos - c) * 0.5;
            o.col = float4((id & 1) ? 1.0 : 0.3, (id & 2) ? 1.0 : 0.3, k / 2.0, 1);
            s.Append(o);
        }
        s.RestartStrip();
    }
}

// Triangles with adjacency: the triangle, tinted by its neighbours.
[maxvertexcount(3)]
void gs40_adj(triangleadj V t[6], inout TriangleStream<V> s)
{
    float4 n = (t[1].col + t[3].col + t[5].col) / 3.0;
    [unroll] for (int i = 0; i < 6; i += 2) {
        V o = t[i];
        o.col = lerp(t[i].col, n, 0.5);
        s.Append(o);
    }
}
