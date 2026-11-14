/* G5f2c test: Direct3D 11 geometry shaders and stream output: points made
 * squares, triangles exploded by SV_PrimitiveID, triangles with adjacency,
 * a vertex shader's output streamed out (not rasterized) and drawn back by
 * DrawAuto, a geometry shader streaming out while it draws, and stream
 * output appended across two draws.
 *
 * Draws seven 64x64 panels of a 384x128 target and prints what the stream
 * output buffers hold; compared with what WARP drew and printed on Windows.
 * `dx5 gpu` runs on this machine's GPU instead.
 *
 *   dx5                 compare with dx5.ref next to the program
 *   dx5 record FILE     write what WARP draws as FILE (a PPM)
 *   dx5 gpu             compare, drawn on the GPU
 *   dx5 stats           as dx5, with each panel's differences counted
 *
 * In each panel at most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/dx5_vs40_main.h"
#include "build/dx5_vs40_turn.h"
#include "build/dx5_ps40_main.h"
#include "build/dx5_gs40_sprite.h"
#include "build/dx5_gs40_explode.h"
#include "build/dx5_gs40_adj.h"

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
static ID3D11InputLayout *layout, *so_layout;

#define RGBA(r, g, b, a) ((DWORD)(r) | (DWORD)(g) << 8 | (DWORD)(b) << 16 | (DWORD)(a) << 24)

static void check(const char *what, HRESULT hr)
{
    if (FAILED(hr))
        printf("%s: 0x%lx\n", what, (unsigned long)hr);
}

static ID3D11Buffer *buffer(UINT bind, const void *data, UINT size)
{
    D3D11_BUFFER_DESC d;
    D3D11_SUBRESOURCE_DATA s;
    ID3D11Buffer *b = NULL;
    memset(&d, 0, sizeof d);
    d.ByteWidth = size; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = bind;
    s.pSysMem = data; s.SysMemPitch = 0; s.SysMemSlicePitch = 0;
    check("CreateBuffer", ID3D11Device_CreateBuffer(dev, &d, data ? &s : NULL, &b));
    return b;
}

static void read_back(ID3D11Buffer *b, void *out, UINT size)
{
    D3D11_BUFFER_DESC d;
    ID3D11Buffer *st = NULL;
    D3D11_MAPPED_SUBRESOURCE m;
    /* Staging the same size as the buffer (CopyResource asks for it). */
    ID3D11Buffer_GetDesc(b, &d);
    d.Usage = D3D11_USAGE_STAGING; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ; d.BindFlags = 0; d.MiscFlags = 0;
    check("CreateBuffer (staging)", ID3D11Device_CreateBuffer(dev, &d, NULL, &st));
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)b);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &m))) {
        memcpy(out, m.pData, size);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
    }
    ID3D11Buffer_Release(st);
}

static void panel(int n)
{
    D3D11_VIEWPORT vp;
    vp.TopLeftX = (float)((n % 6) * 64); vp.TopLeftY = (float)((n / 6) * 64);
    vp.Width = 64; vp.Height = 64; vp.MinDepth = 0; vp.MaxDepth = 1;
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
}

static void draw(const Vtx *v, int n, D3D11_PRIMITIVE_TOPOLOGY t)
{
    ID3D11Buffer *vb = buffer(D3D11_BIND_VERTEX_BUFFER, v, n * sizeof(Vtx));
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, t);
    ID3D11DeviceContext_Draw(ctx, n, 0);
    ID3D11Buffer_Release(vb);
}

/* Draw back what stream output wrote into `b` (40-byte vertices). */
static void draw_auto(ID3D11Buffer *b)
{
    UINT stride = 40, off = 0;
    ID3D11Buffer *nil = NULL;
    ID3D11DeviceContext_IASetInputLayout(ctx, so_layout);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &b, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_DrawAuto(ctx);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &nil, &stride, &off);
}

/* Stream output entries: position, colour, uv: 40 bytes a vertex. */
static D3D11_SO_DECLARATION_ENTRY so_decl[3] = {
    {0, "SV_POSITION", 0, 0, 4, 0},
    {0, "COLOR", 0, 0, 4, 0},
    {0, "TEXCOORD", 0, 0, 2, 0},
};

static void so_targets(ID3D11Buffer *b, UINT offset)
{
    ID3D11DeviceContext_SOSetTargets(ctx, b ? 1 : 0, b ? &b : NULL, &offset);
}

static void print_verts(const char *what, ID3D11Buffer *b, int n)
{
    float v[10 * 12];
    int i;
    read_back(b, v, sizeof v);
    printf("%s:", what);
    for (i = 0; i < n; i++)
        printf(" (%.4f %.4f %.4f %.4f | %.3f %.3f %.3f | %.3f %.3f)", v[i * 10], v[i * 10 + 1], v[i * 10 + 2], v[i * 10 + 3], v[i * 10 + 4], v[i * 10 + 5], v[i * 10 + 6], v[i * 10 + 8], v[i * 10 + 9]);
    printf("\n");
}

static void panels(void)
{
    ID3D11VertexShader *vs = NULL, *turn = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11GeometryShader *sprite = NULL, *explode = NULL, *adj = NULL, *so_vs = NULL, *so_gs = NULL;
    ID3D11RasterizerState *none = NULL;
    D3D11_RASTERIZER_DESC rd;
    ID3D11Buffer *sb, *sb2, *sb3;
    UINT strides[1] = {40};

    check("CreateVertexShader", ID3D11Device_CreateVertexShader(dev, vs40_main, sizeof vs40_main, NULL, &vs));
    check("CreateVertexShader (turn)", ID3D11Device_CreateVertexShader(dev, vs40_turn, sizeof vs40_turn, NULL, &turn));
    check("CreatePixelShader", ID3D11Device_CreatePixelShader(dev, ps40_main, sizeof ps40_main, NULL, &ps));
    check("CreateGeometryShader (sprite)", ID3D11Device_CreateGeometryShader(dev, gs40_sprite, sizeof gs40_sprite, NULL, &sprite));
    check("CreateGeometryShader (explode)", ID3D11Device_CreateGeometryShader(dev, gs40_explode, sizeof gs40_explode, NULL, &explode));
    check("CreateGeometryShader (adjacency)", ID3D11Device_CreateGeometryShader(dev, gs40_adj, sizeof gs40_adj, NULL, &adj));
    check("CreateGeometryShaderWithStreamOutput (vertex shader)", ID3D11Device_CreateGeometryShaderWithStreamOutput(dev, vs40_turn, sizeof vs40_turn, so_decl, 3, strides, 1, D3D11_SO_NO_RASTERIZED_STREAM, NULL, &so_vs));
    check("CreateGeometryShaderWithStreamOutput (explode)", ID3D11Device_CreateGeometryShaderWithStreamOutput(dev, gs40_explode, sizeof gs40_explode, so_decl, 3, strides, 1, 0, NULL, &so_gs));
    memset(&rd, 0, sizeof rd);
    rd.FillMode = D3D11_FILL_SOLID; rd.CullMode = D3D11_CULL_NONE; rd.DepthClipEnable = TRUE;
    check("CreateRasterizerState", ID3D11Device_CreateRasterizerState(dev, &rd, &none));
    ID3D11DeviceContext_RSSetState(ctx, none);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);

    /* 0: points, each a square. */
    {
        Vtx p[5] = {{-0.6f, 0.6f, 0.5f, RGBA(250, 80, 60, 255), 0, 0}, {0.6f, 0.6f, 0.5f, RGBA(80, 250, 60, 255), 0, 0}, {0, 0, 0.5f, RGBA(60, 80, 250, 255), 0, 0},
                    {-0.6f, -0.6f, 0.5f, RGBA(250, 250, 60, 255), 0, 0}, {0.6f, -0.6f, 0.5f, RGBA(250, 60, 250, 255), 0, 0}};
        panel(0);
        ID3D11DeviceContext_GSSetShader(ctx, sprite, NULL, 0);
        draw(p, 5, D3D11_PRIMITIVE_TOPOLOGY_POINTLIST);
    }
    /* 1: two triangles exploded. */
    {
        Vtx t[6] = {{-0.9f, 0.9f, 0.5f, 0, 0, 0}, {0.9f, 0.9f, 0.5f, 0, 1, 0}, {-0.9f, -0.9f, 0.5f, 0, 0, 1},
                    {0.9f, 0.9f, 0.5f, 0, 1, 0}, {0.9f, -0.9f, 0.5f, 0, 1, 1}, {-0.9f, -0.9f, 0.5f, 0, 0, 1}};
        panel(1);
        ID3D11DeviceContext_GSSetShader(ctx, explode, NULL, 0);
        draw(t, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    }
    /* 2: triangles with adjacency. */
    {
        Vtx t[12] = {
            {-0.9f, 0.8f, 0.5f, RGBA(250, 250, 250, 255), 0, 0}, {0, 1, 0.5f, RGBA(250, 0, 0, 255), 0, 0}, {0.8f, 0.8f, 0.5f, RGBA(60, 60, 60, 255), 1, 0},
            {0, 0, 0.5f, RGBA(0, 250, 0, 255), 0, 0}, {-0.8f, -0.1f, 0.5f, RGBA(120, 120, 220, 255), 0, 1}, {-1, 0, 0.5f, RGBA(0, 0, 250, 255), 0, 0},
            {0.9f, 0.6f, 0.5f, RGBA(220, 200, 40, 255), 1, 0}, {1, 0, 0.5f, RGBA(250, 0, 250, 255), 0, 0}, {0.8f, -0.9f, 0.5f, RGBA(40, 200, 220, 255), 1, 1},
            {0, -1, 0.5f, RGBA(0, 250, 250, 255), 0, 0}, {-0.6f, -0.8f, 0.5f, RGBA(220, 40, 200, 255), 0, 1}, {0, 0, 0.5f, RGBA(250, 250, 0, 255), 0, 0},
        };
        panel(2);
        ID3D11DeviceContext_GSSetShader(ctx, adj, NULL, 0);
        draw(t, 12, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST_ADJ);
    }
    /* 3: a vertex shader's vertices streamed out, not drawn, then drawn by
     * DrawAuto. */
    sb = buffer(D3D11_BIND_STREAM_OUTPUT | D3D11_BIND_VERTEX_BUFFER, NULL, 40 * 64);
    {
        Vtx t[6] = {{-0.8f, 0.8f, 0.5f, RGBA(250, 120, 30, 255), 0, 0}, {0.8f, 0.6f, 0.5f, RGBA(30, 250, 120, 255), 1, 0}, {-0.6f, -0.8f, 0.5f, RGBA(120, 30, 250, 255), 0, 1},
                    {0.8f, 0.6f, 0.5f, RGBA(250, 250, 250, 255), 1, 0}, {0.7f, -0.7f, 0.5f, RGBA(30, 30, 30, 255), 1, 1}, {-0.6f, -0.8f, 0.5f, RGBA(250, 30, 30, 255), 0, 1}};
        panel(3);
        ID3D11DeviceContext_VSSetShader(ctx, turn, NULL, 0);
        ID3D11DeviceContext_GSSetShader(ctx, so_vs, NULL, 0);
        so_targets(sb, 0);
        draw(t, 6, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        so_targets(NULL, 0);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_GSSetShader(ctx, NULL, NULL, 0);
        print_verts("streamed from the vertex shader", sb, 3);
        draw_auto(sb);
    }
    /* 4, 5: a geometry shader streaming out while it draws; what it wrote
     * drawn again. */
    sb2 = buffer(D3D11_BIND_STREAM_OUTPUT | D3D11_BIND_VERTEX_BUFFER, NULL, 40 * 64);
    {
        Vtx t[3] = {{0, 0.9f, 0.5f, 0, 0.5f, 0}, {0.9f, -0.8f, 0.5f, 0, 1, 1}, {-0.9f, -0.8f, 0.5f, 0, 0, 1}};
        panel(4);
        ID3D11DeviceContext_GSSetShader(ctx, so_gs, NULL, 0);
        so_targets(sb2, 0);
        draw(t, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        so_targets(NULL, 0);
        ID3D11DeviceContext_GSSetShader(ctx, NULL, NULL, 0);
        print_verts("streamed from the geometry shader", sb2, 4);
        panel(5);
        draw_auto(sb2);
    }
    /* 6: two draws appended into one buffer, then drawn. */
    sb3 = buffer(D3D11_BIND_STREAM_OUTPUT | D3D11_BIND_VERTEX_BUFFER, NULL, 40 * 64);
    {
        Vtx a[3] = {{-0.9f, 0.9f, 0.5f, RGBA(250, 200, 60, 255), 0, 0}, {0.1f, 0.9f, 0.5f, RGBA(250, 200, 60, 255), 1, 0}, {-0.9f, -0.1f, 0.5f, RGBA(250, 60, 60, 255), 0, 1}};
        Vtx b[3] = {{0.9f, -0.9f, 0.5f, RGBA(60, 200, 250, 255), 1, 1}, {-0.1f, -0.9f, 0.5f, RGBA(60, 200, 250, 255), 0, 1}, {0.9f, 0.1f, 0.5f, RGBA(60, 60, 250, 255), 1, 0}};
        ID3D11DeviceContext_VSSetShader(ctx, turn, NULL, 0);
        ID3D11DeviceContext_GSSetShader(ctx, so_vs, NULL, 0);
        so_targets(sb3, 0);
        draw(a, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        so_targets(sb3, (UINT)-1);
        draw(b, 3, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        so_targets(NULL, 0);
        ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
        ID3D11DeviceContext_GSSetShader(ctx, NULL, NULL, 0);
        panel(6);
        draw_auto(sb3);
    }
    ID3D11Buffer_Release(sb); ID3D11Buffer_Release(sb2); ID3D11Buffer_Release(sb3);
    ID3D11RasterizerState_Release(none);
    ID3D11VertexShader_Release(vs); ID3D11VertexShader_Release(turn); ID3D11PixelShader_Release(ps);
    ID3D11GeometryShader_Release(sprite); ID3D11GeometryShader_Release(explode); ID3D11GeometryShader_Release(adj);
    ID3D11GeometryShader_Release(so_vs); ID3D11GeometryShader_Release(so_gs);
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
    D3D_FEATURE_LEVEL want[1] = {D3D_FEATURE_LEVEL_11_0}, got;
    D3D_DRIVER_TYPE driver = D3D_DRIVER_TYPE_WARP;
    D3D11_INPUT_ELEMENT_DESC e[3] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    D3D11_INPUT_ELEMENT_DESC se[3] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"COLOR", 0, DXGI_FORMAT_R32G32B32A32_FLOAT, 0, 16, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 32, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    ID3D11Texture2D *back, *st;
    ID3D11RenderTargetView *rtv = NULL;
    D3D11_TEXTURE2D_DESC td;
    D3D11_MAPPED_SUBRESOURCE mp;
    float clear1[4] = {0.05f, 0.05f, 0.05f, 1};
    HRESULT hr;
    int x, y;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    if (argc > 1 && strcmp(argv[1], "gpu") == 0)
        driver = D3D_DRIVER_TYPE_HARDWARE;
    hr = D3D11CreateDevice(NULL, driver, NULL, 0, want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    printf("device: %s, feature level 0x%x\n", SUCCEEDED(hr) ? "created" : "FAILED", (int)got);
    if (FAILED(hr))
        return 1;
    memset(&td, 0, sizeof td);
    td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_RENDER_TARGET;
    check("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &td, NULL, &back));
    check("CreateRenderTargetView", ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv));
    ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear1);
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
    check("CreateInputLayout", ID3D11Device_CreateInputLayout(dev, e, 3, vs40_main, sizeof vs40_main, &layout));
    check("CreateInputLayout (streamed)", ID3D11Device_CreateInputLayout(dev, se, 3, vs40_main, sizeof vs40_main, &so_layout));
    panels();

    ID3D11Texture2D_GetDesc(back, &td);
    td.Usage = D3D11_USAGE_STAGING; td.CPUAccessFlags = D3D11_CPU_ACCESS_READ; td.BindFlags = 0;
    check("CreateTexture2D (staging)", ID3D11Device_CreateTexture2D(dev, &td, NULL, &st));
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
    ID3D11InputLayout_Release(so_layout);
    ID3D11RenderTargetView_Release(rtv);
    ID3D11Texture2D_Release(back);
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
    strcpy(slash ? slash + 1 : path, "dx5.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    return compare(img);
}
