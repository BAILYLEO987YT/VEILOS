/* G5f1b test: Direct3D 11 drawing with shaders (SM4): vertex shaders with
 * constant buffers, input layouts and instancing, pixel shaders with
 * textures, math, loops, discard, SV_Position, SV_IsFrontFace, flat
 * inputs, indexing, comparison sampling of a depth buffer, render to
 * texture, depth, blending and stencil.
 *
 * Draws twelve 64x64 panels into a 384x128 back buffer, reads it back and
 * compares it with what WARP (Windows' software renderer) drew; `dx2 gpu`
 * draws on this machine's GPU instead, as a second check.
 *
 *   dx2                 compare with dx2.ref next to the program
 *   dx2 record FILE     write what WARP draws as FILE (a PPM)
 *   dx2 gpu             compare, drawn on the GPU
 *   dx2 stats           as dx2, with each panel's differences counted
 *
 * In each panel at most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/dx2_vs40_main.h"
#include "build/dx2_vs40_inst.h"
#include "build/dx2_ps40_color.h"
#include "build/dx2_ps40_tex.h"
#include "build/dx2_ps40_math.h"
#include "build/dx2_ps40_loop.h"
#include "build/dx2_ps40_pos.h"
#include "build/dx2_ps41_shadow.h"
#include "build/dx2_ps40_flat.h"
#include "build/dx2_ps40_index.h"

#define W 384
#define H 128
#define PANELS 12

typedef struct {
    float x, y, z;
    DWORD c;
    float u, v;
} Vtx;

static unsigned char ref[W * H * 3];
static int stats;
static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static ID3D11Texture2D *back, *zbuf;
static ID3D11RenderTargetView *back_rtv;
static ID3D11DepthStencilView *zdsv;
static ID3D11Buffer *xf, *look;
static ID3D11InputLayout *layout;
static ID3D11VertexShader *vs_main, *vs_inst;

#define RGBA(r, g, b, a) ((DWORD)(r) | (DWORD)(g) << 8 | (DWORD)(b) << 16 | (DWORD)(a) << 24)

static void check(const char *what, HRESULT hr)
{
    if (FAILED(hr))
        printf("%s: 0x%lx\n", what, (unsigned long)hr);
}

static ID3D11Buffer *buffer(UINT bind, const void *data, UINT size, D3D11_USAGE usage)
{
    D3D11_BUFFER_DESC d;
    D3D11_SUBRESOURCE_DATA s;
    ID3D11Buffer *b = NULL;
    memset(&d, 0, sizeof d);
    d.ByteWidth = size; d.Usage = usage; d.BindFlags = bind;
    d.CPUAccessFlags = usage == D3D11_USAGE_DYNAMIC ? D3D11_CPU_ACCESS_WRITE : 0;
    s.pSysMem = data; s.SysMemPitch = 0; s.SysMemSlicePitch = 0;
    check("CreateBuffer", ID3D11Device_CreateBuffer(dev, &d, data ? &s : NULL, &b));
    return b;
}

/* The Xf constant buffer: m (as HLSL's mul(v, m) reads it, column-major
 * in memory), tint, misc. */
static void set_xf(const float m[16], float r, float g, float b, float a)
{
    float data[24];
    int i, j;
    for (i = 0; i < 4; i++)
        for (j = 0; j < 4; j++)
            data[j * 4 + i] = m[i * 4 + j];
    data[16] = r; data[17] = g; data[18] = b; data[19] = a;
    data[20] = data[21] = data[22] = data[23] = 0;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)xf, 0, NULL, data, 0, 0);
}

static const float ident[16] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1};

static void panel(int n)
{
    D3D11_VIEWPORT vp;
    vp.TopLeftX = (float)((n % 6) * 64); vp.TopLeftY = (float)((n / 6) * 64);
    vp.Width = 64; vp.Height = 64; vp.MinDepth = 0; vp.MaxDepth = 1;
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
}

static void draw(const Vtx *v, int n, D3D11_PRIMITIVE_TOPOLOGY t)
{
    ID3D11Buffer *vb = buffer(D3D11_BIND_VERTEX_BUFFER, v, n * sizeof(Vtx), D3D11_USAGE_DEFAULT);
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, t);
    ID3D11DeviceContext_Draw(ctx, n, 0);
    ID3D11Buffer_Release(vb);
}

/* A quad over the panel, uv 0..1, as a triangle list. */
static void quad(DWORD c)
{
    Vtx q[6] = {
        {-1, 1, 0.5f, 0, 0, 0}, {1, 1, 0.5f, 0, 1, 0}, {1, -1, 0.5f, 0, 1, 1},
        {-1, 1, 0.5f, 0, 0, 0}, {1, -1, 0.5f, 0, 1, 1}, {-1, -1, 0.5f, 0, 0, 1},
    };
    int i;
    for (i = 0; i < 6; i++)
        q[i].c = c;
    draw(q, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
}

static ID3D11PixelShader *pshader(const void *code, SIZE_T len)
{
    ID3D11PixelShader *p = NULL;
    check("CreatePixelShader", ID3D11Device_CreatePixelShader(dev, code, len, NULL, &p));
    return p;
}

static ID3D11SamplerState *sampler(D3D11_FILTER f, D3D11_TEXTURE_ADDRESS_MODE a, D3D11_COMPARISON_FUNC cmp)
{
    D3D11_SAMPLER_DESC d;
    ID3D11SamplerState *s = NULL;
    memset(&d, 0, sizeof d);
    d.Filter = f; d.AddressU = d.AddressV = d.AddressW = a; d.MaxAnisotropy = 1;
    d.ComparisonFunc = cmp; d.MinLOD = 0; d.MaxLOD = D3D11_FLOAT32_MAX;
    check("CreateSamplerState", ID3D11Device_CreateSamplerState(dev, &d, &s));
    return s;
}

static ID3D11Texture2D *texture(int w, int h, int mips, DXGI_FORMAT f, UINT bind)
{
    D3D11_TEXTURE2D_DESC d;
    ID3D11Texture2D *t = NULL;
    memset(&d, 0, sizeof d);
    d.Width = w; d.Height = h; d.MipLevels = mips; d.ArraySize = 1; d.Format = f;
    d.SampleDesc.Count = 1; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = bind;
    check("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &d, NULL, &t));
    return t;
}

static ID3D11ShaderResourceView *srv_of(ID3D11Texture2D *t, DXGI_FORMAT f)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC d;
    ID3D11ShaderResourceView *v = NULL;
    memset(&d, 0, sizeof d);
    d.Format = f; d.ViewDimension = D3D11_SRV_DIMENSION_TEXTURE2D; d.Texture2D.MipLevels = (UINT)-1;
    check("CreateShaderResourceView", ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, f ? &d : NULL, &v));
    return v;
}

static ID3D11DepthStencilState *dss(BOOL depth, BOOL stencil, D3D11_COMPARISON_FUNC sf, D3D11_STENCIL_OP pass)
{
    D3D11_DEPTH_STENCIL_DESC d;
    ID3D11DepthStencilState *s = NULL;
    memset(&d, 0, sizeof d);
    d.DepthEnable = depth; d.DepthWriteMask = D3D11_DEPTH_WRITE_MASK_ALL; d.DepthFunc = D3D11_COMPARISON_LESS;
    d.StencilEnable = stencil; d.StencilReadMask = 0xFF; d.StencilWriteMask = 0xFF;
    d.FrontFace.StencilFunc = sf; d.FrontFace.StencilPassOp = pass;
    d.FrontFace.StencilFailOp = d.FrontFace.StencilDepthFailOp = D3D11_STENCIL_OP_KEEP;
    d.BackFace = d.FrontFace;
    check("CreateDepthStencilState", ID3D11Device_CreateDepthStencilState(dev, &d, &s));
    return s;
}

static ID3D11RasterizerState *rs(D3D11_CULL_MODE cull, BOOL ccw)
{
    D3D11_RASTERIZER_DESC d;
    ID3D11RasterizerState *s = NULL;
    memset(&d, 0, sizeof d);
    d.FillMode = D3D11_FILL_SOLID; d.CullMode = cull; d.FrontCounterClockwise = ccw; d.DepthClipEnable = TRUE;
    check("CreateRasterizerState", ID3D11Device_CreateRasterizerState(dev, &d, &s));
    return s;
}

static ID3D11BlendState *blend(BOOL on, UINT8 mask)
{
    D3D11_BLEND_DESC d;
    ID3D11BlendState *s = NULL;
    memset(&d, 0, sizeof d);
    d.RenderTarget[0].BlendEnable = on;
    d.RenderTarget[0].SrcBlend = d.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_SRC_ALPHA;
    d.RenderTarget[0].DestBlend = d.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_INV_SRC_ALPHA;
    d.RenderTarget[0].BlendOp = d.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    d.RenderTarget[0].RenderTargetWriteMask = mask;
    check("CreateBlendState", ID3D11Device_CreateBlendState(dev, &d, &s));
    return s;
}

static void targets(BOOL depth)
{
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &back_rtv, depth ? zdsv : NULL);
}

static void panels(void)
{
    ID3D11PixelShader *ps_color = pshader(ps40_color, sizeof ps40_color), *ps_tex = pshader(ps40_tex, sizeof ps40_tex);
    ID3D11PixelShader *ps_math = pshader(ps40_math, sizeof ps40_math), *ps_loop = pshader(ps40_loop, sizeof ps40_loop);
    ID3D11PixelShader *ps_pos = pshader(ps40_pos, sizeof ps40_pos), *ps_shadow = pshader(ps41_shadow, sizeof ps41_shadow);
    ID3D11PixelShader *ps_flat = pshader(ps40_flat, sizeof ps40_flat), *ps_index = pshader(ps40_index, sizeof ps40_index);
    ID3D11RasterizerState *none = rs(D3D11_CULL_NONE, FALSE);
    ID3D11SamplerState *lin = sampler(D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_WRAP, D3D11_COMPARISON_NEVER);
    ID3D11SamplerState *clamp = sampler(D3D11_FILTER_MIN_MAG_MIP_LINEAR, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_COMPARISON_NEVER);
    ID3D11SamplerState *cmp = sampler(D3D11_FILTER_COMPARISON_MIN_MAG_LINEAR_MIP_POINT, D3D11_TEXTURE_ADDRESS_CLAMP, D3D11_COMPARISON_LESS);
    float clear1[4] = {0.05f, 0.05f, 0.05f, 1};
    int i, x, y;

    ID3D11DeviceContext_ClearRenderTargetView(ctx, back_rtv, clear1);
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_VSSetShader(ctx, vs_main, NULL, 0);
    ID3D11DeviceContext_VSSetConstantBuffers(ctx, 0, 1, &xf);
    ID3D11DeviceContext_VSSetConstantBuffers(ctx, 1, 1, &look);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &xf);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 1, 1, &look);
    targets(FALSE);
    set_xf(ident, 1, 1, 1, 1);

    /* 0: a Gouraud triangle. */
    {
        Vtx t[3] = {{0, 0.9f, 0.5f, RGBA(255, 0, 0, 255), 0, 0}, {0.85f, -0.8f, 0.5f, RGBA(0, 255, 0, 255), 0, 0}, {-0.9f, -0.6f, 0.5f, RGBA(0, 0, 255, 255), 0, 0}};
        panel(0);
        ID3D11DeviceContext_PSSetShader(ctx, ps_color, NULL, 0);
        draw(t, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    }
    /* 1: an indexed quad turned by the constant buffer's matrix; its pixel
     * shader indexes a constant buffer and a table. */
    {
        Vtx q[4] = {{-0.7f, 0.7f, 0.5f, RGBA(255, 255, 255, 255), 0, 0}, {0.7f, 0.7f, 0.5f, RGBA(255, 255, 255, 255), 1, 0},
                    {0.7f, -0.7f, 0.5f, RGBA(255, 255, 255, 255), 1, 1}, {-0.7f, -0.7f, 0.5f, RGBA(255, 255, 255, 255), 0, 1}};
        WORD idx[6] = {0, 1, 2, 0, 2, 3};
        float m[16] = {0.8660254f, 0.5f, 0, 0, -0.5f, 0.8660254f, 0, 0, 0, 0, 1, 0, 0.05f, -0.05f, 0, 1};
        ID3D11Buffer *vb = buffer(D3D11_BIND_VERTEX_BUFFER, q, sizeof q, D3D11_USAGE_DEFAULT);
        ID3D11Buffer *ib = buffer(D3D11_BIND_INDEX_BUFFER, idx, sizeof idx, D3D11_USAGE_DEFAULT);
        UINT stride = sizeof(Vtx), off = 0;
        set_xf(m, 0.9f, 0.8f, 0.6f, 1);
        panel(1);
        ID3D11DeviceContext_PSSetShader(ctx, ps_index, NULL, 0);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &off);
        ID3D11DeviceContext_IASetIndexBuffer(ctx, ib, DXGI_FORMAT_R16_UINT, 0);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_DrawIndexed(ctx, 6, 0, 0);
        ID3D11Buffer_Release(vb);
        ID3D11Buffer_Release(ib);
        set_xf(ident, 1, 1, 1, 1);
    }
    /* 2: a mipmapped texture on a floor in perspective. */
    {
        static DWORD px[64 * 64];
        ID3D11Texture2D *t = texture(64, 64, 7, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_SHADER_RESOURCE);
        ID3D11ShaderResourceView *v;
        float m[16] = {1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0.25f, 1, 0, 0, 0, 0};
        Vtx f[6] = {
            {-3, 3, 3, 0, 0, 0}, {3, 3, 3, 0, 4, 0}, {1, -1, 1, 0, 4, 8},
            {-3, 3, 3, 0, 0, 0}, {1, -1, 1, 0, 4, 8}, {-1, -1, 1, 0, 0, 8},
        };
        int l;
        for (l = 0; l < 7; l++) {
            int s = 64 >> l;
            DWORD tints[7] = {0, 0x00300000, 0x00003000, 0x00000030, 0x00303000, 0x00003030, 0x00300030};
            for (y = 0; y < s; y++)
                for (x = 0; x < s; x++)
                    px[y * s + x] = (((x * 8 / s) + (y * 8 / s)) & 1 ? RGBA(230, 220, 200, 255) : RGBA(40, 60, 120, 255)) + tints[l];
            ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)t, l, NULL, px, s * 4, 0);
        }
        v = srv_of(t, 0);
        for (i = 0; i < 6; i++)
            f[i].c = RGBA(255, 255, 255, 255);
        set_xf(m, 1, 1, 1, 1);
        panel(2);
        ID3D11DeviceContext_PSSetShader(ctx, ps_tex, NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &v);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &lin);
        draw(f, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        set_xf(ident, 1, 1, 1, 1);
        ID3D11ShaderResourceView_Release(v);
        ID3D11Texture2D_Release(t);
    }
    /* 3: two triangles through each other, depth tested. */
    {
        Vtx t[6] = {
            {-0.9f, 0.8f, 0.1f, RGBA(240, 120, 40, 255), 0, 0}, {0.9f, 0.1f, 0.9f, RGBA(240, 120, 40, 255), 0, 0}, {-0.9f, -0.6f, 0.1f, RGBA(240, 200, 40, 255), 0, 0},
            {0.9f, 0.8f, 0.1f, RGBA(40, 120, 240, 255), 0, 0}, {0.9f, -0.6f, 0.1f, RGBA(40, 200, 240, 255), 0, 0}, {-0.9f, 0.1f, 0.9f, RGBA(40, 120, 240, 255), 0, 0},
        };
        ID3D11DepthStencilState *z = dss(TRUE, FALSE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_KEEP);
        ID3D11DeviceContext_ClearDepthStencilView(ctx, zdsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL, 1, 0);
        targets(TRUE);
        ID3D11DeviceContext_OMSetDepthStencilState(ctx, z, 0);
        ID3D11DeviceContext_RSSetState(ctx, none);
        panel(3);
        ID3D11DeviceContext_PSSetShader(ctx, ps_color, NULL, 0);
        draw(t, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_OMSetDepthStencilState(ctx, NULL, 0);
        ID3D11DeviceContext_RSSetState(ctx, NULL);
        targets(FALSE);
        ID3D11DepthStencilState_Release(z);
    }
    /* 4: blending over stripes. */
    {
        Vtx t[3] = {{-0.8f, 0.8f, 0.5f, RGBA(255, 40, 40, 160), 0, 0}, {0.8f, 0.6f, 0.5f, RGBA(40, 255, 40, 60), 0, 0}, {0, -0.9f, 0.5f, RGBA(255, 255, 40, 230), 0, 0}};
        Vtx s[6];
        ID3D11BlendState *b = blend(TRUE, 0xF);
        panel(4);
        ID3D11DeviceContext_PSSetShader(ctx, ps_color, NULL, 0);
        for (i = 0; i < 4; i++) {
            float x0 = -1 + i * 0.5f, x1 = x0 + 0.25f;
            Vtx a = {x0, 1, 0.5f, RGBA(60 * i, 80, 200 - 40 * i, 255), 0, 0};
            s[0] = a; s[1] = a; s[1].x = x1; s[2] = a; s[2].x = x1; s[2].y = -1;
            s[3] = a; s[4] = s[2]; s[5] = a; s[5].y = -1;
            draw(s, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        }
        ID3D11DeviceContext_OMSetBlendState(ctx, b, NULL, 0xFFFFFFFF);
        draw(t, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_OMSetBlendState(ctx, NULL, NULL, 0xFFFFFFFF);
        ID3D11BlendState_Release(b);
    }
    /* 5-7: arithmetic, a loop, discard and the pixel's position. */
    panel(5);
    ID3D11DeviceContext_PSSetShader(ctx, ps_math, NULL, 0);
    quad(RGBA(255, 255, 255, 255));
    panel(6);
    ID3D11DeviceContext_PSSetShader(ctx, ps_loop, NULL, 0);
    quad(RGBA(255, 255, 255, 255));
    panel(7);
    ID3D11DeviceContext_PSSetShader(ctx, ps_pos, NULL, 0);
    quad(RGBA(255, 128, 255, 255));
    /* 6+: the look-up colours. */
    {
        float lk[20] = {0.9f, 0.3f, 0.2f, 1, 0.2f, 0.8f, 0.3f, 1, 0.3f, 0.4f, 0.9f, 1, 0.9f, 0.8f, 0.2f, 1, 0.5f, 0.75f, 1, 1};
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)look, 0, NULL, lk, 0, 0);
    }
    /* 8: an instanced strip: four instances, offsets per instance. */
    {
        Vtx q[4] = {{-1, 1, 0.5f, 0, 0, 0}, {1, 1, 0.5f, 0, 1, 0}, {-1, -1, 0.5f, 0, 0, 1}, {1, -0.6f, 0.5f, 0, 1, 1}};
        float offs[8] = {-0.5f, 0.5f, 0.5f, 0.5f, -0.5f, -0.5f, 0.55f, -0.45f};
        ID3D11Buffer *vb[2];
        UINT stride[2] = {sizeof(Vtx), 8}, off[2] = {0, 0};
        D3D11_INPUT_ELEMENT_DESC e[4] = {
            {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
            {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0},
            {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0},
            {"OFFSET", 0, DXGI_FORMAT_R32G32_FLOAT, 1, 0, D3D11_INPUT_PER_INSTANCE_DATA, 1},
        };
        ID3D11InputLayout *il = NULL;
        check("CreateInputLayout (instanced)", ID3D11Device_CreateInputLayout(dev, e, 4, vs40_inst, sizeof vs40_inst, &il));
        vb[0] = buffer(D3D11_BIND_VERTEX_BUFFER, q, sizeof q, D3D11_USAGE_DEFAULT);
        vb[1] = buffer(D3D11_BIND_VERTEX_BUFFER, offs, sizeof offs, D3D11_USAGE_DEFAULT);
        panel(8);
        ID3D11DeviceContext_IASetInputLayout(ctx, il);
        ID3D11DeviceContext_VSSetShader(ctx, vs_inst, NULL, 0);
        ID3D11DeviceContext_PSSetShader(ctx, ps_color, NULL, 0);
        ID3D11DeviceContext_RSSetState(ctx, none);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 2, vb, stride, off);
        ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
        ID3D11DeviceContext_DrawInstanced(ctx, 4, 4, 0, 0);
        ID3D11DeviceContext_RSSetState(ctx, NULL);
        ID3D11DeviceContext_IASetInputLayout(ctx, layout);
        ID3D11DeviceContext_VSSetShader(ctx, vs_main, NULL, 0);
        ID3D11Buffer_Release(vb[0]);
        ID3D11Buffer_Release(vb[1]);
        ID3D11InputLayout_Release(il);
    }
    /* 9: a depth buffer drawn with no render target, then compared with
     * (percentage closer filtering) and loaded. */
    {
        D3D11_TEXTURE2D_DESC d;
        D3D11_DEPTH_STENCIL_VIEW_DESC dd;
        ID3D11Texture2D *t;
        ID3D11DepthStencilView *dv = NULL;
        ID3D11ShaderResourceView *sv;
        D3D11_VIEWPORT vp = {0, 0, 64, 64, 0, 1};
        Vtx occ[3] = {{-0.8f, 0.9f, 0.2f, 0, 0, 0}, {0.9f, 0.3f, 0.7f, 0, 0, 0}, {-0.3f, -0.9f, 0.45f, 0, 0, 0}};
        Vtx q[6] = {
            {-1, 1, 0.5f, RGBA(0, 0, 0, 255), 0, 0}, {1, 1, 0.5f, RGBA(255, 0, 0, 255), 1, 0}, {1, -1, 0.5f, RGBA(255, 0, 0, 255), 1, 1},
            {-1, 1, 0.5f, RGBA(0, 0, 0, 255), 0, 0}, {1, -1, 0.5f, RGBA(255, 0, 0, 255), 1, 1}, {-1, -1, 0.5f, RGBA(0, 0, 0, 255), 0, 1},
        };
        memset(&d, 0, sizeof d);
        d.Width = 64; d.Height = 64; d.MipLevels = 1; d.ArraySize = 1; d.Format = DXGI_FORMAT_R24G8_TYPELESS;
        d.SampleDesc.Count = 1; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = D3D11_BIND_DEPTH_STENCIL | D3D11_BIND_SHADER_RESOURCE;
        check("CreateTexture2D (depth)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &t));
        memset(&dd, 0, sizeof dd);
        dd.Format = DXGI_FORMAT_D24_UNORM_S8_UINT; dd.ViewDimension = D3D11_DSV_DIMENSION_TEXTURE2D;
        check("CreateDepthStencilView", ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)t, &dd, &dv));
        ID3D11DeviceContext_ClearDepthStencilView(ctx, dv, D3D11_CLEAR_DEPTH, 1, 0);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 0, NULL, dv);
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_PSSetShader(ctx, NULL, NULL, 0);
        draw(occ, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        targets(FALSE);
        sv = srv_of(t, DXGI_FORMAT_R24_UNORM_X8_TYPELESS);
        panel(9);
        ID3D11DeviceContext_PSSetShader(ctx, ps_shadow, NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &sv);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &cmp);
        draw(q, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        {
            ID3D11ShaderResourceView *nil = NULL;
            ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &nil);
        }
        ID3D11ShaderResourceView_Release(sv);
        ID3D11DepthStencilView_Release(dv);
        ID3D11Texture2D_Release(t);
    }
    /* 10: drawn into a texture, then that texture drawn magnified. */
    {
        ID3D11Texture2D *t = texture(32, 32, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE);
        ID3D11RenderTargetView *rv = NULL;
        ID3D11ShaderResourceView *sv;
        D3D11_VIEWPORT vp = {0, 0, 32, 32, 0, 1};
        float c[4] = {0.9f, 0.9f, 0.85f, 1};
        Vtx tri[3] = {{0, 0.9f, 0.5f, RGBA(200, 0, 60, 255), 0, 0}, {0.9f, -0.9f, 0.5f, RGBA(0, 160, 60, 255), 0, 0}, {-0.9f, -0.9f, 0.5f, RGBA(20, 40, 220, 255), 0, 0}};
        check("CreateRenderTargetView", ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t, NULL, &rv));
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rv, c);
        ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rv, NULL);
        ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
        ID3D11DeviceContext_PSSetShader(ctx, ps_color, NULL, 0);
        draw(tri, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        targets(FALSE);
        sv = srv_of(t, 0);
        panel(10);
        ID3D11DeviceContext_PSSetShader(ctx, ps_tex, NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &sv);
        ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &clamp);
        quad(RGBA(255, 255, 255, 255));
        ID3D11ShaderResourceView_Release(sv);
        ID3D11RenderTargetView_Release(rv);
        ID3D11Texture2D_Release(t);
    }
    /* 11: stencil written by a diamond (no colour), then two triangles,
     * one each way round, flat-shaded, where it's set. */
    {
        Vtx dia[6] = {{0, 0.95f, 0.5f, 0, 0, 0}, {0.95f, 0, 0.5f, 0, 0, 0}, {0, -0.95f, 0.5f, 0, 0, 0}, {0, 0.95f, 0.5f, 0, 0, 0}, {0, -0.95f, 0.5f, 0, 0, 0}, {-0.95f, 0, 0.5f, 0, 0, 0}};
        Vtx two[6] = {
            {-1, 1, 0.5f, RGBA(250, 120, 30, 255), 0, 0}, {1, 1, 0.5f, RGBA(0, 0, 0, 255), 0, 0}, {-1, -1, 0.5f, RGBA(0, 0, 0, 255), 0, 0},
            {1, 1, 0.5f, RGBA(40, 160, 250, 255), 0, 0}, {-1, -1, 0.5f, RGBA(0, 0, 0, 255), 0, 0}, {1, -1, 0.5f, RGBA(0, 0, 0, 255), 0, 0},
        };
        ID3D11DepthStencilState *wr = dss(FALSE, TRUE, D3D11_COMPARISON_ALWAYS, D3D11_STENCIL_OP_REPLACE);
        ID3D11DepthStencilState *eq = dss(FALSE, TRUE, D3D11_COMPARISON_EQUAL, D3D11_STENCIL_OP_KEEP);
        ID3D11BlendState *off = blend(FALSE, 0);
        ID3D11DeviceContext_ClearDepthStencilView(ctx, zdsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL, 1, 0);
        targets(TRUE);
        panel(11);
        ID3D11DeviceContext_RSSetState(ctx, none);
        ID3D11DeviceContext_PSSetShader(ctx, ps_color, NULL, 0);
        ID3D11DeviceContext_OMSetBlendState(ctx, off, NULL, 0xFFFFFFFF);
        ID3D11DeviceContext_OMSetDepthStencilState(ctx, wr, 1);
        draw(dia, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_OMSetBlendState(ctx, NULL, NULL, 0xFFFFFFFF);
        ID3D11DeviceContext_OMSetDepthStencilState(ctx, eq, 1);
        ID3D11DeviceContext_PSSetShader(ctx, ps_flat, NULL, 0);
        draw(two, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        ID3D11DeviceContext_OMSetDepthStencilState(ctx, NULL, 0);
        ID3D11DeviceContext_RSSetState(ctx, NULL);
        targets(FALSE);
        ID3D11DepthStencilState_Release(wr);
        ID3D11DepthStencilState_Release(eq);
        ID3D11BlendState_Release(off);
    }

    ID3D11PixelShader_Release(ps_color); ID3D11PixelShader_Release(ps_tex); ID3D11PixelShader_Release(ps_math);
    ID3D11PixelShader_Release(ps_loop); ID3D11PixelShader_Release(ps_pos); ID3D11PixelShader_Release(ps_shadow);
    ID3D11PixelShader_Release(ps_flat); ID3D11PixelShader_Release(ps_index);
    ID3D11RasterizerState_Release(none);
    ID3D11SamplerState_Release(lin); ID3D11SamplerState_Release(clamp); ID3D11SamplerState_Release(cmp);
}

/* What creating and binding shaders gives back. */
static void checks(void)
{
    ID3D11PixelShader *p = NULL;
    ID3D11VertexShader *v = NULL;
    ID3D11ClassInstance *ci[1];
    UINT n = 1;
    static const BYTE junk[64] = {'D', 'X', 'B', 'C'};
    printf("vertex shader from pixel shader code: 0x%lx\n", (unsigned long)ID3D11Device_CreateVertexShader(dev, ps40_color, sizeof ps40_color, NULL, &v));
    printf("pixel shader from junk: 0x%lx\n", (unsigned long)ID3D11Device_CreatePixelShader(dev, junk, sizeof junk, NULL, &p));
    printf("pixel shader, no out: 0x%lx\n", (unsigned long)ID3D11Device_CreatePixelShader(dev, ps40_color, sizeof ps40_color, NULL, NULL));
    ID3D11DeviceContext_VSSetShader(ctx, vs_main, NULL, 0);
    ID3D11DeviceContext_VSGetShader(ctx, &v, ci, &n);
    printf("VSGetShader: %s, %u class instances\n", v == vs_main ? "the one set" : "another", n);
    if (v)
        ID3D11VertexShader_Release(v);
}

static int compare(const unsigned char *img)
{
    int n, bad = 0;
    for (n = 0; n < PANELS; n++) {
        int x0 = (n % 6) * 64, y0 = (n / 6) * 64, x, y, off = 0, worst = 0, any = 0;
        for (y = y0; y < y0 + 64; y++)
            for (x = x0; x < x0 + 64; x++) {
                int i = (y * W + x) * 3, k, d = 0;
                for (k = 0; k < 3; k++) {
                    int e = abs(img[i + k] - ref[i + k]);
                    if (e > d) d = e;
                }
                if (d > 0) any++;
                if (d > 4) off++;
                if (d > worst) worst = d;
            }
        printf("panel %d: %s\n", n, off <= 40 ? "ok" : "FAIL");
        if (off > 40) bad++;
        if (stats)
            printf("  %d pixels differ, %d by more than 4, largest difference %d\n", any, off, worst);
    }
    return bad;
}

static int load_ref(const char *path)
{
    char hdr[32];
    FILE *f = fopen(path, "rb");
    int ok;
    if (!f) return 0;
    ok = fgets(hdr, sizeof hdr, f) && strcmp(hdr, "P6\n") == 0
        && fgets(hdr, sizeof hdr, f) && strcmp(hdr, "384 128\n") == 0
        && fgets(hdr, sizeof hdr, f) && strcmp(hdr, "255\n") == 0
        && fread(ref, 1, sizeof ref, f) == sizeof ref;
    fclose(f);
    return ok;
}

int main(int argc, char **argv)
{
    static unsigned char img[W * H * 3];
    D3D_FEATURE_LEVEL want[2] = {D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0}, got;
    D3D_DRIVER_TYPE driver = D3D_DRIVER_TYPE_WARP;
    D3D11_INPUT_ELEMENT_DESC e[3] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    D3D11_DEPTH_STENCIL_VIEW_DESC dd;
    ID3D11Texture2D *st;
    D3D11_MAPPED_SUBRESOURCE mp;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    if (argc > 1 && strcmp(argv[1], "gpu") == 0)
        driver = D3D_DRIVER_TYPE_HARDWARE;
    hr = D3D11CreateDevice(NULL, driver, NULL, 0, want, 2, D3D11_SDK_VERSION, &dev, &got, &ctx);
    printf("device: %s, feature level 0x%x\n", SUCCEEDED(hr) ? "created" : "FAILED", (int)got);
    if (FAILED(hr))
        return 1;
    back = texture(W, H, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_RENDER_TARGET);
    zbuf = texture(W, H, 1, DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_BIND_DEPTH_STENCIL);
    check("CreateRenderTargetView", ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &back_rtv));
    memset(&dd, 0, sizeof dd);
    dd.Format = DXGI_FORMAT_D24_UNORM_S8_UINT; dd.ViewDimension = D3D11_DSV_DIMENSION_TEXTURE2D;
    check("CreateDepthStencilView", ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)zbuf, &dd, &zdsv));
    xf = buffer(D3D11_BIND_CONSTANT_BUFFER, NULL, 96, D3D11_USAGE_DEFAULT);
    look = buffer(D3D11_BIND_CONSTANT_BUFFER, NULL, 80, D3D11_USAGE_DEFAULT);
    {
        float lk[20] = {1, 0.5f, 0.25f, 1, 0.25f, 1, 0.5f, 1, 0.5f, 0.25f, 1, 1, 1, 1, 0.25f, 1, 0.8f, 0.6f, 0.9f, 1};
        ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)look, 0, NULL, lk, 0, 0);
    }
    check("CreateVertexShader", ID3D11Device_CreateVertexShader(dev, vs40_main, sizeof vs40_main, NULL, &vs_main));
    check("CreateVertexShader (instanced)", ID3D11Device_CreateVertexShader(dev, vs40_inst, sizeof vs40_inst, NULL, &vs_inst));
    check("CreateInputLayout", ID3D11Device_CreateInputLayout(dev, e, 3, vs40_main, sizeof vs40_main, &layout));
    panels();
    checks();

    {
        D3D11_TEXTURE2D_DESC d;
        ID3D11Texture2D_GetDesc(back, &d);
        d.Usage = D3D11_USAGE_STAGING; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ; d.BindFlags = 0;
        check("CreateTexture2D (staging)", ID3D11Device_CreateTexture2D(dev, &d, NULL, &st));
    }
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)back);
    ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &mp);
    for (y = 0; y < H; y++)
        for (x = 0; x < W; x++) {
            unsigned char *p = (unsigned char *)mp.pData + y * mp.RowPitch + x * 4, *q = img + (y * W + x) * 3;
            q[0] = p[0]; q[1] = p[1]; q[2] = p[2];
        }
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
    ID3D11Texture2D_Release(st);
    ID3D11DeviceContext_ClearState(ctx);
    ID3D11InputLayout_Release(layout);
    ID3D11VertexShader_Release(vs_main);
    ID3D11VertexShader_Release(vs_inst);
    ID3D11Buffer_Release(xf);
    ID3D11Buffer_Release(look);
    ID3D11RenderTargetView_Release(back_rtv);
    ID3D11DepthStencilView_Release(zdsv);
    ID3D11Texture2D_Release(back);
    ID3D11Texture2D_Release(zbuf);
    ID3D11DeviceContext_Release(ctx);
    ID3D11Device_Release(dev);

    if (argc > 2 && strcmp(argv[1], "record") == 0) {
        FILE *fo = fopen(argv[2], "wb");
        if (!fo) {
            printf("can't write %s\n", argv[2]);
            return 1;
        }
        fprintf(fo, "P6\n384 128\n255\n");
        fwrite(img, 1, sizeof img, fo);
        fclose(fo);
        printf("recorded\n");
        return 0;
    }
    GetModuleFileNameA(NULL, path, sizeof path);
    slash = strrchr(path, '\\');
    if (!slash) slash = strrchr(path, '/');
    strcpy(slash ? slash + 1 : path, "dx2.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
