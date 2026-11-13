/* G5f1a test: DXGI and Direct3D 11 without shaders: the factory, adapter
 * and output, a device (feature level 10_1 or lower) with a swap chain,
 * buffers and textures, views, Map, UpdateSubresource, copies, clears,
 * GenerateMips, depth, state objects and queries.
 *
 * Builds a 384x128 back buffer out of twelve 64x64 panels, each made by
 * clears and copies, reads it back through a staging texture, and compares
 * it with what WARP (Windows' software renderer) made; `dx1 gpu` makes it
 * on this machine's GPU instead, as a second check.
 *
 *   dx1                 compare with dx1.ref next to the program
 *   dx1 record FILE     write what WARP makes as FILE (a PPM)
 *   dx1 gpu             compare, made on the GPU
 *   dx1 stats           as dx1, with each panel's differences counted
 *
 * Clears and copies are exact, so the tolerance is tight: in each panel at
 * most 1% of pixels (40) may differ by more than 4.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d11.h>
#include <dxgi1_2.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 384
#define H 128
#define PANELS 12

static unsigned char ref[W * H * 3];
static int stats;
static ID3D11Device *dev;
static ID3D11DeviceContext *ctx;
static ID3D11Texture2D *back;

static ID3D11Texture2D *texture(int w, int h, int mips, DXGI_FORMAT f, UINT bind, D3D11_USAGE usage, UINT cpu, const void *data, int pitch)
{
    D3D11_TEXTURE2D_DESC d;
    D3D11_SUBRESOURCE_DATA s;
    ID3D11Texture2D *t = NULL;
    HRESULT hr;
    memset(&d, 0, sizeof d);
    d.Width = w; d.Height = h; d.MipLevels = mips; d.ArraySize = 1; d.Format = f;
    d.SampleDesc.Count = 1; d.Usage = usage; d.BindFlags = bind; d.CPUAccessFlags = cpu;
    d.MiscFlags = mips != 1 && (bind & D3D11_BIND_RENDER_TARGET) && (bind & D3D11_BIND_SHADER_RESOURCE) ? D3D11_RESOURCE_MISC_GENERATE_MIPS : 0;
    s.pSysMem = data; s.SysMemPitch = pitch; s.SysMemSlicePitch = 0;
    hr = ID3D11Device_CreateTexture2D(dev, &d, data ? &s : NULL, &t);
    if (FAILED(hr))
        printf("CreateTexture2D %dx%d format %d mips %d bind 0x%x: 0x%lx\n", w, h, (int)f, mips, bind, (unsigned long)hr);
    return t;
}

/* Copy a 64x64 (or smaller) texture's mip into panel n of the back buffer. */
static void to_panel(ID3D11Texture2D *t, UINT sub, int n, int dx, int dy)
{
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)back, 0, (n % 6) * 64 + dx, (n / 6) * 64 + dy, 0, (ID3D11Resource *)t, sub, NULL);
}

static ID3D11RenderTargetView *rtv_of(ID3D11Texture2D *t)
{
    ID3D11RenderTargetView *v = NULL;
    ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t, NULL, &v);
    return v;
}

static void clear(ID3D11Texture2D *t, float r, float g, float b, float a)
{
    float c[4];
    ID3D11RenderTargetView *v = rtv_of(t);
    c[0] = r; c[1] = g; c[2] = b; c[3] = a;
    ID3D11DeviceContext_ClearRenderTargetView(ctx, v, c);
    ID3D11RenderTargetView_Release(v);
}

static void panels(void)
{
    static DWORD pat[64 * 64], tile[16 * 16];
    ID3D11Texture2D *t, *m, *dyn, *s565;
    D3D11_MAPPED_SUBRESOURCE mp;
    D3D11_BOX box;
    int x, y;

    /* 0: a cleared render target. */
    t = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, 0, NULL, 0);
    clear(t, 0.8f, 0.3f, 0.1f, 1);
    to_panel(t, 0, 0, 0, 0);
    /* 1: four clears of one target, each copied into a quarter. */
    clear(t, 0.1f, 0.6f, 0.2f, 1); box.left = 0; box.top = 0; box.front = 0; box.right = 32; box.bottom = 32; box.back = 1;
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)back, 0, 64, 0, 0, (ID3D11Resource *)t, 0, &box);
    clear(t, 0.1f, 0.2f, 0.7f, 1);
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)back, 0, 96, 0, 0, (ID3D11Resource *)t, 0, &box);
    clear(t, 0.9f, 0.9f, 0.2f, 1);
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)back, 0, 64, 32, 0, (ID3D11Resource *)t, 0, &box);
    clear(t, 0.5f, 0.5f, 0.5f, 1);
    ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)back, 0, 96, 32, 0, (ID3D11Resource *)t, 0, &box);
    ID3D11Texture2D_Release(t);

    /* 2: initial data. */
    for (y = 0; y < 64; y++)
        for (x = 0; x < 64; x++)
            pat[y * 64 + x] = 0xFF000000 | (DWORD)(x * 4) | (DWORD)(y * 4) << 8 | (DWORD)((x ^ y) * 4) << 16;
    t = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_DEFAULT, 0, pat, 256);
    to_panel(t, 0, 2, 0, 0);
    /* 3: UpdateSubresource into a box of it. */
    for (y = 0; y < 16; y++)
        for (x = 0; x < 16; x++)
            tile[y * 16 + x] = 0xFF2080F0 - (DWORD)(x * 8) - (DWORD)(y * 0x800);
    box.left = 20; box.top = 12; box.right = 36; box.bottom = 28;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)t, 0, &box, tile, 64, 0);
    to_panel(t, 0, 3, 0, 0);
    ID3D11Texture2D_Release(t);

    /* 4: a dynamic texture written with Map(WRITE_DISCARD). */
    dyn = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_SHADER_RESOURCE, D3D11_USAGE_DYNAMIC, D3D11_CPU_ACCESS_WRITE, NULL, 0);
    if (SUCCEEDED(ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)dyn, 0, D3D11_MAP_WRITE_DISCARD, 0, &mp))) {
        for (y = 0; y < 64; y++)
            for (x = 0; x < 64; x++)
                ((DWORD *)((char *)mp.pData + y * mp.RowPitch))[x] = ((x / 8 + y / 8) & 1) ? 0xFFE0E0E0 : 0xFF203060;
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)dyn, 0);
    }
    to_panel(dyn, 0, 4, 0, 0);
    ID3D11Texture2D_Release(dyn);

    /* 5: a B5G6R5 texture copied through an R8G8B8A8 one? Formats must match: so B8G8R8A8 into a B8G8R8A8 target, then CopyResource. */
    {
        ID3D11Texture2D *a, *b;
        DWORD bgra[64 * 64];
        for (y = 0; y < 64; y++)
            for (x = 0; x < 64; x++)
                bgra[y * 64 + x] = 0xFF000000 | (DWORD)(x * 4) << 16 | (DWORD)(255 - y * 4);
        a = texture(64, 64, 1, DXGI_FORMAT_B8G8R8A8_UNORM, 0, D3D11_USAGE_DEFAULT, 0, bgra, 256);
        b = texture(64, 64, 1, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, 0, NULL, 0);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)b, (ID3D11Resource *)a);
        /* Its pixels, read back through a staging copy, into an RGBA texture. */
        {
            ID3D11Texture2D *st = texture(64, 64, 1, DXGI_FORMAT_B8G8R8A8_UNORM, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ, NULL, 0);
            ID3D11Texture2D *rgba;
            static DWORD conv[64 * 64];
            ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)b);
            ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &mp);
            for (y = 0; y < 64; y++)
                for (x = 0; x < 64; x++) {
                    DWORD p = ((DWORD *)((char *)mp.pData + y * mp.RowPitch))[x];
                    conv[y * 64 + x] = (p & 0xFF00FF00) | (p >> 16 & 0xFF) | (p & 0xFF) << 16;
                }
            printf("B8G8R8A8 texel (10, 20): 0x%08lx\n", ((DWORD *)((char *)mp.pData + 20 * mp.RowPitch))[10]);
            ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
            rgba = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_DEFAULT, 0, conv, 256);
            to_panel(rgba, 0, 5, 0, 0);
            ID3D11Texture2D_Release(rgba);
            ID3D11Texture2D_Release(st);
        }
        ID3D11Texture2D_Release(a);
        ID3D11Texture2D_Release(b);
    }

    /* 6, 7: GenerateMips: mips 1 and 2 of a pattern, side by side. */
    m = texture(64, 64, 0, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE, D3D11_USAGE_DEFAULT, 0, NULL, 0);
    for (y = 0; y < 64; y++)
        for (x = 0; x < 64; x++)
            pat[y * 64 + x] = ((x / 2 + y / 2) & 1) ? 0xFFFFFFFF : 0xFF000000 | (DWORD)(x * 4) << 8;
    ID3D11DeviceContext_UpdateSubresource(ctx, (ID3D11Resource *)m, 0, NULL, pat, 256, 0);
    {
        ID3D11ShaderResourceView *srv = NULL;
        ID3D11Device_CreateShaderResourceView(dev, (ID3D11Resource *)m, NULL, &srv);
        ID3D11DeviceContext_GenerateMips(ctx, srv);
        ID3D11ShaderResourceView_Release(srv);
    }
    to_panel(m, 1, 6, 0, 0);
    to_panel(m, 2, 6, 32, 0);
    to_panel(m, 3, 6, 32, 16);
    to_panel(m, 0, 7, 0, 0);
    ID3D11Texture2D_Release(m);

    /* 8: a render target, cleared, a box of another copied in, cleared
     * again only where a view of mip 1 covers. */
    t = texture(64, 64, 2, DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_RENDER_TARGET, D3D11_USAGE_DEFAULT, 0, NULL, 0);
    {
        D3D11_RENDER_TARGET_VIEW_DESC vd;
        ID3D11RenderTargetView *v1 = NULL;
        float c[4] = {0.2f, 0.9f, 0.9f, 1};
        clear(t, 0.4f, 0.1f, 0.4f, 1);
        memset(&vd, 0, sizeof vd);
        vd.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
        vd.ViewDimension = D3D11_RTV_DIMENSION_TEXTURE2D;
        vd.Texture2D.MipSlice = 1;
        ID3D11Device_CreateRenderTargetView(dev, (ID3D11Resource *)t, &vd, &v1);
        ID3D11DeviceContext_ClearRenderTargetView(ctx, v1, c);
        ID3D11RenderTargetView_GetDesc(v1, &vd);
        printf("mip view: format %d, dimension %d, mip %u\n", (int)vd.Format, (int)vd.ViewDimension, vd.Texture2D.MipSlice);
        ID3D11RenderTargetView_Release(v1);
    }
    to_panel(t, 0, 8, 0, 0);
    to_panel(t, 1, 8, 16, 16);
    ID3D11Texture2D_Release(t);

    /* 9: a staging texture written by Map, copied up. */
    s565 = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_WRITE | D3D11_CPU_ACCESS_READ, NULL, 0);
    ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)s565, 0, D3D11_MAP_WRITE, 0, &mp);
    for (y = 0; y < 64; y++)
        for (x = 0; x < 64; x++)
            ((DWORD *)((char *)mp.pData + y * mp.RowPitch))[x] = 0xFF000000 | (DWORD)((x * y) & 0xFF) << 8 | (DWORD)(x * 4);
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)s565, 0);
    to_panel(s565, 0, 9, 0, 0);
    ID3D11Texture2D_Release(s565);

    /* 10: a buffer copied into another and back out through Map. */
    {
        D3D11_BUFFER_DESC bd;
        D3D11_SUBRESOURCE_DATA sd;
        ID3D11Buffer *b1, *b2, *st;
        static DWORD words[64 * 64];
        memset(&bd, 0, sizeof bd);
        bd.ByteWidth = sizeof words; bd.Usage = D3D11_USAGE_DEFAULT; bd.BindFlags = D3D11_BIND_VERTEX_BUFFER;
        for (x = 0; x < 64 * 64; x++) words[x] = 0xFF000000 | (DWORD)x * 0x010203;
        sd.pSysMem = words; sd.SysMemPitch = 0; sd.SysMemSlicePitch = 0;
        ID3D11Device_CreateBuffer(dev, &bd, &sd, &b1);
        ID3D11Device_CreateBuffer(dev, &bd, NULL, &b2);
        box.left = 1024; box.right = 8192; box.top = 0; box.bottom = 1; box.front = 0; box.back = 1;
        ID3D11DeviceContext_CopySubresourceRegion(ctx, (ID3D11Resource *)b2, 0, 4096, 0, 0, (ID3D11Resource *)b1, 0, &box);
        bd.Usage = D3D11_USAGE_STAGING; bd.BindFlags = 0; bd.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        ID3D11Device_CreateBuffer(dev, &bd, NULL, &st);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)b2);
        ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &mp);
        memcpy(words, mp.pData, sizeof words);
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
        for (x = 0; x < 64 * 64; x++) words[x] |= 0xFF000000;
        t = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_DEFAULT, 0, words, 256);
        to_panel(t, 0, 10, 0, 0);
        ID3D11Texture2D_Release(t);
        ID3D11Buffer_Release(b1);
        ID3D11Buffer_Release(b2);
        ID3D11Buffer_Release(st);
    }

    /* 11: depth: cleared, copied to staging, its values shown as grey. */
    {
        ID3D11Texture2D *d = texture(64, 64, 1, DXGI_FORMAT_D24_UNORM_S8_UINT, D3D11_BIND_DEPTH_STENCIL, D3D11_USAGE_DEFAULT, 0, NULL, 0);
        ID3D11Texture2D *st = texture(64, 64, 1, DXGI_FORMAT_D24_UNORM_S8_UINT, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ, NULL, 0);
        ID3D11DepthStencilView *dsv = NULL;
        static DWORD grey[64 * 64];
        DWORD v;
        ID3D11Device_CreateDepthStencilView(dev, (ID3D11Resource *)d, NULL, &dsv);
        ID3D11DeviceContext_ClearDepthStencilView(ctx, dsv, D3D11_CLEAR_DEPTH | D3D11_CLEAR_STENCIL, 0.25f, 0x5A);
        ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)d);
        ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &mp);
        v = ((DWORD *)mp.pData)[0];
        printf("depth texel: depth 0x%06lx, stencil 0x%02lx\n", v & 0xFFFFFF, v >> 24);
        for (x = 0; x < 64 * 64; x++) grey[x] = 0xFF000000 | ((v >> 16) & 0xFF) * 0x010101;
        ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
        t = texture(64, 64, 1, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_DEFAULT, 0, grey, 256);
        to_panel(t, 0, 11, 0, 0);
        ID3D11Texture2D_Release(t);
        ID3D11DepthStencilView_Release(dsv);
        ID3D11Texture2D_Release(d);
        ID3D11Texture2D_Release(st);
    }
}

static void checks(IDXGIFactory1 *f)
{
    IDXGIAdapter1 *a = NULL;
    IDXGIOutput *o = NULL;
    IDXGIDevice *dd = NULL;
    IDXGISurface *surf = NULL;
    DXGI_ADAPTER_DESC1 ad;
    DXGI_SURFACE_DESC sd;
    UINT n = 0, support = 0;
    D3D11_BLEND_DESC bd;
    D3D11_RASTERIZER_DESC rd;
    D3D11_SAMPLER_DESC smd;
    ID3D11BlendState *bs;
    ID3D11RasterizerState *rs;
    ID3D11SamplerState *ss;
    ID3D11Query *q;
    D3D11_QUERY_DESC qd;
    BOOL done = FALSE;
    HRESULT hr;
    int spins = 0;

    printf("EnumAdapters1(0): %s\n", SUCCEEDED(IDXGIFactory1_EnumAdapters1(f, 0, &a)) ? "ok" : "FAILED");
    IDXGIAdapter1_GetDesc1(a, &ad);
    printf("adapter has a name: %s\n", ad.Description[0] ? "yes" : "no");
    printf("EnumOutputs(0): %s\n", SUCCEEDED(IDXGIAdapter1_EnumOutputs(a, 0, &o)) ? "ok" : "none");
    if (o) {
        IDXGIOutput_GetDisplayModeList(o, DXGI_FORMAT_R8G8B8A8_UNORM, 0, &n, NULL);
        printf("display modes: %s\n", n > 0 ? "some" : "none");
        IDXGIOutput_Release(o);
    }
    IDXGIAdapter1_Release(a);
    hr = ID3D11Device_QueryInterface(dev, &IID_IDXGIDevice, (void **)&dd);
    printf("device as IDXGIDevice: %s\n", SUCCEEDED(hr) ? "yes" : "no");
    if (dd) {
        IDXGIAdapter *pa = NULL;
        printf("its adapter: %s\n", SUCCEEDED(IDXGIDevice_GetAdapter(dd, &pa)) && pa ? "yes" : "no");
        if (pa) IDXGIAdapter_Release(pa);
        IDXGIDevice_Release(dd);
    }
    hr = ID3D11Texture2D_QueryInterface(back, &IID_IDXGISurface, (void **)&surf);
    printf("back buffer as IDXGISurface: %s\n", SUCCEEDED(hr) ? "yes" : "no");
    if (surf) {
        IDXGISurface_GetDesc(surf, &sd);
        printf("  %ux%u, format %d\n", sd.Width, sd.Height, (int)sd.Format);
        IDXGISurface_Release(surf);
    }
    ID3D11Device_CheckFormatSupport(dev, DXGI_FORMAT_R8G8B8A8_UNORM, &support);
    printf("R8G8B8A8: render target %s, sample %s\n", support & D3D11_FORMAT_SUPPORT_RENDER_TARGET ? "yes" : "no", support & D3D11_FORMAT_SUPPORT_SHADER_SAMPLE ? "yes" : "no");
    ID3D11Device_CheckFormatSupport(dev, DXGI_FORMAT_D24_UNORM_S8_UINT, &support);
    printf("D24S8: depth %s\n", support & D3D11_FORMAT_SUPPORT_DEPTH_STENCIL ? "yes" : "no");
    ID3D11Device_CheckMultisampleQualityLevels(dev, DXGI_FORMAT_R8G8B8A8_UNORM, 1, &n);
    printf("1x quality levels: %u\n", n);

    memset(&bd, 0, sizeof bd);
    bd.RenderTarget[0].BlendEnable = TRUE;
    bd.RenderTarget[0].SrcBlend = D3D11_BLEND_SRC_ALPHA; bd.RenderTarget[0].DestBlend = D3D11_BLEND_INV_SRC_ALPHA; bd.RenderTarget[0].BlendOp = D3D11_BLEND_OP_ADD;
    bd.RenderTarget[0].SrcBlendAlpha = D3D11_BLEND_ONE; bd.RenderTarget[0].DestBlendAlpha = D3D11_BLEND_ZERO; bd.RenderTarget[0].BlendOpAlpha = D3D11_BLEND_OP_ADD;
    bd.RenderTarget[0].RenderTargetWriteMask = 0xF;
    ID3D11Device_CreateBlendState(dev, &bd, &bs);
    memset(&bd, 0, sizeof bd);
    ID3D11BlendState_GetDesc(bs, &bd);
    printf("blend state back: enable %d, src %d, dst %d; target 3: src %d\n", bd.RenderTarget[0].BlendEnable, (int)bd.RenderTarget[0].SrcBlend, (int)bd.RenderTarget[0].DestBlend, (int)bd.RenderTarget[3].SrcBlend);
    ID3D11BlendState_Release(bs);
    memset(&rd, 0, sizeof rd);
    rd.FillMode = D3D11_FILL_SOLID; rd.CullMode = D3D11_CULL_BACK; rd.DepthClipEnable = TRUE;
    printf("rasterizer state: 0x%lx\n", (unsigned long)ID3D11Device_CreateRasterizerState(dev, &rd, &rs));
    ID3D11RasterizerState_Release(rs);
    rd.FillMode = (D3D11_FILL_MODE)7;
    printf("a bad fill mode: 0x%lx\n", (unsigned long)ID3D11Device_CreateRasterizerState(dev, &rd, &rs));
    memset(&smd, 0, sizeof smd);
    smd.Filter = D3D11_FILTER_MIN_MAG_MIP_LINEAR; smd.AddressU = smd.AddressV = smd.AddressW = D3D11_TEXTURE_ADDRESS_WRAP; smd.MaxLOD = D3D11_FLOAT32_MAX; smd.ComparisonFunc = D3D11_COMPARISON_NEVER;
    ID3D11Device_CreateSamplerState(dev, &smd, &ss);
    ID3D11SamplerState_GetDesc(ss, &smd);
    printf("sampler back: filter 0x%x, address %d\n", (int)smd.Filter, (int)smd.AddressU);
    ID3D11SamplerState_Release(ss);

    qd.Query = D3D11_QUERY_EVENT; qd.MiscFlags = 0;
    ID3D11Device_CreateQuery(dev, &qd, &q);
    ID3D11DeviceContext_End(ctx, (ID3D11Asynchronous *)q);
    while ((hr = ID3D11DeviceContext_GetData(ctx, (ID3D11Asynchronous *)q, &done, sizeof done, 0)) == S_FALSE && spins < 10000000) spins++;
    printf("event query: 0x%lx, %d\n", (unsigned long)hr, done);
    printf("its data size: %u\n", ID3D11Asynchronous_GetDataSize((ID3D11Asynchronous *)q));
    ID3D11Query_Release(q);
    {
        ID3D11Texture2D *t;
        D3D11_TEXTURE2D_DESC td;
        memset(&td, 0, sizeof td);
        td.Width = 0; td.Height = 4; td.MipLevels = 1; td.ArraySize = 1; td.Format = DXGI_FORMAT_R8G8B8A8_UNORM; td.SampleDesc.Count = 1;
        printf("a 0-wide texture: 0x%lx\n", (unsigned long)ID3D11Device_CreateTexture2D(dev, &td, NULL, &t));
        td.Width = 4;
        td.BindFlags = D3D11_BIND_DEPTH_STENCIL;
        printf("a colour format as depth: 0x%lx\n", (unsigned long)ID3D11Device_CreateTexture2D(dev, &td, NULL, &t));
        td.BindFlags = 0;
        td.MipLevels = 0;
        td.Width = 32; td.Height = 8;
        ID3D11Device_CreateTexture2D(dev, &td, NULL, &t);
        ID3D11Texture2D_GetDesc(t, &td);
        printf("a full chain of 32x8: %u mips\n", td.MipLevels);
        ID3D11Texture2D_Release(t);
    }
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
    IDXGIFactory1 *factory = NULL;
    IDXGISwapChain *swap = NULL;
    DXGI_SWAP_CHAIN_DESC sd;
    D3D_FEATURE_LEVEL want[2] = {D3D_FEATURE_LEVEL_10_1, D3D_FEATURE_LEVEL_10_0}, got;
    D3D_DRIVER_TYPE driver = D3D_DRIVER_TYPE_WARP;
    ID3D11Texture2D *st;
    D3D11_MAPPED_SUBRESOURCE mp;
    HWND hwnd;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    if (argc > 1 && strcmp(argv[1], "gpu") == 0)
        driver = D3D_DRIVER_TYPE_HARDWARE;
    hwnd = CreateWindowExA(0, "STATIC", "dx1", WS_POPUP, 0, 0, W, H, NULL, NULL, NULL, NULL);
    printf("CreateDXGIFactory1: %s\n", SUCCEEDED(CreateDXGIFactory1(&IID_IDXGIFactory1, (void **)&factory)) ? "ok" : "FAILED");
    memset(&sd, 0, sizeof sd);
    sd.BufferDesc.Width = W;
    sd.BufferDesc.Height = H;
    sd.BufferDesc.Format = DXGI_FORMAT_R8G8B8A8_UNORM;
    sd.SampleDesc.Count = 1;
    sd.BufferUsage = DXGI_USAGE_RENDER_TARGET_OUTPUT;
    sd.BufferCount = 1;
    sd.OutputWindow = hwnd;
    sd.Windowed = TRUE;
    sd.SwapEffect = DXGI_SWAP_EFFECT_DISCARD;
    hr = D3D11CreateDeviceAndSwapChain(NULL, driver, NULL, 0, want, 2, D3D11_SDK_VERSION, &sd, &swap, &dev, &got, &ctx);
    printf("device and swap chain: %s, feature level 0x%x\n", SUCCEEDED(hr) ? "created" : "FAILED", (int)got);
    if (FAILED(hr))
        return 1;
    IDXGISwapChain_GetBuffer(swap, 0, &IID_ID3D11Texture2D, (void **)&back);
    {
        ID3D11RenderTargetView *v = rtv_of(back);
        float c[4] = {0.05f, 0.05f, 0.05f, 1};
        ID3D11DeviceContext_ClearRenderTargetView(ctx, v, c);
        ID3D11RenderTargetView_Release(v);
    }
    panels();
    printf("Present: 0x%lx\n", (unsigned long)IDXGISwapChain_Present(swap, 0, 0));

    st = texture(W, H, 1, DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ, NULL, 0);
    ID3D11DeviceContext_CopyResource(ctx, (ID3D11Resource *)st, (ID3D11Resource *)back);
    ID3D11DeviceContext_Map(ctx, (ID3D11Resource *)st, 0, D3D11_MAP_READ, 0, &mp);
    for (y = 0; y < H; y++)
        for (x = 0; x < W; x++) {
            unsigned char *p = (unsigned char *)mp.pData + y * mp.RowPitch + x * 4, *q = img + (y * W + x) * 3;
            q[0] = p[0]; q[1] = p[1]; q[2] = p[2];
        }
    ID3D11DeviceContext_Unmap(ctx, (ID3D11Resource *)st, 0);
    ID3D11Texture2D_Release(st);
    checks(factory);
    ID3D11Texture2D_Release(back);
    ID3D11DeviceContext_ClearState(ctx);
    ID3D11DeviceContext_Release(ctx);
    IDXGISwapChain_Release(swap);
    ID3D11Device_Release(dev);
    IDXGIFactory1_Release(factory);

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
    strcpy(slash ? slash + 1 : path, "dx1.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
