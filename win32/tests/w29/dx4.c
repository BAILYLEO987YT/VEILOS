/* G5f2b test: Direct3D 11 at feature level 11_0: compute shaders writing a
 * texture through an unordered access view (shown by a ps_5_0 shader),
 * structured, raw and typed buffers, a reduction in group shared memory
 * with barriers, and atomics.
 *
 * Draws two 64x64 panels of a 384x128 target (the others stay clear) and
 * prints what the buffers hold; compared with what WARP drew and printed
 * on Windows. `dx4 gpu` runs on this machine's GPU instead.
 *
 *   dx4                 compare with dx4.ref next to the program
 *   dx4 record FILE     write what WARP draws as FILE (a PPM)
 *   dx4 gpu             compare, run on the GPU
 *   dx4 stats           as dx4, with each panel's differences counted
 *
 * In each panel at most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/dx4_cs50_pattern.h"
#include "build/dx4_cs50_buffers.h"
#include "build/dx4_cs50_reduce.h"
#include "build/dx4_cs50_hist.h"
#include "build/dx4_vs50_main.h"
#include "build/dx4_ps50_show.h"

#define W 384
#define H 128
#define PANELS 12

typedef struct {
    float x, y, z, u, v;
} Vtx;

static unsigned char ref[W * H * 3];
static int stats;
static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;

static void check(const char *what, HRESULT hr)
{
    if (FAILED(hr))
        printf("%s: 0x%lx\n", what, (unsigned long)hr);
}

static ID3D11Buffer *buffer(UINT bind, UINT misc, UINT stride, const void *data, UINT size)
{
    D3D11_BUFFER_DESC d;
    D3D11_SUBRESOURCE_DATA s;
    ID3D11Buffer *b = NULL;
    memset(&d, 0, sizeof d);
    d.ByteWidth = size; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = bind; d.MiscFlags = misc; d.StructureByteStride = stride;
    s.pSysMem = data; s.SysMemPitch = 0; s.SysMemSlicePitch = 0;
    check("CreateBuffer", ID3D11Device_CreateBuffer(dev, &d, data ? &s : NULL, &b));
    return b;
}

/* A buffer's bytes, through a staging copy. */
static void read_back(ID3D11Buffer *b, void *out, UINT size)
{
    D3D11_BUFFER_DESC d;
    ID3D11Buffer *st = NULL;
    D3D11_MAPPED_SUBRESOURCE m;
    memset(&d, 0, sizeof d);
    d.ByteWidth = size; d.Usage = D3D11_USAGE_STAGING; d.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
    check("CreateBuffer (staging)", ID3D11Device_CreateBuffer(dev, &d, NULL, &st));
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)b);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &m))) {
        memcpy(out, m.pData, size);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
    }
    ID3D11Buffer_Release(st);
}

static ID3D11UnorderedAccessView *uav_buffer(ID3D11Buffer *b, DXGI_FORMAT f, UINT n, UINT flags)
{
    D3D11_UNORDERED_ACCESS_VIEW_DESC d;
    ID3D11UnorderedAccessView *v = NULL;
    memset(&d, 0, sizeof d);
    d.Format = f; d.ViewDimension = D3D11_UAV_DIMENSION_BUFFER; d.Buffer.NumElements = n; d.Buffer.Flags = flags;
    check("CreateUnorderedAccessView", ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)b, &d, &v));
    return v;
}

static ID3D11ShaderResourceView *srv_buffer(ID3D11Buffer *b, DXGI_FORMAT f, UINT n)
{
    D3D11_SHADER_RESOURCE_VIEW_DESC d;
    ID3D11ShaderResourceView *v = NULL;
    memset(&d, 0, sizeof d);
    d.Format = f; d.ViewDimension = D3D11_SRV_DIMENSION_BUFFER; d.Buffer.NumElements = n;
    check("CreateShaderResourceView", ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)b, &d, &v));
    return v;
}

static ID3D11ComputeShader *cshader(const void *code, SIZE_T len)
{
    ID3D11ComputeShader *c = NULL;
    check("CreateComputeShader", ID3D11Device_CreateComputeShader(dev, code, len, NULL, &c));
    return c;
}

static void set_uav(UINT slot, ID3D11UnorderedAccessView *v)
{
    ID3D11DeviceContext_CSSetUnorderedAccessViews(ctx, slot, 1, &v, NULL);
}

static void consts(ID3D11Buffer *cb, float fx)
{
    UINT k[8];
    float f = fx;
    memset(k, 0, sizeof k);
    memcpy(&k[4], &f, 4);
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)cb, 0, NULL, k, 0, 0);
}

/* A compute shader writes a texture, a pixel shader shows it in panel n. */
static void pattern_panel(ID3D11Buffer *cb, int size, float fx, int n, ID3D11RenderTargetView *rtv)
{
    D3D11_TEXTURE2D_DESC d;
    ID3D11Texture2D *t = NULL;
    ID3D11UnorderedAccessView *u = NULL;
    ID3D11ShaderResourceView *s = NULL;
    ID3D11ComputeShader *cs = cshader(cs50_pattern, sizeof cs50_pattern);
    D3D11_VIEWPORT vp;
    Vtx q[6] = {{-1, 1, 0.5f, 0, 0}, {1, 1, 0.5f, 1, 0}, {1, -1, 0.5f, 1, 1}, {-1, 1, 0.5f, 0, 0}, {1, -1, 0.5f, 1, 1}, {-1, -1, 0.5f, 0, 1}};
    ID3D11Buffer *vb = buffer(D3D11_BIND_VERTEX_BUFFER, 0, 0, q, sizeof q);
    UINT stride = sizeof(Vtx), off = 0;
    ID3D11UnorderedAccessView *nil = NULL;

    memset(&d, 0, sizeof d);
    d.Width = size; d.Height = size; d.MipLevels = 1; d.ArraySize = 1; d.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    d.SampleDesc.Count = 1; d.Usage = D3D11_USAGE_DEFAULT; d.BindFlags = D3D11_BIND_UNORDERED_ACCESS | D3D11_BIND_SHADER_RESOURCE;
    check("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &d, NULL, &t));
    check("CreateUnorderedAccessView (texture)", ID3D11Device_CreateUnorderedAccessView(dev, (ID3D11Resource *)t, NULL, &u));
    check("CreateShaderResourceView", ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)t, NULL, &s));
    consts(cb, fx);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetConstantBuffers(ctx, 0, 1, &cb);
    set_uav(0, u);
    ID3D11DeviceContext_Dispatch(ctx, size / 8, size / 8, 1);
    set_uav(0, nil);

    vp.TopLeftX = (float)((n % 6) * 64); vp.TopLeftY = (float)((n / 6) * 64); vp.Width = 64; vp.Height = 64; vp.MinDepth = 0; vp.MaxDepth = 1;
    ID3D11DeviceContext_OMSetRenderTargets(ctx, 1, &rtv, NULL);
    ID3D11DeviceContext_RSSetViewports(ctx, 1, &vp);
    ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &s);
    ID3D11DeviceContext_IASetVertexBuffers(ctx, 0, 1, &vb, &stride, &off);
    ID3D11DeviceContext_IASetPrimitiveTopology(ctx, D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    ID3D11DeviceContext_Draw(ctx, 6, 0);
    {
        ID3D11ShaderResourceView *ns = NULL;
        ID3D11DeviceContext_PSSetShaderResources(ctx, 0, 1, &ns);
    }
    ID3D11Buffer_Release(vb);
    ID3D11ComputeShader_Release(cs);
    ID3D11UnorderedAccessView_Release(u);
    ID3D11ShaderResourceView_Release(s);
    ID3D11Texture2D_Release(t);
}

typedef struct {
    float a;
    UINT b;
    float c[2];
} Item;

static void buffers(void)
{
    static Item items[256];
    static UINT raw[256 * 2];
    ID3D11Buffer *ib, *rb;
    ID3D11UnorderedAccessView *iu, *ru;
    ID3D11ComputeShader *cs = cshader(cs50_buffers, sizeof cs50_buffers);
    UINT i, sum = 0;
    double fs = 0;
    for (i = 0; i < 256; i++) {
        items[i].a = (float)i * 0.5f - 20.0f;
        items[i].b = i * 7u + 1u;
        items[i].c[0] = (float)i;
        items[i].c[1] = 1.0f / (float)(i + 1);
    }
    ib = buffer(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, sizeof(Item), items, sizeof items);
    rb = buffer(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, 0, NULL, sizeof raw);
    iu = uav_buffer(ib, DXGI_FORMAT_UNKNOWN, 256, 0);
    ru = uav_buffer(rb, DXGI_FORMAT_R32_TYPELESS, 512, D3D11_BUFFER_UAV_FLAG_RAW);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    set_uav(1, iu);
    set_uav(2, ru);
    ID3D11DeviceContext_Dispatch(ctx, 4, 1, 1);
    read_back(ib, items, sizeof items);
    read_back(rb, raw, sizeof raw);
    for (i = 0; i < 256; i++) {
        sum = sum * 31u + items[i].b + raw[i * 2] * 3u + raw[i * 2 + 1];
        fs += items[i].a + items[i].c[0] * 2.0 + items[i].c[1];
    }
    printf("structured: item 3 = (%.6f, %u, %.6f, %.6f), item 200 = (%.6f, %u, %.6f, %.6f)\n", items[3].a, items[3].b, items[3].c[0], items[3].c[1], items[200].a, items[200].b, items[200].c[0], items[200].c[1]);
    printf("raw: [10] = %u %u, [255] = %u %u\n", raw[20], raw[21], raw[510], raw[511]);
    printf("checksums: %u, %.4f\n", sum, fs);
    ID3D11UnorderedAccessView_Release(iu);
    ID3D11UnorderedAccessView_Release(ru);
    ID3D11Buffer_Release(ib);
    ID3D11Buffer_Release(rb);
    ID3D11ComputeShader_Release(cs);
}

static void reduce(void)
{
    static float vals[512];
    float totals[4];
    ID3D11Buffer *vb, *tb;
    ID3D11ShaderResourceView *vs;
    ID3D11UnorderedAccessView *tu;
    ID3D11ComputeShader *cs = cshader(cs50_reduce, sizeof cs50_reduce);
    int i;
    for (i = 0; i < 512; i++)
        vals[i] = (float)(i % 37) * 0.25f + (float)(i / 128);
    vb = buffer(D3D11_BIND_SHADER_RESOURCE, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 4, vals, sizeof vals);
    tb = buffer(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 4, NULL, 16);
    vs = srv_buffer(vb, DXGI_FORMAT_UNKNOWN, 512);
    tu = uav_buffer(tb, DXGI_FORMAT_UNKNOWN, 4, 0);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 0, 1, &vs);
    set_uav(1, tu);
    ID3D11DeviceContext_Dispatch(ctx, 4, 1, 1);
    read_back(tb, totals, sizeof totals);
    printf("reduced: %.4f %.4f %.4f %.4f\n", totals[0], totals[1], totals[2], totals[3]);
    ID3D11ShaderResourceView_Release(vs);
    ID3D11UnorderedAccessView_Release(tu);
    ID3D11Buffer_Release(vb);
    ID3D11Buffer_Release(tb);
    ID3D11ComputeShader_Release(cs);
}

static void hist(void)
{
    static UINT keys[256], h[32], order[16], olds[256];
    ID3D11Buffer *kb, *hb, *ob, *db;
    ID3D11ShaderResourceView *ks;
    ID3D11UnorderedAccessView *hu, *ou, *du;
    ID3D11ComputeShader *cs = cshader(cs50_hist, sizeof cs50_hist);
    UINT i, x = 12345, sums[16];
    for (i = 0; i < 256; i++) {
        x = x * 1103515245u + 12345u;
        keys[i] = x >> 16;
    }
    for (i = 0; i < 16; i++) {
        h[i] = 0;
        h[16 + i] = 0xFFFFFFFFu;
    }
    kb = buffer(D3D11_BIND_SHADER_RESOURCE, 0, 0, keys, sizeof keys);
    hb = buffer(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_ALLOW_RAW_VIEWS, 0, h, sizeof h);
    ob = buffer(D3D11_BIND_UNORDERED_ACCESS, 0, 0, NULL, sizeof order);
    db = buffer(D3D11_BIND_UNORDERED_ACCESS, D3D11_RESOURCE_MISC_BUFFER_STRUCTURED, 4, NULL, sizeof olds);
    ks = srv_buffer(kb, DXGI_FORMAT_R32_UINT, 256);
    hu = uav_buffer(hb, DXGI_FORMAT_R32_TYPELESS, 32, D3D11_BUFFER_UAV_FLAG_RAW);
    ou = uav_buffer(ob, DXGI_FORMAT_R32_UINT, 16, 0);
    du = uav_buffer(db, DXGI_FORMAT_UNKNOWN, 256, 0);
    ID3D11DeviceContext_CSSetShader(ctx, cs, NULL, 0);
    ID3D11DeviceContext_CSSetShaderResources(ctx, 1, 1, &ks);
    set_uav(2, hu);
    set_uav(3, ou);
    set_uav(4, du);
    ID3D11DeviceContext_Dispatch(ctx, 8, 1, 1);
    read_back(hb, h, sizeof h);
    read_back(ob, order, sizeof order);
    read_back(db, olds, sizeof olds);
    for (i = 0; i < 16; i++)
        sums[i] = 0;
    for (i = 0; i < 256; i++)
        sums[keys[i] & 15] += olds[i];
    printf("histogram:");
    for (i = 0; i < 16; i++)
        printf(" %u", h[i]);
    printf("\nlast of each:");
    for (i = 0; i < 16; i++)
        printf(" %u", order[i]);
    printf("\nfirst of each:");
    for (i = 0; i < 16; i++)
        printf(" %u", h[16 + i]);
    printf("\nold values add up (n(n-1)/2 each):");
    for (i = 0; i < 16; i++)
        printf(" %s", sums[i] == h[i] * (h[i] - 1) / 2 ? "yes" : "NO");
    printf("\n");
    ID3D11ShaderResourceView_Release(ks);
    ID3D11UnorderedAccessView_Release(hu);
    ID3D11UnorderedAccessView_Release(ou);
    ID3D11UnorderedAccessView_Release(du);
    ID3D11Buffer_Release(kb);
    ID3D11Buffer_Release(hb);
    ID3D11Buffer_Release(ob);
    ID3D11Buffer_Release(db);
    ID3D11ComputeShader_Release(cs);
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
        {"TEXCOORD", 0, DXGI_FORMAT_R32G32_FLOAT, 0, 12, D3D11_INPUT_PER_VERTEX_DATA, 0},
    };
    ID3D11InputLayout *layout = NULL;
    ID3D11VertexShader *vs = NULL;
    ID3D11PixelShader *ps = NULL;
    ID3D11SamplerState *smp = NULL;
    D3D11_SAMPLER_DESC sd;
    ID3D11Texture2D *back, *st;
    ID3D11RenderTargetView *rtv = NULL;
    ID3D11Buffer *cb;
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
    printf("default feature level: 0x%x\n", (int)ID3D11Device_GetFeatureLevel(dev));
    memset(&td, 0, sizeof td);
    td.Width = W; td.Height = H; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    td.SampleDesc.Count = 1; td.Usage = D3D11_USAGE_DEFAULT; td.BindFlags = D3D11_BIND_RENDER_TARGET;
    check("CreateTexture2D", ID3D11Device_CreateTexture2D(dev, &td, NULL, &back));
    check("CreateRenderTargetView", ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)back, NULL, &rtv));
    ID3D11DeviceContext_ClearRenderTargetView(ctx, rtv, clear1);
    cb = buffer(D3D11_BIND_CONSTANT_BUFFER, 0, 0, NULL, 32);
    check("CreateVertexShader", ID3D11Device_CreateVertexShader(dev, vs50_main, sizeof vs50_main, NULL, &vs));
    check("CreatePixelShader", ID3D11Device_CreatePixelShader(dev, ps50_show, sizeof ps50_show, NULL, &ps));
    check("CreateInputLayout", ID3D11Device_CreateInputLayout(dev, e, 2, vs50_main, sizeof vs50_main, &layout));
    memset(&sd, 0, sizeof sd);
    sd.Filter = D3D11_FILTER_MIN_MAG_MIP_POINT; sd.AddressU = sd.AddressV = sd.AddressW = D3D11_TEXTURE_ADDRESS_CLAMP;
    sd.MaxAnisotropy = 1; sd.MaxLOD = D3D11_FLOAT32_MAX;
    check("CreateSamplerState", ID3D11Device_CreateSamplerState(dev, &sd, &smp));
    ID3D11DeviceContext_IASetInputLayout(ctx, layout);
    ID3D11DeviceContext_VSSetShader(ctx, vs, NULL, 0);
    ID3D11DeviceContext_PSSetShader(ctx, ps, NULL, 0);
    ID3D11DeviceContext_PSSetSamplers(ctx, 0, 1, &smp);
    pattern_panel(cb, 64, 0.0f, 0, rtv);
    pattern_panel(cb, 32, 1.5f, 1, rtv);
    buffers();
    reduce();
    hist();

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
    ID3D11PixelShader_Release(ps);
    ID3D11SamplerState_Release(smp);
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
    strcpy(slash ? slash + 1 : path, "dx4.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    return compare(img);
}
