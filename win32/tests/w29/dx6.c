/* G5f2d test: Direct3D 11 tessellation: hull shaders (a control point
 * phase, patch constants in fork phases) and domain shaders over triangle
 * and quad patches, integer and fractional odd partitioning, tess factors
 * from a constant buffer, and back faces culled.
 *
 * Draws seven 64x64 panels of a 384x128 target and compares them with what
 * WARP drew; `dx6 gpu` draws on this machine's GPU instead. The domain
 * shaders bulge the patches, so where the tessellator puts its points
 * shows.
 *
 *   dx6                 compare with dx6.ref next to the program
 *   dx6 record FILE     write what WARP draws as FILE (a PPM)
 *   dx6 gpu             compare, drawn on the GPU
 *   dx6 stats           as dx6, with each panel's differences counted
 *
 * In each panel at most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/dx6_vs50_main.h"
#include "build/dx6_hs50_tri.h"
#include "build/dx6_hs50_trifrac.h"
#include "build/dx6_hs50_quad.h"
#include "build/dx6_ds50_tri.h"
#include "build/dx6_ds50_quad.h"
#include "build/dx6_ps50_main.h"
#include "build/dx6_ps50_quad.h"

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
static ID3D11Buffer *cb;

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

static void panel(int n)
{
    D3D11_VIEWPORT vp;
    vp.TopLeftX = (float)((n % 6) * 64); vp.TopLeftY = (float)((n / 6) * 64);
    vp.Width = 64; vp.Height = 64; vp.MinDepth = 0; vp.MaxDepth = 1;
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
}

static void factors(float e0, float e1, float e2, float e3, float i0, float i1)
{
    float f[8];
    f[0] = e0; f[1] = e1; f[2] = e2; f[3] = e3; f[4] = i0; f[5] = i1; f[6] = 0; f[7] = 0;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)cb, 0, NULL, f, 0, 0);
}

static void draw(const Vtx *v, int n, D3D11_PRIMITIVE_TOPOLOGY t)
{
    ID3D11Buffer *vb = buffer(D3D11_BIND_VERTEX_BUFFER, v, n * sizeof(Vtx));
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, t);
    ID3D11DeviceContext_Draw(ctx, n, 0);
    ID3D11Buffer_Release(vb);
}

static void panels(void)
{
    ID3D11HullShader *h_tri = NULL, *h_frac = NULL, *h_quad = NULL;
    ID3D11DomainShader *d_tri = NULL, *d_quad = NULL;
    ID3D11PixelShader *p_main = NULL, *p_quad = NULL;
    ID3D11RasterizerState *none = NULL, *back = NULL;
    D3D11_RASTERIZER_DESC rd;
    Vtx tri[3] = {{0, 0.85f, 0.5f, RGBA(250, 90, 60, 255), 0, 0}, {0.85f, -0.75f, 0.5f, RGBA(60, 230, 90, 255), 0, 0}, {-0.85f, -0.75f, 0.5f, RGBA(70, 90, 250, 255), 0, 0}};
    Vtx quad[4] = {{-0.8f, -0.8f, 0.5f, RGBA(250, 200, 60, 255), 0, 0}, {0.8f, -0.8f, 0.5f, RGBA(60, 200, 250, 255), 0, 0},
                   {0.8f, 0.8f, 0.5f, RGBA(250, 60, 200, 255), 0, 0}, {-0.8f, 0.8f, 0.5f, RGBA(120, 250, 120, 255), 0, 0}};

    check("CreateHullShader", ID3D11Device_CreateHullShader(dev, hs50_tri, sizeof hs50_tri, NULL, &h_tri));
    check("CreateHullShader (fractional)", ID3D11Device_CreateHullShader(dev, hs50_trifrac, sizeof hs50_trifrac, NULL, &h_frac));
    check("CreateHullShader (quad)", ID3D11Device_CreateHullShader(dev, hs50_quad, sizeof hs50_quad, NULL, &h_quad));
    check("CreateDomainShader", ID3D11Device_CreateDomainShader(dev, ds50_tri, sizeof ds50_tri, NULL, &d_tri));
    check("CreateDomainShader (quad)", ID3D11Device_CreateDomainShader(dev, ds50_quad, sizeof ds50_quad, NULL, &d_quad));
    check("CreatePixelShader", ID3D11Device_CreatePixelShader(dev, ps50_main, sizeof ps50_main, NULL, &p_main));
    check("CreatePixelShader (quad)", ID3D11Device_CreatePixelShader(dev, ps50_quad, sizeof ps50_quad, NULL, &p_quad));
    memset(&rd, 0, sizeof rd);
    rd.FillMode = D3D11_FILL_SOLID; rd.CullMode = D3D11_CULL_NONE; rd.DepthClipEnable = TRUE;
    check("CreateRasterizerState", ID3D11Device_CreateRasterizerState(dev, &rd, &none));
    rd.CullMode = D3D11_CULL_BACK;
    check("CreateRasterizerState (back)", ID3D11Device_CreateRasterizerState(dev, &rd, &back));
    ID3D11DeviceContext_HSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_DSSetConstantBuffers(ctx, 0, 1, &cb);
    ID3D11DeviceContext_RSSetState(ctx, none);

    /* 0-2: triangles, integer: uneven edges; odd inside; all 16. */
    ID3D11DeviceContext_HSSetShader(ctx, h_tri, NULL, 0);
    ID3D11DeviceContext_DSSetShader(ctx, d_tri, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, p_main, NULL, 0);
    factors(3, 4, 5, 0, 4, 0);
    panel(0);
    draw(tri, 3, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
    factors(2, 2, 2, 0, 3, 0);
    panel(1);
    draw(tri, 3, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
    factors(16, 16, 16, 0, 16, 0);
    panel(2);
    draw(tri, 3, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
    /* 3: back faces culled (the patch faces the other way). */
    {
        Vtx rev[3];
        rev[0] = tri[0]; rev[1] = tri[2]; rev[2] = tri[1];
        ID3D11DeviceContext_RSSetState(ctx, back);
        factors(4, 4, 4, 0, 4, 0);
        panel(3);
        draw(tri, 3, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
        draw(rev, 3, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
        ID3D11DeviceContext_RSSetState(ctx, none);
    }
    /* 4: fractional odd partitioning. */
    ID3D11DeviceContext_HSSetShader(ctx, h_frac, NULL, 0);
    factors(2.5f, 3.7f, 1.3f, 0, 4.2f, 0);
    panel(4);
    draw(tri, 3, D3D11_PRIMITIVE_TOPOLOGY_3_CONTROL_POINT_PATCHLIST);
    /* 5, 6: quads, integer: even; odd. */
    ID3D11DeviceContext_HSSetShader(ctx, h_quad, NULL, 0);
    ID3D11DeviceContext_DSSetShader(ctx, d_quad, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, p_quad, NULL, 0);
    factors(4, 4, 4, 4, 4, 4);
    panel(5);
    draw(quad, 4, D3D11_PRIMITIVE_TOPOLOGY_4_CONTROL_POINT_PATCHLIST);
    factors(5, 5, 5, 5, 5, 5);
    panel(6);
    draw(quad, 4, D3D11_PRIMITIVE_TOPOLOGY_4_CONTROL_POINT_PATCHLIST);

    ID3D11DeviceContext_HSSetShader(ctx, NULL, NULL, 0);
    ID3D11DeviceContext_DSSetShader(ctx, NULL, NULL, 0);
    ID3D11HullShader_Release(h_tri); ID3D11HullShader_Release(h_frac); ID3D11HullShader_Release(h_quad);
    ID3D11DomainShader_Release(d_tri); ID3D11DomainShader_Release(d_quad);
    ID3D11PixelShader_Release(p_main); ID3D11PixelShader_Release(p_quad);
    ID3D11RasterizerState_Release(none); ID3D11RasterizerState_Release(back);
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
        ID3D11Texture2D *back, *st;
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
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
    check("CreateInputLayout", ID3D11Device_CreateInputLayout(dev, e, 3, vs50_main, sizeof vs50_main, &layout));
    check("CreateVertexShader", ID3D11Device_CreateVertexShader(dev, vs50_main, sizeof vs50_main, NULL, &vs));
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    cb = buffer(D3D11_BIND_CONSTANT_BUFFER, NULL, 32);
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
    ID3D11VertexShader_Release(vs);
    ID3D11Buffer_Release(cb);
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
    strcpy(slash ? slash + 1 : path, "dx6.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    return compare(img);
}
