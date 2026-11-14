/* G5f2a test: Direct3D 9 cube maps read by pixel shaders (until G5f2a the
 * kernel sampled 2D textures only, and cube maps read white). Compared
 * pixel by pixel with what this machine's GPU draws (recorded on Windows).
 *
 * Draws 384x128 into the back buffer; four of the twelve 64x64 panels:
 *
 *   0  a cube map (one level, linear), every direction around
 *   1  a mipmapped cube map at level 0.6 (texCUBElod)
 *   2  a mipmapped cube map at level 1.5 (texCUBElod, ps_3_0)
 *   3  a cube map rendered into: each face cleared, a quad on +z
 *
 *   d3d5                compare with d3d5.ref next to the program
 *   d3d5 record FILE    write what this machine draws as FILE (a PPM)
 *   d3d5 stats          as d3d5, with each panel's differences counted
 *
 * Tolerance as d3d1: in each panel at most 2% of pixels (81) may differ by
 * more than 8 in any channel.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/d3d5_vs20_main.h"
#include "build/d3d5_ps20_cube.h"
#include "build/d3d5_ps30_lod.h"

#define W 384
#define H 128
#define PANELS 12

static unsigned char ref[W * H * 3];
static int stats;
static IDirect3DDevice9 *dev;

typedef struct { float x, y, z, w, u, v; } VX;

static DWORD *aligned(const BYTE *code, size_t size)
{
    DWORD *a = malloc(size);
    memcpy(a, code, size);
    return a;
}

static IDirect3DVertexShader9 *vshader(const BYTE *code, size_t size)
{
    IDirect3DVertexShader9 *s = NULL;
    DWORD *a = aligned(code, size);
    if (FAILED(IDirect3DDevice9_CreateVertexShader(dev, a, &s)))
        printf("CreateVertexShader: FAILED\n");
    free(a);
    return s;
}

static IDirect3DPixelShader9 *pshader(const BYTE *code, size_t size)
{
    IDirect3DPixelShader9 *s = NULL;
    DWORD *a = aligned(code, size);
    if (FAILED(IDirect3DDevice9_CreatePixelShader(dev, a, &s)))
        printf("CreatePixelShader: FAILED\n");
    free(a);
    return s;
}

static void panel(int n)
{
    D3DVIEWPORT9 v;
    v.X = (n % 6) * 64; v.Y = (n / 6) * 64; v.Width = 64; v.Height = 64; v.MinZ = 0; v.MaxZ = 1;
    IDirect3DDevice9_SetViewport(dev, &v);
}

static void quad(void)
{
    VX q[4] = {{-1, 1, 0.5f, 1, 0, 0}, {1, 1, 0.5f, 1, 1, 0}, {-1, -1, 0.5f, 1, 0, 1}, {1, -1, 0.5f, 1, 1, 1}};
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZW | D3DFVF_TEX1);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof(VX));
}

static DWORD face_color[6] = {0xFFE03C32, 0xFF3CC846, 0xFF3C5AE6, 0xFFE6DC3C, 0xFFDC46DC, 0xFF3CD2DC};

/* A cube map, `size` across with `levels` levels, each face its colour,
 * checkered, darker by level. */
static IDirect3DCubeTexture9 *make_cube(int size, int levels)
{
    IDirect3DCubeTexture9 *t = NULL;
    int f, l, x, y;
    if (FAILED(IDirect3DDevice9_CreateCubeTexture(dev, size, levels, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL))) {
        printf("CreateCubeTexture: FAILED\n");
        return NULL;
    }
    for (f = 0; f < 6; f++)
        for (l = 0; l < levels; l++) {
            D3DLOCKED_RECT r;
            int s = size >> l;
            IDirect3DCubeTexture9_LockRect(t, (D3DCUBEMAP_FACES)f, l, &r, NULL, 0);
            for (y = 0; y < s; y++)
                for (x = 0; x < s; x++) {
                    DWORD c = face_color[f];
                    int dark = ((x * 8 / s + y * 8 / s) & 1) ? 0 : 60, k;
                    DWORD o = 0xFF000000;
                    dark += l * 30;
                    for (k = 0; k < 24; k += 8) {
                        int v = (int)((c >> k) & 255) - dark;
                        o |= (DWORD)(v > 0 ? v : 0) << k;
                    }
                    ((DWORD *)((BYTE *)r.pBits + y * r.Pitch))[x] = o;
                }
            IDirect3DCubeTexture9_UnlockRect(t, (D3DCUBEMAP_FACES)f, l);
        }
    return t;
}

static void filter(DWORD f, DWORD mip)
{
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, f);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, f);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MIPFILTER, mip);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
}

static void panels(IDirect3DSurface9 *back)
{
    IDirect3DVertexShader9 *vs = vshader(vs20_main, sizeof vs20_main);
    IDirect3DPixelShader9 *ps_cube = pshader(ps20_cube, sizeof ps20_cube), *ps_lod = pshader(ps30_lod, sizeof ps30_lod);
    IDirect3DCubeTexture9 *one = make_cube(32, 1), *mips = make_cube(32, 6), *rt = NULL;
    float k[4] = {1.5f, 0, 0, 0};
    int f;

    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_ZENABLE, FALSE);
    IDirect3DDevice9_SetVertexShader(dev, vs);
    IDirect3DDevice9_SetPixelShader(dev, ps_cube);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)one);
    filter(D3DTEXF_LINEAR, D3DTEXF_NONE);
    panel(0);
    quad();
    IDirect3DDevice9_SetPixelShader(dev, ps_lod);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)mips);
    filter(D3DTEXF_LINEAR, D3DTEXF_LINEAR);
    k[0] = 0.6f;
    IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, k, 1);
    panel(1);
    quad();
    k[0] = 1.5f;
    IDirect3DDevice9_SetPixelShaderConstantF(dev, 0, k, 1);
    panel(2);
    quad();

    /* 3: rendered into. */
    if (FAILED(IDirect3DDevice9_CreateCubeTexture(dev, 32, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &rt, NULL)))
        printf("CreateCubeTexture (render target): FAILED\n");
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetPixelShader(dev, ps_cube);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)one);
    filter(D3DTEXF_POINT, D3DTEXF_NONE);
    for (f = 0; f < 6 && rt; f++) {
        IDirect3DSurface9 *s = NULL;
        IDirect3DCubeTexture9_GetCubeMapSurface(rt, (D3DCUBEMAP_FACES)f, 0, &s);
        IDirect3DDevice9_SetRenderTarget(dev, 0, s);
        IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, face_color[(f + 3) % 6], 1, 0);
        if (f == D3DCUBEMAP_FACE_POSITIVE_Z) {
            D3DVIEWPORT9 v = {8, 8, 16, 16, 0, 1};
            IDirect3DDevice9_SetViewport(dev, &v);
            quad();
        }
        IDirect3DSurface9_Release(s);
    }
    IDirect3DDevice9_SetRenderTarget(dev, 0, back);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)rt);
    filter(D3DTEXF_LINEAR, D3DTEXF_NONE);
    panel(3);
    quad();

    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetVertexShader(dev, NULL);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DVertexShader9_Release(vs);
    IDirect3DPixelShader9_Release(ps_cube);
    IDirect3DPixelShader9_Release(ps_lod);
    IDirect3DCubeTexture9_Release(one);
    IDirect3DCubeTexture9_Release(mips);
    if (rt)
        IDirect3DCubeTexture9_Release(rt);
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
                if (d > 8) off++;
                if (d > worst) worst = d;
            }
        printf("panel %d: %s\n", n, off <= 81 ? "ok" : "FAIL");
        if (off > 81) bad++;
        if (stats)
            printf("  %d pixels differ, %d by more than 8, largest difference %d\n", any, off, worst);
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
    IDirect3D9 *d3d;
    D3DPRESENT_PARAMETERS pp;
    IDirect3DSurface9 *back, *sys;
    D3DLOCKED_RECT r;
    static unsigned char img[W * H * 3];
    HWND hwnd;
    HRESULT hr;
    int x, y;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    hwnd = CreateWindowExA(0, "STATIC", "d3d5", WS_POPUP, 0, 0, W, H, NULL, NULL, NULL, NULL);
    d3d = Direct3DCreate9(D3D_SDK_VERSION);
    printf("Direct3D: %s\n", d3d ? "created" : "FAILED");
    if (!d3d)
        return 1;
    memset(&pp, 0, sizeof pp);
    pp.BackBufferWidth = W;
    pp.BackBufferHeight = H;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferCount = 1;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.hDeviceWindow = hwnd;
    pp.Windowed = TRUE;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = IDirect3D9_CreateDevice(d3d, 0, D3DDEVTYPE_HAL, hwnd, D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &dev);
    printf("device: %s\n", SUCCEEDED(hr) ? "created" : "FAILED");
    if (FAILED(hr))
        return 1;
    IDirect3DDevice9_GetRenderTarget(dev, 0, &back);
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xFF101010, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    panels(back);
    IDirect3DDevice9_EndScene(dev);
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, W, H, D3DFMT_X8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);
    printf("GetRenderTargetData: 0x%lx\n", (unsigned long)IDirect3DDevice9_GetRenderTargetData(dev, back, sys));
    IDirect3DSurface9_LockRect(sys, &r, NULL, D3DLOCK_READONLY);
    for (y = 0; y < H; y++)
        for (x = 0; x < W; x++) {
            unsigned char *p = (unsigned char *)r.pBits + y * r.Pitch + x * 4, *q = img + (y * W + x) * 3;
            q[0] = p[2]; q[1] = p[1]; q[2] = p[0];
        }
    IDirect3DSurface9_UnlockRect(sys);
    IDirect3DSurface9_Release(sys);
    IDirect3DSurface9_Release(back);
    IDirect3DDevice9_Release(dev);
    IDirect3D9_Release(d3d);

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
    strcpy(slash ? slash + 1 : path, "d3d5.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    return compare(img);
}
