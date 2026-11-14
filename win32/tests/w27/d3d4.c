/* G5e3 test: Direct3D 9's ps_1_1, ps_1_3 and ps_1_4 pixel shaders,
 * compared pixel by pixel with what this machine's GPU draws (recorded on
 * Windows). The shaders are d3d4.psa, assembled by psasm.py.
 *
 * Draws 384x128 into the back buffer, in twelve 64x64 panels, each a
 * pretransformed quad with four sets of texture coordinates:
 *
 *   0  tex, mul                        6  texreg2ar, texreg2gb
 *   1  lrp, mad_d2, co-issue           7  texm3x2pad, texm3x2tex
 *   2  dp3_sat with _bx2 (DOT3)        8  ps_1_3: texdp3tex, dp4, cmp
 *   3  cnd on r0.a                     9  ps_1_4: texld, texcrd, phase,
 *   4  texkill, texcoord                  a dependent texld
 *   5  texbem (the stage's bump       10  ps_1_4: bem
 *      matrix)                        11  ps_1_4: texld _dw, cmp_x2, mov_d2
 *
 *   d3d4                compare with d3d4.ref next to the program
 *   d3d4 record FILE    write what this machine draws as FILE (a PPM)
 *   d3d4 stats          as d3d4, with each panel's differences counted
 *
 * Tolerance as d3d1: in each panel at most 2% of pixels (81) may differ by
 * more than 8 in any channel.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "build/d3d4_shaders.h"

#define W 384
#define H 128
#define PANELS 12

static unsigned char ref[W * H * 3];
static int stats;
static IDirect3DDevice9 *dev;

typedef struct { float x, y, z, rhw; DWORD c, s; float t[4][4]; } VT;
#define FVF_VT (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_SPECULAR | D3DFVF_TEX4 | D3DFVF_TEXCOORDSIZE4(0) | D3DFVF_TEXCOORDSIZE4(1) | D3DFVF_TEXCOORDSIZE4(2) | D3DFVF_TEXCOORDSIZE4(3))

static int px, py;

/* Each corner's texture coordinates: set s at corner (cx, cy), 0 or 1. */
typedef void (*Coords)(int s, int cx, int cy, float out[4]);

static void uv01(int s, int cx, int cy, float o[4])
{
    (void)s;
    o[0] = (float)cx; o[1] = (float)cy; o[2] = 0; o[3] = 1;
}

/* Draw the panel's quad with shader `code`, corner colours c0 (top) and
 * c1 (bottom), specular cs. */
static void quad(const DWORD *code, Coords tc, DWORD c0, DWORD c1, DWORD cs)
{
    VT q[4];
    IDirect3DPixelShader9 *ps = NULL;
    HRESULT hr;
    int i, s;
    for (i = 0; i < 4; i++) {
        int cx = i & 1, cy = i >> 1;
        q[i].x = px + 2 + cx * 60.0f - 0.5f;
        q[i].y = py + 2 + cy * 60.0f - 0.5f;
        q[i].z = 0.5f;
        q[i].rhw = 1;
        q[i].c = cy ? c1 : c0;
        q[i].s = cs;
        for (s = 0; s < 4; s++)
            tc(s, cx, cy, q[i].t[s]);
    }
    hr = IDirect3DDevice9_CreatePixelShader(dev, code, &ps);
    if (FAILED(hr)) {
        printf("panel at %d,%d: CreatePixelShader 0x%lx\n", px, py, (unsigned long)hr);
        return;
    }
    IDirect3DDevice9_SetPixelShader(dev, ps);
    IDirect3DDevice9_SetFVF(dev, FVF_VT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof(VT));
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    IDirect3DPixelShader9_Release(ps);
}

static void panel(int n)
{
    D3DVIEWPORT9 v;
    int i;
    px = (n % 6) * 64;
    py = (n / 6) * 64;
    v.X = px; v.Y = py; v.Width = 64; v.Height = 64; v.MinZ = 0; v.MaxZ = 1;
    IDirect3DDevice9_SetViewport(dev, &v);
    for (i = 0; i < 4; i++) {
        IDirect3DDevice9_SetTexture(dev, i, NULL);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_ADDRESSU, D3DTADDRESS_WRAP);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_ADDRESSV, D3DTADDRESS_WRAP);
    }
}

/* -----| textures |----- */

static IDirect3DTexture9 *make_texture(int size, DWORD (*f)(int, int, int))
{
    IDirect3DTexture9 *t;
    D3DLOCKED_RECT r;
    int x, y;
    IDirect3DDevice9_CreateTexture(dev, size, size, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL);
    IDirect3DTexture9_LockRect(t, 0, &r, NULL, 0);
    for (y = 0; y < size; y++)
        for (x = 0; x < size; x++)
            ((DWORD *)((char *)r.pBits + y * r.Pitch))[x] = f(x, y, size);
    IDirect3DTexture9_UnlockRect(t, 0);
    return t;
}

static DWORD checker(int x, int y, int n)
{
    int s = n / 8;
    return ((x / s + y / s) & 1) ? 0x30E8E0D0 : 0xD0305080;
}

static DWORD waves(int x, int y, int n)
{
    float fx = (float)x / n, fy = (float)y / n;
    int r = (int)(128 + 100 * sin(fx * 6.2832 * 2)), g = (int)(128 + 100 * cos(fy * 6.2832 * 2));
    return (DWORD)(255 - (x + y) * 255 / (2 * n)) << 24 | (DWORD)r << 16 | (DWORD)g << 8 | (DWORD)(64 + x * 128 / n);
}

/* A bump: normals as colours. */
static DWORD bumps(int x, int y, int n)
{
    float fx = (x + 0.5f) / n * 2 - 1, fy = (y + 0.5f) / n * 2 - 1, fz = 1 - fx * fx - fy * fy, l;
    if (fz < 0.1f) fz = 0.1f;
    l = (float)sqrt(fx * fx + fy * fy + fz * fz);
    return 0xFF000000 | (DWORD)((fx / l * 0.5f + 0.5f) * 255) << 16 | (DWORD)((fy / l * 0.5f + 0.5f) * 255) << 8 | (DWORD)((fz / l * 0.5f + 0.5f) * 255);
}

static DWORD stripes(int x, int y, int n)
{
    (void)y;
    return 0xFF000000 | (DWORD)(x * 255 / (n - 1)) << 16 | (DWORD)(((x / 4) & 1) ? 0xC0 : 0x40) << 8 | (DWORD)(255 - x * 255 / (n - 1));
}

static IDirect3DTexture9 *t_check, *t_waves, *t_bumps, *t_stripes;

static void tex(int s, IDirect3DTexture9 *t)
{
    IDirect3DDevice9_SetTexture(dev, s, (IDirect3DBaseTexture9 *)t);
}

/* -----| per-panel texture coordinates |----- */

static void tc_kill(int s, int cx, int cy, float o[4])
{
    o[0] = cx - 0.3f; o[1] = cy - 0.6f; o[2] = 0; o[3] = 1;
    if (s == 1) { o[0] = 0.2f + cx * 0.6f; o[1] = 0.9f - cy * 0.7f; o[2] = 0.5f; }
}

static void tc_m3x2(int s, int cx, int cy, float o[4])
{
    /* Set 0 samples the bump; sets 1 and 2 are the rows of a 3x2 matrix. */
    if (s == 0) { uv01(s, cx, cy, o); return; }
    o[0] = s == 1 ? 0.9f + cx * 0.3f : 0.1f;
    o[1] = s == 1 ? 0.2f : 0.8f + cy * 0.4f;
    o[2] = 0.5f;
    o[3] = 1;
}

static void tc_dp3(int s, int cx, int cy, float o[4])
{
    if (s == 0) { uv01(s, cx, cy, o); return; }
    o[0] = 0.4f + cx * 0.5f; o[1] = 0.6f; o[2] = 0.3f + cy * 0.4f; o[3] = 1;
}

static void tc_proj(int s, int cx, int cy, float o[4])
{
    /* Set 0 projective: (u w, v w, 0, w) with w changing across. */
    float w = 1 + cx * 1.5f + cy * 0.5f;
    if (s == 0) { o[0] = cx * 1.2f * w; o[1] = cy * 1.2f * w; o[2] = 0; o[3] = w; return; }
    o[0] = 0.2f + cx * 0.7f; o[1] = 0.8f - cy * 0.5f; o[2] = 0.5f + cx * 0.3f; o[3] = 1;
}

static void tc_offset(int s, int cx, int cy, float o[4])
{
    o[0] = cx * 1.0f + (s == 1 ? 0.13f : 0); o[1] = cy * 1.0f; o[2] = 0; o[3] = 1;
}

static void bump_matrix(int stage, float m00, float m01, float m10, float m11)
{
    DWORD v;
    memcpy(&v, &m00, 4); IDirect3DDevice9_SetTextureStageState(dev, stage, D3DTSS_BUMPENVMAT00, v);
    memcpy(&v, &m01, 4); IDirect3DDevice9_SetTextureStageState(dev, stage, D3DTSS_BUMPENVMAT01, v);
    memcpy(&v, &m10, 4); IDirect3DDevice9_SetTextureStageState(dev, stage, D3DTSS_BUMPENVMAT10, v);
    memcpy(&v, &m11, 4); IDirect3DDevice9_SetTextureStageState(dev, stage, D3DTSS_BUMPENVMAT11, v);
}

static void draw(void)
{
    panel(0);
    tex(0, t_check);
    quad(ps11_mod, uv01, 0xFFFFFFFF, 0xFFFF8040, 0);

    panel(1);
    tex(0, t_check);
    tex(1, t_waves);
    quad(ps11_lrp, uv01, 0xFF8080FF, 0xFFFFFF80, 0xFF202040);

    panel(2);
    tex(0, t_bumps);
    quad(ps11_dot3, uv01, 0xFFC0A0E0, 0xFF60A0E0, 0);

    panel(3);
    tex(0, t_check);
    tex(1, t_stripes);
    quad(ps11_cnd, uv01, 0xFF40C040, 0xFFC04040, 0);

    panel(4);
    quad(ps11_kill, tc_kill, 0xFFFFFFFF, 0xFF8080FF, 0);

    panel(5);
    tex(0, t_waves);
    tex(1, t_check);
    bump_matrix(1, 0.15f, 0.05f, -0.05f, 0.15f);
    quad(ps11_bem, uv01, 0xFFFFFFFF, 0xFFC0FFC0, 0);

    panel(6);
    tex(0, t_waves);
    tex(1, t_check);
    tex(2, t_stripes);
    quad(ps11_reg2, uv01, 0, 0, 0);

    panel(7);
    tex(0, t_bumps);
    tex(2, t_check);
    quad(ps11_m3x2, tc_m3x2, 0, 0, 0);

    panel(8);
    tex(0, t_waves);
    tex(1, t_stripes);
    quad(ps13_dp3tex, tc_dp3, 0xFF4080C0, 0xFFC08040, 0xFF806040);

    panel(9);
    tex(0, t_bumps);
    tex(2, t_check);
    quad(ps14_dep, tc_offset, 0xFFFFFFFF, 0xFFFFC080, 0);

    panel(10);
    tex(0, t_waves);
    tex(1, t_check);
    bump_matrix(1, -0.12f, 0.08f, 0.08f, 0.12f);
    quad(ps14_bem, tc_offset, 0, 0, 0);

    panel(11);
    tex(0, t_stripes);
    quad(ps14_proj, tc_proj, 0xFF808080, 0xFF202020, 0);
}

static int compare(const unsigned char *img)
{
    static const char *names[PANELS] = {"tex, mul", "lrp, mad_d2, co-issue", "dp3 _bx2", "cnd", "texkill, texcoord", "texbem",
                                        "texreg2ar, texreg2gb", "texm3x2", "ps_1_3 texdp3tex, dp4, cmp", "ps_1_4 dependent texld", "ps_1_4 bem", "ps_1_4 _dw, cmp_x2"};
    int n, bad_panels = 0;
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
        if (off <= 81) {
            printf("panel %d (%s): ok\n", n, names[n]);
        } else {
            printf("panel %d (%s): FAIL\n", n, names[n]);
            bad_panels++;
        }
        if (stats)
            printf("  %d pixels differ, %d by more than 8, largest difference %d\n", any, off, worst);
    }
    return bad_panels;
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
    D3DCAPS9 caps;
    D3DLOCKED_RECT r;
    static unsigned char img[W * H * 3];
    HWND hwnd;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    hwnd = CreateWindowExA(0, "STATIC", "d3d4", WS_POPUP, 0, 0, W, H, NULL, NULL, NULL, NULL);
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
    IDirect3DDevice9_GetDeviceCaps(dev, &caps);
    printf("pixel shaders 1.4 or later: %s\n", caps.PixelShaderVersion >= D3DPS_VERSION(1, 4) ? "yes" : "no");
    printf("ps_1_x range at least 1: %s\n", caps.PixelShader1xMaxValue >= 1 ? "yes" : "no");
    t_check = make_texture(32, checker);
    t_waves = make_texture(32, waves);
    t_bumps = make_texture(32, bumps);
    t_stripes = make_texture(32, stripes);
    IDirect3DDevice9_GetRenderTarget(dev, 0, &back);
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xFF101010, 1, 0);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(dev, D3DRS_CULLMODE, D3DCULL_NONE);
    IDirect3DDevice9_BeginScene(dev);
    draw();
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
    strcpy(slash ? slash + 1 : path, "d3d4.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
