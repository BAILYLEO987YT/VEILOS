/* G5f2a test: Direct3D 11 cube, 3D and array textures: sampled at given
 * levels, loaded, their dimensions asked, views of some of an array's
 * slices, a cube array, and a cube's face drawn into.
 *
 * Draws twelve 64x64 panels into a 384x128 target, reads it back and
 * compares it with what WARP drew; `dx3 gpu` draws on this machine's GPU.
 *
 *   dx3                 compare with dx3.ref next to the program
 *   dx3 record FILE     write what WARP draws as FILE (a PPM)
 *   dx3 gpu             compare, drawn on the GPU
 *   dx3 stats           as dx3, with each panel's differences counted
 *
 * In each panel at most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/dx3_vs40_main.h"
#include "build/dx3_ps40_cube.h"
#include "build/dx3_ps40_cubelod.h"
#include "build/dx3_ps40_vol.h"
#include "build/dx3_ps40_volload.h"
#include "build/dx3_ps40_arr.h"
#include "build/dx3_ps40_arrload.h"
#include "build/dx3_ps40_arr1.h"
#include "build/dx3_ps40_dims.h"
#include "build/dx3_ps41_cubes.h"

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
static ID3D11Texture2D *back;
static ID3D11RenderTargetView *back_rtv;
static ID3D11Buffer *pb;

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

static void draw(const Vtx *v, int n)
{
    ID3D11Buffer *vb = buffer(D3D11_BIND_VERTEX_BUFFER, v, n * sizeof(Vtx));
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_Draw(ctx, n, 0);
    ID3D11Buffer_Release(vb);
}

static void quad(void)
{
    Vtx q[6] = {
        {-1, 1, 0.5f, 0xFFFFFFFF, 0, 0}, {1, 1, 0.5f, 0xFFFFFFFF, 1, 0}, {1, -1, 0.5f, 0xFFFFFFFF, 1, 1},
        {-1, 1, 0.5f, 0xFFFFFFFF, 0, 0}, {1, -1, 0.5f, 0xFFFFFFFF, 1, 1}, {-1, -1, 0.5f, 0xFFFFFFFF, 0, 1},
    };
    draw(q, 6);
}

static ID3D11PixelShader *pshader(const void *code, SIZE_T len)
{
    ID3D11PixelShader *p = NULL;
    check("CreatePixelShader", ID3D11Device_CreatePixelShader(dev, code, len, NULL, &p));
    return p;
}

/* A texture of any kind: 1D, 2D (cube with misc) or 3D. */
static ID3D11Resource *texture(int dim, int w, int h, int d, int mips, int array, UINT bind, UINT misc)
{
    ID3D11Resource *r = NULL;
    if (dim == 1) {
        D3D11_TEXTURE1D_DESC t;
        memset(&t, 0, sizeof t);
        t.Width = w; t.MipLevels = mips; t.ArraySize = array; t.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        t.Usage = D3D11_USAGE_DEFAULT; t.BindFlags = bind; t.MiscFlags = misc;
        check("CreateTexture1D", ID3D11Device_CreateTexture1D(dev, &t, NULL, (ID3D11Texture1D **)&r));
    } else if (dim == 2) {
        D3D11_TEXTURE2D_DESC t;
        memset(&t, 0, sizeof t);
        t.Width = w; t.Height = h; t.MipLevels = mips; t.ArraySize = array; t.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        t.SampleDesc.Count = 1; t.Usage = D3D11_USAGE_DEFAULT; t.BindFlags = bind; t.MiscFlags = misc;
        check("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &t, NULL, (ID3D11Texture2D **)&r));
    } else {
        D3D11_TEXTURE3D_DESC t;
        memset(&t, 0, sizeof t);
        t.Width = w; t.Height = h; t.Depth = d; t.MipLevels = mips; t.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        t.Usage = D3D11_USAGE_DEFAULT; t.BindFlags = bind; t.MiscFlags = misc;
        check("CreateTexture3D", ID3D11Device_CreateTexture3D(dev, &t, NULL, (ID3D11Texture3D **)&r));
    }
    return r;
}

static DWORD base_color[12] = {
    RGBA(230, 60, 50, 255), RGBA(60, 200, 70, 255), RGBA(60, 90, 230, 255), RGBA(230, 220, 60, 255),
    RGBA(220, 70, 220, 255), RGBA(60, 210, 220, 255), RGBA(240, 150, 40, 255), RGBA(150, 240, 150, 255),
    RGBA(150, 150, 250, 255), RGBA(250, 250, 250, 255), RGBA(120, 60, 30, 255), RGBA(40, 40, 90, 255),
};

/* Fill subresource `sub` (w x h x d) with layer `layer`'s colour,
 * checkered, darker by `mip`. */
static void fill(ID3D11Resource *r, UINT sub, int w, int h, int d, int layer, int mip)
{
    static DWORD px[64 * 64 * 16];
    int x, y, z;
    for (z = 0; z < d; z++)
        for (y = 0; y < h; y++)
            for (x = 0; x < w; x++) {
                DWORD c = base_color[(layer + z) % 12];
                int r8 = c & 255, g8 = (c >> 8) & 255, b8 = (c >> 16) & 255;
                int dark = ((x * 8 / w + y * 8 / (h > 1 ? h : 8)) & 1) ? 0 : 60;
                dark += mip * 30;
                r8 = r8 > dark ? r8 - dark : 0; g8 = g8 > dark ? g8 - dark : 0; b8 = b8 > dark ? b8 - dark : 0;
                px[(z * h + y) * w + x] = RGBA(r8, g8, b8, 255);
            }
    ID3D11DeviceContext_UpdateSubresource(ctx, r, sub, NULL, px, w * 4, w * h * 4);
}

static ID3D11ShaderResourceView *srv(ID3D11Resource *r, D3D11_SRV_DIMENSION dim, UINT first, UINT count)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC d;
    ID3D11ShaderResourceView *v = NULL;
    memset(&d, 0, sizeof d);
    d.Format = DXGI_FORMAT_R8G8B8A8_UNORM; d.ViewDimension = dim;
    switch (dim) {
    case D3D11_SRV_DIMENSION_TEXTURECUBE: d.TextureCube.MipLevels = (UINT)-1; break;
    case D3D11_SRV_DIMENSION_TEXTURECUBEARRAY: d.TextureCubeArray.MipLevels = (UINT)-1; d.TextureCubeArray.First2DArrayFace = first; d.TextureCubeArray.NumCubes = count; break;
    case D3D11_SRV_DIMENSION_TEXTURE3D: d.Texture3D.MipLevels = (UINT)-1; break;
    case D3D11_SRV_DIMENSION_TEXTURE2DARRAY: d.Texture2DArray.MipLevels = (UINT)-1; d.Texture2DArray.FirstArraySlice = first; d.Texture2DArray.ArraySize = count; break;
    case D3D11_SRV_DIMENSION_TEXTURE1DARRAY: d.Texture1DArray.MipLevels = (UINT)-1; d.Texture1DArray.FirstArraySlice = first; d.Texture1DArray.ArraySize = count; break;
    default: break;
    }
    check("CreateShaderResourceView", ID3D11Device_CreateShaderResourceView(dev, r, &d, &v));
    return v;
}

static void bind_srv(UINT slot, ID3D11ShaderResourceView *v)
{
    ID3D11DeviceContext_PSSetShaderResources(ctx, slot, 1, &v);
}

static void consts(float x, float y)
{
    float k[4];
    k[0] = x; k[1] = y; k[2] = 0; k[3] = 0;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)pb, 0, NULL, k, 0, 0);
}

static void panels(void)
{
    ID3D11PixelShader *ps_cube = pshader(ps40_cube, sizeof ps40_cube), *ps_cubelod = pshader(ps40_cubelod, sizeof ps40_cubelod);
    ID3D11PixelShader *ps_vol = pshader(ps40_vol, sizeof ps40_vol), *ps_volload = pshader(ps40_volload, sizeof ps40_volload);
    ID3D11PixelShader *ps_arr = pshader(ps40_arr, sizeof ps40_arr), *ps_arrload = pshader(ps40_arrload, sizeof ps40_arrload);
    ID3D11PixelShader *ps_arr1 = pshader(ps40_arr1, sizeof ps40_arr1), *ps_dims = pshader(ps40_dims, sizeof ps40_dims);
    ID3D11PixelShader *ps_cubes = pshader(ps41_cubes, sizeof ps41_cubes);
    ID3D11Resource *cube, *vol, *arr, *arr1, *cubes, *rcube;
    ID3D11ShaderResourceView *v;
    ID3D11SamplerState *smp = NULL;
    D3D11_SAMPLER_DESC sd;
    float clear1[4] = {0.05f, 0.05f, 0.05f, 1};
    int f, l;

    memset(&sd, 0, sizeof sd);
    sd.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR; sd.AddressU = sd.AddressV = sd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    sd.MaxAnisotropy = 1; sd.MaxLOD = D3D11_FLOAT32_MAX;
    check("CreateSamplerState", ID3D11Device_CreateSamplerState(dev, &sd, &smp));
    ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &smp);
    ID3D11DeviceContext_PSSetConstantBuffers(ctx, 0, 1, &pb);
    ID3D11DeviceContext_ClearRenderTargetView(ctx, back_rtv, clear1);

    /* The textures. */
    cube = texture(2, 32, 32, 1, 6, 6, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_TEXTURECUBE);
    for (f = 0; f < 6; f++)
        for (l = 0; l < 6; l++)
            fill(cube, l + f * 6, 32 >> l, 32 >> l, 1, f, l);
    vol = texture(3, 16, 16, 8, 2, 1, D3D11_BIND_SHADER_RESOURCE, 0);
    fill(vol, 0, 16, 16, 8, 0, 0);
    fill(vol, 1, 8, 8, 4, 4, 1);
    arr = texture(2, 32, 32, 1, 1, 6, D3D11_BIND_SHADER_RESOURCE, 0);
    for (f = 0; f < 6; f++)
        fill(arr, f, 32, 32, 1, f + 3, 0);
    arr1 = texture(1, 64, 1, 1, 1, 4, D3D11_BIND_SHADER_RESOURCE, 0);
    for (f = 0; f < 4; f++)
        fill(arr1, f, 64, 1, 1, f * 2, 0);
    cubes = texture(2, 32, 32, 1, 1, 12, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_TEXTURECUBE);
    for (f = 0; f < 12; f++)
        fill(cubes, f, 32, 32, 1, 11 - f, 0);

    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &back_rtv, NULL);
    /* 0, 1: a cube at level 0, and between levels 1 and 2. */
    v = srv(cube, D3D11_SRV_DIMENSION_TEXTURECUBE, 0, 0);
    bind_srv(0, v);
    panel(0);
    ID3D11DeviceContext_PSSetShader(ctx, ps_cube, NULL, 0);
    quad();
    consts(1.5f, 0);
    panel(1);
    ID3D11DeviceContext_PSSetShader(ctx, ps_cubelod, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);
    /* 2, 3: a 3D texture sampled (trilinear) and loaded. */
    v = srv(vol, D3D11_SRV_DIMENSION_TEXTURE3D, 0, 0);
    bind_srv(1, v);
    consts(0, 0.4f);
    panel(2);
    ID3D11DeviceContext_PSSetShader(ctx, ps_vol, NULL, 0);
    quad();
    panel(3);
    ID3D11DeviceContext_PSSetShader(ctx, ps_volload, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);
    /* 4-6: an array, all of it, slices 2 and 3, slice 5 alone. */
    v = srv(arr, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 6);
    bind_srv(2, v);
    panel(4);
    ID3D11DeviceContext_PSSetShader(ctx, ps_arr, NULL, 0);
    quad();
    /* 9: the array loaded. */
    panel(9);
    ID3D11DeviceContext_PSSetShader(ctx, ps_arrload, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);
    v = srv(arr, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 2, 2);
    bind_srv(2, v);
    panel(5);
    ID3D11DeviceContext_PSSetShader(ctx, ps_arr, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);
    v = srv(arr, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 5, 1);
    bind_srv(2, v);
    panel(6);
    quad();
    ID3D11ShaderResourceView_Release(v);
    /* 7: a 1D array. */
    v = srv(arr1, D3D11_SRV_DIMENSION_TEXTURE1DARRAY, 0, 4);
    bind_srv(3, v);
    panel(7);
    ID3D11DeviceContext_PSSetShader(ctx, ps_arr1, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);
    /* 8: dimensions. */
    {
        ID3D11ShaderResourceView *a = srv(cube, D3D11_SRV_DIMENSION_TEXTURECUBE, 0, 0), *b = srv(vol, D3D11_SRV_DIMENSION_TEXTURE3D, 0, 0), *c = srv(arr, D3D11_SRV_DIMENSION_TEXTURE2DARRAY, 0, 6);
        bind_srv(0, a); bind_srv(1, b); bind_srv(2, c);
        panel(8);
        ID3D11DeviceContext_PSSetShader(ctx, ps_dims, NULL, 0);
        quad();
        ID3D11ShaderResourceView_Release(a); ID3D11ShaderResourceView_Release(b); ID3D11ShaderResourceView_Release(c);
    }
    /* 10: a cube array, two cubes. */
    v = srv(cubes, D3D11_SRV_DIMENSION_TEXTURECUBEARRAY, 0, 2);
    bind_srv(4, v);
    panel(10);
    ID3D11DeviceContext_PSSetShader(ctx, ps_cubes, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);
    /* 11: each face of a cube cleared through its own view, a triangle
     * drawn into +z, then the cube sampled. */
    rcube = texture(2, 32, 32, 1, 1, 6, D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET, D3D11_RESOURCE_MISC_TEXTURECUBE);
    for (f = 0; f < 6; f++) {
        D3D11_RENDER_TARGET_VIEW_DESC rd;
        ID3D11RenderTargetView *rv = NULL;
        float c[4];
        DWORD bc = base_color[f + 6];
        memset(&rd, 0, sizeof rd);
        rd.Format = DXGI_FORMAT_R8G8B8A8_UNORM; rd.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE2DARRAY;
        rd.Texture2DArray.FirstArraySlice = f; rd.Texture2DArray.ArraySize = 1;
        check("CreateRenderTargetView", ID3D11Device_CreateRenderTargetView(dev, rcube, &rd, &rv));
        c[0] = (bc & 255) / 255.0f; c[1] = ((bc >> 8) & 255) / 255.0f; c[2] = ((bc >> 16) & 255) / 255.0f; c[3] = 1;
        ID3D11DeviceContext_ClearRenderTargetView(ctx, rv, c);
        if (f == 4) {
            D3D11_VIEWPORT vp = {0, 0, 32, 32, 0, 1};
            Vtx t[3] = {{0, 0.8f, 0.5f, RGBA(20, 20, 20, 255), 0, 0}, {0.8f, -0.8f, 0.5f, RGBA(250, 250, 250, 255), 0, 0}, {-0.8f, -0.8f, 0.5f, RGBA(250, 20, 20, 255), 0, 0}};
            /* Coloured by the 3D texture at the triangle's coordinates. */
            ID3D11ShaderResourceView *b = srv(vol, D3D11_SRV_DIMENSION_TEXTURE3D, 0, 0);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rv, NULL);
            ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
            bind_srv(1, b);
            consts(0, 0);
            ID3D11DeviceContext_PSSetShader(ctx, ps_vol, NULL, 0);
            draw(t, 3);
            ID3D11ShaderResourceView_Release(b);
            ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &back_rtv, NULL);
        }
        ID3D11RenderTargetView_Release(rv);
    }
    v = srv(rcube, D3D11_SRV_DIMENSION_TEXTURECUBE, 0, 0);
    bind_srv(0, v);
    panel(11);
    ID3D11DeviceContext_PSSetShader(ctx, ps_cube, NULL, 0);
    quad();
    ID3D11ShaderResourceView_Release(v);

    ID3D11Resource_Release(cube); ID3D11Resource_Release(vol); ID3D11Resource_Release(arr);
    ID3D11Resource_Release(arr1); ID3D11Resource_Release(cubes); ID3D11Resource_Release(rcube);
    ID3D11SamplerState_Release(smp);
    ID3D11PixelShader_Release(ps_cube); ID3D11PixelShader_Release(ps_cubelod); ID3D11PixelShader_Release(ps_vol);
    ID3D11PixelShader_Release(ps_volload); ID3D11PixelShader_Release(ps_arr); ID3D11PixelShader_Release(ps_arrload);
    ID3D11PixelShader_Release(ps_arr1); ID3D11PixelShader_Release(ps_dims); ID3D11PixelShader_Release(ps_cubes);
}

/* What views of these kinds give back. */
static void checks(void)
{
    ID3D11Resource *r = texture(2, 16, 16, 1, 1, 6, D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_TEXTURECUBE);
    ID3D11ShaderResourceView *v = NULL;
    D3D11_SHADER_RESOURCE_VIEW_DESC d;
    if (SUCCEEDED(ID3D11Device_CreateShaderResourceView(dev, r, NULL, &v))) {
        ID3D11ShaderResourceView_GetDesc(v, &d);
        printf("a cube's default view: dimension %d, %u levels\n", (int)d.ViewDimension, d.TextureCube.MipLevels);
        ID3D11ShaderResourceView_Release(v);
    }
    ID3D11Resource_Release(r);
    r = texture(3, 8, 8, 4, 0, 1, D3D11_BIND_SHADER_RESOURCE, 0);
    {
        D3D11_TEXTURE3D_DESC t;
        ID3D11Texture3D_GetDesc((ID3D11Texture3D *)r, &t);
        printf("8x8x4 3D texture: %u levels\n", t.MipLevels);
    }
    ID3D11Resource_Release(r);
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
    D3D_FEATURE_LEVEL want[1] = {D3D_FEATURE_LEVEL_10_1}, got;
    D3D_DRIVER_TYPE driver = D3D_DRIVER_TYPE_WARP;
    D3D11_INPUT_ELEMENT_DESC e[3] = {
        {"POSITION", 0, DXGI_FORMAT_R32G32B32_FLOAT, 0, 0, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"COLOR", 0, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_INPUT_PER_VERTEX_DATA, 0},
        {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, D3D11_APPEND_ALIGNED_ELEMENT, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11Texture2D *st;
    D3D11_TEXTURE2D_DESC td;
    D3D11_MAPPED_SUBRESOURCE mp;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    if (argc > 1 && strcmp(argv[1], "gpu") == 0)
        driver = D3D_DRIVER_TYPE_HARDWARE;
    hr = D3D11CreateDevice(NULL, driver, NULL, 0, want, 1, D3D11_SDK_VERSION, &dev, &got, &ctx);
    printf("device: %s, feature level 0x%x\n", SUCCEEDED(hr) ? "created" : "FAILED", (int)got);
    if (FAILED(hr))
        return 1;
    back = (ID3D11Texture2D *)texture(2, W, H, 1, 1, 1, D3D11_BIND_RENDER_TARGET, 0);
    check("CreateRenderTargetView", ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &back_rtv));
    pb = buffer(D3D11_BIND_CONSTANT_BUFFER, NULL, 16);
    check("CreateVertexShader", ID3D11Device_CreateVertexShader(dev, vs40_main, sizeof vs40_main, NULL, &vs));
    check("CreateInputLayout", ID3D11Device_CreateInputLayout(dev, e, 3, vs40_main, sizeof vs40_main, &layout));
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    panels();
    checks();

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
    ID3D11Buffer_Release(pb);
    ID3D11RenderTargetView_Release(back_rtv);
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
    strcpy(slash ? slash + 1 : path, "dx3.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
