/* G5f2e test: Direct3D 11 deferred contexts and command lists: draws,
 * clears, constant buffer updates, a dynamic vertex buffer mapped and a
 * compute dispatch, all recorded on deferred contexts and executed on the
 * immediate one (its state put back, or cleared); also indirect draws and
 * unordered access view clears.
 *
 * Draws eight 64x64 panels of a 384x128 target and prints what the
 * contexts and a cleared buffer give; compared with what WARP drew and
 * printed on Windows. `dx7 gpu` runs on this machine's GPU instead.
 *
 *   dx7                 compare with dx7.ref next to the program
 *   dx7 record FILE     write what WARP draws as FILE (a PPM)
 *   dx7 gpu             compare, drawn on the GPU
 *   dx7 stats           as dx7, with each panel's differences counted
 *
 * In each panel at most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/dx7_vs50_main.h"
#include "build/dx7_ps50_main.h"
#include "build/dx7_ps50_show.h"
#include "build/dx7_cs50_rings.h"

#define W 384
#define H 128
#define PANELS 12

typedef struct {
    float x, y, z;
    DWORD c;
} Vtx;

static unsigned char ref[W * H * 3];
static int stats;
static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

#define RGBA(r, g, b, a) ((DWORD)(r) | (DWORD)(g) << 8 | (DWORD)(b) << 16 | (DWORD)(a) << 24)

static void check(const char *what, HRESULT hr)
{
    if (FAILED(hr))
        printf("%s: 0x%lx\n", what, (unsigned long)hr);
}

static ID3D11Buffer *buffer(UINT bind, D3D11_USAGE usage, UINT misc, const void *data, UINT size)
{
    D3D11_BUFFER_DESC d;
    D3D11_SUBRESOURCE_DATA s;
    ID3D11Buffer *b = NULL;
    memset(&d, 0, sizeof d);
    d.ByteWidth = size; d.Usage = usage; d.BindFlags = bind; d.MiscFlags = misc;
    d.CPUAccessFlags = usage == D3D11_USAGE_DYNAMIC ? D3D11_CPU_ACCESS_WRITE : 0;
    s.pSysMem = data; s.SysMemPitch = 0; s.SysMemSlicePitch = 0;
    check("CreateBuffer", ID3D11Device_CreateBuffer(dev, &d, data ? &s : NULL, &b));
    return b;
}

static void read_back(ID3D11Buffer *b, void *out, UINT size)
{
    D3D11_BUFFER_DESC d;
    ID3D11Buffer *st = NULL;
    D3D11_MAPPED_SUBRESOURCE m;
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

static void panel(ID3D11DeviceContext *c, int n)
{
    D3D11_VIEWPORT vp;
    vp.TopLeftX = (float)((n % 6) * 64); vp.TopLeftY = (float)((n / 6) * 64);
    vp.Width = 64; vp.Height = 64; vp.MinDepth = 0; vp.MaxDepth = 1;
    ID3D11DeviceContext_RSSetViewports(c, 1, &vp);
}

static void tint(ID3D11DeviceContext *c, ID3D11Buffer *cb, float r, float g, float b)
{
    float t[4];
    t[0] = r; t[1] = g; t[2] = b; t[3] = 1;
    ID3D11DeviceContext_UpdateSubresource(c, (ID3D11Resource *)cb, 0, NULL, t, 0, 0);
}

static ID3D11Buffer *quad_vb, *tri_vb, *dyn_vb, *cb;
static ID3D11InputLayout *layout;
static ID3D11VertexShader *vs;
static ID3D11PixelShader *ps, *ps_show;
static ID3D11ComputeShader *cs;

/* The pipeline for coloured triangles, set on `c`. */
static void pipeline(ID3D11DeviceContext *c, ID3D11RenderTargetView *rtv)
{
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11DeviceContext_IASetInputLayout(c, layout);
    ID3D11DeviceContext_IASetVertexBuffers(c, 0, 1, &tri_vb, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(c, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_VSSetShader(c, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(c, ps, NULL, 0);
    ID3D11DeviceContext_PSSetConstantBuffers(c, 0, 1, &cb);
    ID3D11DeviceContext_OMSetRenderTargets(c, 1, &rtv, NULL);
}

static void panels(ID3D11RenderTargetView *rtv)
{
    ID3D11DeviceContext *dc = NULL, *dc2 = NULL;
    ID3D11CommandList *list = NULL, *list2 = NULL;
    ID3D11Texture2D *t = NULL;
    ID3D11UnorderedAccessView *u = NULL;
    ID3D11ShaderResourceView *s = NULL;
    D3D11_TEXTURE2D_DESC td;
    D3D11_MAPPED_SUBRESOURCE m;
    float clear0[4] = {0.15f, 0.1f, 0.3f, 1};
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11RenderTargetView *got = NULL;

    check("CreateDeferredContext", ID3D11Device_CreateDeferredContext(dev, 0, &dc));
    check("CreateDeferredContext (2)", ID3D11Device_CreateDeferredContext(dev, 0, &dc2));
    printf("context types: immediate %d, deferred %d\n", (int)ID3D11DeviceContext_GetType(ctx), (int)ID3D11DeviceContext_GetType(dc));

    /* A texture the compute shader writes in the command list. */
    memset(&td, 0, sizeof td);
    td.Width = 64; td.Height = 64; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE;
    check("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &td, NULL, &t));
    check("CreateUnorderedAccessView", ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)t, NULL, &u));
    check("CreateShaderResourceView", ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, NULL, &s));

    /* The first list: the panels 0-3, the constant buffer updated between
     * draws, a dynamic vertex buffer mapped, the dispatch. */
    pipeline(dc, rtv);
    panel(dc, 0);
    tint(dc, cb, 1, 1, 1);
    ID3D11DeviceContext_Draw(dc, 3, 0);
    panel(dc, 1);
    tint(dc, cb, 1, 0.4f, 0.4f);
    ID3D11DeviceContext_Draw(dc, 3, 0);
    tint(dc, cb, 0.3f, 0.3f, 1);
    ID3D11DeviceContext_Draw(dc, 3, 3);
    panel(dc, 2);
    tint(dc, cb, 1, 1, 1);
    if (SUCCEEDED(ID3D11DeviceContext_Map(dc, (ID3D11Resource *)dyn_vb, 0, D3D11_MAP_WRITE_DISCARD, 0, &m))) {
        Vtx v[3] = {{-0.9f, -0.9f, 0.5f, RGBA(250, 250, 40, 255)}, {0.9f, 0.9f, 0.5f, RGBA(40, 250, 250, 255)}, {0.9f, -0.9f, 0.5f, RGBA(250, 40, 250, 255)}};
        memcpy(m.pData, v, sizeof v);
        ID3D11DeviceContext_Unmap(dc, (ID3D11Resource *)dyn_vb, 0);
    }
    ID3D11DeviceContext_IASetVertexBuffers(dc, 0, 1, &dyn_vb, &stride, &off);
    ID3D11DeviceContext_Draw(dc, 3, 0);
    ID3D11DeviceContext_CSSetShader(dc, cs, NULL, 0);
    ID3D11DeviceContext_CSSetUnorderedAccessViews(dc, 0, 1, &u, NULL);
    ID3D11DeviceContext_CSSetConstantBuffers(dc, 0, 1, &cb);
    tint(dc, cb, 1, 0.5f, 1);
    ID3D11DeviceContext_Dispatch(dc, 8, 8, 1);
    {
        ID3D11UnorderedAccessView *nu = NULL;
        ID3D11DeviceContext_CSSetUnorderedAccessViews(dc, 0, 1, &nu, NULL);
    }
    ID3D11DeviceContext_IASetVertexBuffers(dc, 0, 1, &quad_vb, &stride, &off);
    ID3D11DeviceContext_PSSetShader(dc, ps_show, NULL, 0);
    ID3D11DeviceContext_PSSetShaderResources(dc, 0, 1, &s);
    panel(dc, 3);
    ID3D11DeviceContext_Draw(dc, 6, 0);
    check("FinishCommandList", ID3D11DeviceContext_FinishCommandList(dc, FALSE, &list));

    /* The second list, recorded meanwhile on another context: a clear
     * and panel 4. */
    pipeline(dc2, rtv);
    ID3D11DeviceContext_ClearRenderTargetView(dc2, rtv, clear0);
    panel(dc2, 4);
    tint(dc2, cb, 0.5f, 1, 0.5f);
    ID3D11DeviceContext_Draw(dc2, 6, 0);
    check("FinishCommandList (2)", ID3D11DeviceContext_FinishCommandList(dc2, TRUE, &list2));

    /* The immediate context's own state, kept through the first list. */
    pipeline(ctx, rtv);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_ExecuteCommandList(ctx, list2, TRUE);
    ID3D11DeviceContext_ExecuteCommandList(ctx, list, TRUE);
    ID3D11DeviceContext_OMGetRenderTargets(ctx, 1, &got, NULL);
    printf("state kept: render target %s\n", got == rtv ? "as set" : "changed");
    if (got)
        ID3D11RenderTargetView_Release(got);
    /* Drawn by the immediate context after: panel 5. */
    panel(ctx, 5);
    tint(ctx, cb, 0.8f, 0.8f, 0.2f);
    ID3D11DeviceContext_Draw(ctx, 3, 0);
    /* The first list again, cleared after: nothing bound. */
    ID3D11DeviceContext_ExecuteCommandList(ctx, list, FALSE);
    ID3D11DeviceContext_OMGetRenderTargets(ctx, 1, &got, NULL);
    printf("state cleared: render target %s\n", got ? "still bound" : "none");
    if (got)
        ID3D11RenderTargetView_Release(got);

    /* 6: an indirect draw. */
    {
        UINT args[4] = {3, 1, 3, 0};
        D3D11_BUFFER_DESC d;
        ID3D11Buffer *ab = NULL;
        D3D11_SUBRESOURCE_DATA sd;
        memset(&d, 0, sizeof d);
        d.ByteWidth = sizeof args; d.Usage = D3D11_USAGE_DEFAULT; d.MiscFlags = D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS;
        sd.pSysMem = args; sd.SysMemPitch = 0; sd.SysMemSlicePitch = 0;
        check("CreateBuffer (indirect)", ID3D11Device_CreateBuffer(dev, &d, &sd, &ab));
        pipeline(ctx, rtv);
        panel(ctx, 6);
        tint(ctx, cb, 1, 1, 1);
        ID3D11DeviceContext_DrawInstancedIndirect(ctx, ab, 0);
        ID3D11Buffer_Release(ab);
    }
    /* 7: the texture cleared through its UAV, then shown. */
    {
        float c[4] = {0.25f, 0.75f, 0.5f, 1};
        ID3D11DeviceContext_ClearUnorderedAccessViewFloat(ctx, u, c);
        ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &quad_vb, &stride, &off);
        ID3D11DeviceContext_PSSetShader(ctx, ps_show, NULL, 0);
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &s);
        panel(ctx, 7);
        ID3D11DeviceContext_Draw(ctx, 6, 0);
    }
    /* A buffer cleared through a raw UAV. */
    {
        UINT vals[16], i;
        UINT cu[4] = {0xA5A5u, 1, 2, 3};
        ID3D11Buffer *b = buffer(D3D11_BIND_UNORDERED_ACCESS, D3D11_USAGE_DEFAULT, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, NULL, sizeof vals);
        ID3D11UnorderedAccessView *bu = NULL;
        D3D11_UNORDERED_ACCESS_VIEW_DESC ud;
        memset(&ud, 0, sizeof ud);
        ud.Format = DXGI_FORMAT_R32_TYPELESS; ud.ViewDimension = D3D11_UAV_DIMENSION_BUFFER; ud.Buffer.FirstElement = 4; ud.Buffer.NumElements = 8; ud.Buffer.Flags = D3D11_BUFFER_UAV_FLAG_RAW;
        check("CreateUnorderedAccessView (raw)", ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)b, &ud, &bu));
        ID3D11DeviceContext_ClearUnorderedAccessViewUint(ctx, bu, cu);
        read_back(b, vals, sizeof vals);
        printf("buffer cleared from element 4 to 11:");
        for (i = 0; i < 16; i++)
            printf(" %x", vals[i]);
        printf("\n");
        ID3D11UnorderedAccessView_Release(bu);
        ID3D11Buffer_Release(b);
    }
    ID3D11CommandList_Release(list);
    ID3D11CommandList_Release(list2);
    ID3D11DeviceContext_Release(dc);
    ID3D11DeviceContext_Release(dc2);
    ID3D11UnorderedAccessView_Release(u);
    ID3D11ShaderResourceView_Release(s);
    ID3D11Texture2D_Release(t);
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
    D3D11_INPUT_ELEMENT_DESC e[2] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    Vtx tris[6] = {{0, 0.9f, 0.5f, RGBA(250, 80, 60, 255)}, {0.9f, -0.8f, 0.5f, RGBA(60, 230, 90, 255)}, {-0.9f, -0.8f, 0.5f, RGBA(70, 90, 250, 255)},
                   {-0.9f, 0.9f, 0.5f, RGBA(250, 250, 250, 255)}, {0.2f, 0.9f, 0.5f, RGBA(250, 250, 250, 255)}, {-0.9f, -0.2f, 0.5f, RGBA(250, 250, 250, 255)}};
    Vtx quad[6] = {{-1, 1, 0.5f, 0xFFFFFFFF}, {1, 1, 0.5f, 0xFFFFFFFF}, {1, -1, 0.5f, 0xFFFFFFFF}, {-1, 1, 0.5f, 0xFFFFFFFF}, {1, -1, 0.5f, 0xFFFFFFFF}, {-1, -1, 0.5f, 0xFFFFFFFF}};
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
    check("CreateInputLayout", ID3D11Device_CreateInputLayout(dev, e, 2, vs50_main, sizeof vs50_main, &layout));
    check("CreateVertexShader", ID3D11Device_CreateVertexShader(dev, vs50_main, sizeof vs50_main, NULL, &vs));
    check("CreatePixelShader", ID3D11Device_CreatePixelShader(dev, ps50_main, sizeof ps50_main, NULL, &ps));
    check("CreatePixelShader (show)", ID3D11Device_CreatePixelShader(dev, ps50_show, sizeof ps50_show, NULL, &ps_show));
    check("CreateComputeShader", ID3D11Device_CreateComputeShader(dev, cs50_rings, sizeof cs50_rings, NULL, &cs));
    tri_vb = buffer(D3D11_BIND_VERTEX_BUFFER, D3D11_USAGE_DEFAULT, 0, tris, sizeof tris);
    quad_vb = buffer(D3D11_BIND_VERTEX_BUFFER, D3D11_USAGE_DEFAULT, 0, quad, sizeof quad);
    dyn_vb = buffer(D3D11_BIND_VERTEX_BUFFER, D3D11_USAGE_DYNAMIC, 0, NULL, 3 * sizeof(Vtx));
    cb = buffer(D3D11_BIND_CONSTANT_BUFFER, D3D11_USAGE_DEFAULT, 0, NULL, 16);
    panels(rtv);

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
    ID3D11VertexShader_Release(vs); ID3D11PixelShader_Release(ps); ID3D11PixelShader_Release(ps_show); ID3D11ComputeShader_Release(cs);
    ID3D11Buffer_Release(tri_vb); ID3D11Buffer_Release(quad_vb); ID3D11Buffer_Release(dyn_vb); ID3D11Buffer_Release(cb);
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
    strcpy(slash ? slash + 1 : path, "dx7.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    return compare(img);
}
