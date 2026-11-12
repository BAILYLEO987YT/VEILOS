/* G5e1 test: Direct3D 9 vertex shaders (vs_1_1, vs_2_0, vs_2_a) feeding
 * the fixed-function pixel pipeline, compared pixel by pixel with what this
 * machine's GPU draws (recorded on Windows). The shaders are d3d2.hlsl,
 * compiled with fxc by build.sh.
 *
 * Draws 384x128 into the back buffer, in twelve 64x64 panels:
 *
 *   0  vs_1_1: per-vertex lighting with lit
 *   1  vs_2_0: a loop over lights (loop aL, i0, relative addressing) and a
 *      static branch on a boolean constant
 *   2  vs_2_0: a matrix palette indexed per vertex (mova, a0)
 *   3  vs_2_0: a wave (sincos), colours from frac, exp2, log2, pow
 *   4  vs_1_1: fog from the shader (oFog)
 *   5  vs_1_1: point sizes from the shader (oPts)
 *   6  vs_2_0: texture coordinates from the position, sampled by the
 *      fixed-function stages
 *   7  vs_2_a: dynamic branches and a loop that breaks early
 *   8  vs_2_0 with a user clip plane (in clip space)
 *   9  vs_2_0 with three streams: positions, UBYTE4N colours, FLOAT16_2
 *      texture coordinates
 *  10  vs_2_0: normalize, cross, sign, abs, lerp, min, max, step
 *  11  vs_2_0: a branch on a boolean constant, both ways
 *
 * and prints what shader objects, constants and errors give, the same on
 * Windows and on VEILOS.
 *
 *   d3d2                compare with d3d2.ref next to the program
 *   d3d2 record FILE    write what this machine draws as FILE (a PPM)
 *   d3d2 stats          as d3d2, with each panel's differences counted
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

#include "build/d3d2_vs11_lit.h"
#include "build/d3d2_vs20_loop.h"
#include "build/d3d2_vs20_skin.h"
#include "build/d3d2_vs20_wave.h"
#include "build/d3d2_vs11_fog.h"
#include "build/d3d2_vs11_points.h"
#include "build/d3d2_vs20_texgen.h"
#include "build/d3d2_vs2a_dyn.h"
#include "build/d3d2_vs20_plain.h"
#include "build/d3d2_vs20_misc.h"
#include "build/d3d2_vs20_call.h"

#define W 384
#define H 128
#define PANELS 12

static unsigned char ref[W * H * 3];
static int stats;
static IDirect3DDevice9 *dev;

typedef struct { float m[16]; } M;

static M identity(void)
{
    M r;
    memset(&r, 0, sizeof r);
    r.m[0] = r.m[5] = r.m[10] = r.m[15] = 1;
    return r;
}

static M mul(M a, M b)
{
    M r;
    int i, j, k;
    for (i = 0; i < 4; i++)
        for (j = 0; j < 4; j++) {
            float s = 0;
            for (k = 0; k < 4; k++) s += a.m[i * 4 + k] * b.m[k * 4 + j];
            r.m[i * 4 + j] = s;
        }
    return r;
}

static M perspective(float fov, float zn, float zf)
{
    M r;
    float h = 1.0f / (float)tan(fov / 2);
    memset(&r, 0, sizeof r);
    r.m[0] = h; r.m[5] = h;
    r.m[10] = zf / (zf - zn); r.m[11] = 1;
    r.m[14] = -zn * zf / (zf - zn);
    return r;
}

static M translate(float x, float y, float z)
{
    M r = identity();
    r.m[12] = x; r.m[13] = y; r.m[14] = z;
    return r;
}

static M rot_x(float a)
{
    M r = identity();
    r.m[5] = (float)cos(a); r.m[6] = (float)sin(a);
    r.m[9] = -(float)sin(a); r.m[10] = (float)cos(a);
    return r;
}

static void rs(D3DRENDERSTATETYPE s, DWORD v) { IDirect3DDevice9_SetRenderState(dev, s, v); }
static DWORD fbits(float f) { DWORD d; memcpy(&d, &f, 4); return d; }

static void vconst(int reg, float x, float y, float z, float w)
{
    float v[4];
    v[0] = x; v[1] = y; v[2] = z; v[3] = w;
    IDirect3DDevice9_SetVertexShaderConstantF(dev, reg, v, 1);
}

/* The world and world-view-projection matrices into c4-c7 and c0-c3. */
static void matrices(M world)
{
    M wvp = mul(world, perspective(1.0f, 0.5f, 50.0f));
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 0, wvp.m, 4);
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 4, world.m, 4);
}

static IDirect3DVertexShader9 *shader(const BYTE *code, size_t size)
{
    IDirect3DVertexShader9 *s = NULL;
    DWORD *aligned = malloc(size);
    memcpy(aligned, code, size);
    if (FAILED(IDirect3DDevice9_CreateVertexShader(dev, aligned, &s)))
        printf("CreateVertexShader: FAILED\n");
    free(aligned);
    return s;
}
#define SHADER(name) shader(name, sizeof name)

static void use(IDirect3DVertexShader9 *s)
{
    IDirect3DDevice9_SetVertexShader(dev, s);
}

static int px, py;
static void panel(int n)
{
    D3DVIEWPORT9 v;
    px = (n % 6) * 64;
    py = (n / 6) * 64;
    v.X = px; v.Y = py; v.Width = 64; v.Height = 64; v.MinZ = 0; v.MaxZ = 1;
    IDirect3DDevice9_SetViewport(dev, &v);
    rs(D3DRS_CULLMODE, D3DCULL_NONE);
    rs(D3DRS_LIGHTING, FALSE);
    rs(D3DRS_FOGENABLE, FALSE);
    rs(D3DRS_SPECULARENABLE, FALSE);
    rs(D3DRS_CLIPPLANEENABLE, 0);
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG2);
}

/* -----| geometry |----- */

typedef struct { float x, y, z, nx, ny, nz; } VN;

static VN sphere_v[13 * 17];
static WORD sphere_i[12 * 16 * 6];
static int sphere_n;

static void make_sphere(void)
{
    int i, j;
    for (i = 0; i <= 12; i++)
        for (j = 0; j <= 16; j++) {
            float a = 3.14159265f * i / 12, b = 2 * 3.14159265f * j / 16;
            VN *p = &sphere_v[i * 17 + j];
            p->nx = (float)(sin(a) * cos(b)); p->ny = (float)cos(a); p->nz = (float)(sin(a) * sin(b));
            p->x = p->nx; p->y = p->ny; p->z = p->nz;
        }
    for (i = 0; i < 12; i++)
        for (j = 0; j < 16; j++) {
            WORD a = (WORD)(i * 17 + j), b = (WORD)(a + 17);
            sphere_i[sphere_n++] = a; sphere_i[sphere_n++] = a + 1; sphere_i[sphere_n++] = b;
            sphere_i[sphere_n++] = a + 1; sphere_i[sphere_n++] = b + 1; sphere_i[sphere_n++] = b;
        }
}

static void sphere(M world)
{
    matrices(world);
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_NORMAL);
    IDirect3DDevice9_DrawIndexedPrimitiveUP(dev, D3DPT_TRIANGLELIST, 0, 13 * 17, sphere_n / 3, sphere_i, D3DFMT_INDEX16, sphere_v, sizeof(VN));
}

typedef struct { float x, y, z; DWORD c; float u, v; } VCT;

/* -----| the panels |----- */

static void lit(void)
{
    panel(0);
    use(SHADER(vs11_lit));
    vconst(8, 0.5f, -0.6f, 0.62f, 0);
    vconst(9, 0.8f, 0.7f, 0.5f, 1);
    vconst(10, 0.1f, 0.1f, 0.2f, 1);
    vconst(12, 0, 0, 0, 1);
    sphere(translate(0, 0, 3));
}

static void loop(void)
{
    int n[4] = {3, 0, 1, 0};
    BOOL b = TRUE;
    panel(1);
    use(SHADER(vs20_loop));
    vconst(10, 0.05f, 0.05f, 0.05f, 1);
    vconst(14, 3, 0, 0, 0);
    vconst(16, 0.7f, 0.0f, -0.7f, 0); vconst(20, 0.8f, 0.2f, 0.1f, 1);
    vconst(17, -0.7f, 0.0f, -0.7f, 0); vconst(21, 0.1f, 0.7f, 0.2f, 1);
    vconst(18, 0.0f, 0.8f, -0.6f, 0); vconst(22, 0.1f, 0.2f, 0.8f, 1);
    vconst(19, 0.0f, -0.8f, -0.6f, 0); vconst(23, 0.9f, 0.9f, 0.9f, 1);
    IDirect3DDevice9_SetVertexShaderConstantI(dev, 0, n, 1);
    IDirect3DDevice9_SetVertexShaderConstantB(dev, 0, &b, 1);
    sphere(translate(0, 0, 3));
}

static void skin(void)
{
    static const D3DVERTEXELEMENT9 el[] = {
        {0, 0, D3DDECLTYPE_FLOAT3, 0, D3DDECLUSAGE_POSITION, 0},
        {0, 12, D3DDECLTYPE_FLOAT1, 0, D3DDECLUSAGE_BLENDWEIGHT, 0},
        {0, 16, D3DDECLTYPE_UBYTE4N, 0, D3DDECLUSAGE_BLENDINDICES, 0},
        {0, 20, D3DDECLTYPE_D3DCOLOR, 0, D3DDECLUSAGE_COLOR, 0},
        D3DDECL_END()};
    struct { float x, y, z, w; DWORD idx, c; } v[2 * 9];
    IDirect3DVertexDeclaration9 *d;
    int i, b;
    panel(2);
    use(SHADER(vs20_skin));
    /* Four bones: each a turn about z and a move. */
    for (b = 0; b < 4; b++) {
        float a = 0.35f * b, m[12];
        memset(m, 0, sizeof m);
        m[0] = (float)cos(a); m[1] = (float)sin(a);
        m[3] = -(float)sin(a); m[4] = (float)cos(a);
        m[8] = 1;
        m[9] = 0.3f * b; m[10] = 0.1f * b;
        IDirect3DDevice9_SetVertexShaderConstantF(dev, 32 + b * 3, m, 3);
    }
    for (i = 0; i < 9; i++) {
        float y = -1.2f + i * 0.3f;
        int bone = i / 3;
        DWORD idx = (DWORD)bone | (DWORD)(bone + 1 > 3 ? 3 : bone + 1) << 8;
        v[2 * i].x = -0.4f; v[2 * i + 1].x = 0.4f;
        v[2 * i].y = v[2 * i + 1].y = y;
        v[2 * i].z = v[2 * i + 1].z = 0;
        v[2 * i].w = v[2 * i + 1].w = 1 - (i % 3) / 3.0f;
        v[2 * i].idx = v[2 * i + 1].idx = idx;
        v[2 * i].c = 0xFF000000 | (DWORD)(i * 28) << 16 | 0x8000 | (DWORD)(255 - i * 28);
        v[2 * i + 1].c = 0xFFFFFFFF;
    }
    IDirect3DDevice9_CreateVertexDeclaration(dev, el, &d);
    IDirect3DDevice9_SetVertexDeclaration(dev, d);
    matrices(translate(-0.2f, 0, 3));
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 16, v, sizeof v[0]);
    IDirect3DVertexDeclaration9_Release(d);
}

/* A grid of n x n quads over -1..1 in x and z, at height y. */
static void grid(int n, float y, DWORD c)
{
    VCT *v = malloc(sizeof(VCT) * (n + 1) * 2);
    int i, j;
    for (j = 0; j < n; j++) {
        for (i = 0; i <= n; i++) {
            int k;
            for (k = 0; k < 2; k++) {
                VCT *p = &v[i * 2 + k];
                p->x = -1 + 2.0f * i / n;
                p->z = -1 + 2.0f * (j + k) / n;
                p->y = y;
                p->c = c;
                p->u = p->x; p->v = p->z;
            }
        }
        IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, n * 2, v, sizeof(VCT));
    }
    free(v);
}

static void wave(void)
{
    panel(3);
    use(SHADER(vs20_wave));
    vconst(11, 5.3f, 0.7f, 0, 0);
    matrices(mul(rot_x(0.5f), translate(0, -0.1f, 3.2f)));
    grid(12, 0, 0xFFFFFFFF);
}

static void fog(void)
{
    VCT v[4];
    int i;
    panel(4);
    use(SHADER(vs11_fog));
    rs(D3DRS_FOGENABLE, TRUE);
    rs(D3DRS_FOGCOLOR, 0x00704020);
    rs(D3DRS_FOGVERTEXMODE, D3DFOG_NONE);
    rs(D3DRS_FOGTABLEMODE, D3DFOG_NONE);
    vconst(11, 0, 0, 1.5f, 10);
    for (i = 0; i < 4; i++) { v[i].c = 0xFFA0E0FF; v[i].u = v[i].v = 0; }
    v[0].x = -1.5f; v[0].y = -0.6f; v[0].z = 0.8f;
    v[1].x = 1.5f; v[1].y = -0.6f; v[1].z = 0.8f;
    v[2].x = -1.5f; v[2].y = 1.4f; v[2].z = 16;
    v[3].x = 1.5f; v[3].y = 1.4f; v[3].z = 16;
    matrices(identity());
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(VCT));
}

static void points(void)
{
    VCT v[12];
    int i;
    panel(5);
    use(SHADER(vs11_points));
    rs(D3DRS_ZENABLE, FALSE);
    for (i = 0; i < 12; i++) {
        v[i].x = -1 + (i % 4) * 0.65f + 0.05f;
        v[i].y = 0.8f - (i / 4) * 0.75f + 0.03f;
        v[i].z = 0;
        v[i].c = (DWORD)(i * 21) << 24 | 0xFFFF80 - (DWORD)(i * 0x1500);
        v[i].u = v[i].v = 0;
    }
    matrices(translate(0, 0, 2.5f));
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_POINTLIST, 12, v, sizeof(VCT));
    rs(D3DRS_ZENABLE, TRUE);
}

static DWORD checker_px(int x, int y)
{
    return ((x / 4 + y / 4) & 1) ? 0xFFE8E0D0 : 0xFF305080;
}

static IDirect3DTexture9 *checker(void)
{
    IDirect3DTexture9 *t;
    D3DLOCKED_RECT r;
    int x, y;
    IDirect3DDevice9_CreateTexture(dev, 32, 32, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL);
    IDirect3DTexture9_LockRect(t, 0, &r, NULL, 0);
    for (y = 0; y < 32; y++)
        for (x = 0; x < 32; x++)
            ((DWORD *)((char *)r.pBits + y * r.Pitch))[x] = checker_px(x, y);
    IDirect3DTexture9_UnlockRect(t, 0);
    return t;
}

static void texgen(IDirect3DTexture9 *t)
{
    panel(6);
    use(SHADER(vs20_texgen));
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
    vconst(11, 0.45f, 0.35f, 0.1f, 0.25f);
    matrices(mul(rot_x(0.6f), translate(0, -0.2f, 2.8f)));
    grid(4, 0, 0xFFFFFFFF);
}

static void dyn(void)
{
    panel(7);
    use(SHADER(vs2a_dyn));
    sphere(translate(0, 0, 3));
}

static void clip(void)
{
    VCT v[3];
    float plane[4] = {1, 0.5f, 0, -0.1f};
    int i;
    panel(8);
    use(SHADER(vs20_plain));
    for (i = 0; i < 3; i++) { v[i].z = 0; v[i].u = v[i].v = 0; }
    v[0].x = -1; v[0].y = -0.9f; v[0].c = 0xFFFF2020;
    v[1].x = 0.1f; v[1].y = 1.0f; v[1].c = 0xFF20FF20;
    v[2].x = 1.1f; v[2].y = -0.7f; v[2].c = 0xFF2020FF;
    IDirect3DDevice9_SetClipPlane(dev, 0, plane);
    rs(D3DRS_CLIPPLANEENABLE, 1);
    matrices(translate(0, 0, 2.5f));
    IDirect3DDevice9_SetFVF(dev, D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, v, sizeof(VCT));
}

static WORD f16(float f)
{
    DWORD b;
    int e;
    memcpy(&b, &f, 4);
    e = (int)((b >> 23) & 255) - 127 + 15;
    if (e <= 0) return (WORD)(b >> 16 & 0x8000);
    return (WORD)((b >> 16 & 0x8000) | e << 10 | (b >> 13 & 0x3FF));
}

static void streams(IDirect3DTexture9 *t)
{
    static const D3DVERTEXELEMENT9 el[] = {
        {0, 0, D3DDECLTYPE_FLOAT3, 0, D3DDECLUSAGE_POSITION, 0},
        {1, 0, D3DDECLTYPE_UBYTE4N, 0, D3DDECLUSAGE_COLOR, 0},
        {2, 0, D3DDECLTYPE_FLOAT16_2, 0, D3DDECLUSAGE_TEXCOORD, 0},
        D3DDECL_END()};
    IDirect3DVertexDeclaration9 *d;
    IDirect3DVertexBuffer9 *vb[3];
    float *p;
    BYTE *c;
    WORD *uv;
    int i;
    panel(9);
    use(SHADER(vs20_plain));
    IDirect3DDevice9_CreateVertexBuffer(dev, 4 * 12, 0, 0, D3DPOOL_MANAGED, &vb[0], NULL);
    IDirect3DDevice9_CreateVertexBuffer(dev, 4 * 4, 0, 0, D3DPOOL_MANAGED, &vb[1], NULL);
    IDirect3DDevice9_CreateVertexBuffer(dev, 4 * 4, 0, 0, D3DPOOL_MANAGED, &vb[2], NULL);
    IDirect3DVertexBuffer9_Lock(vb[0], 0, 0, (void **)&p, 0);
    IDirect3DVertexBuffer9_Lock(vb[1], 0, 0, (void **)&c, 0);
    IDirect3DVertexBuffer9_Lock(vb[2], 0, 0, (void **)&uv, 0);
    for (i = 0; i < 4; i++) {
        p[i * 3] = (i & 1) ? 0.9f : -0.9f;
        p[i * 3 + 1] = (i & 2) ? -0.9f : 0.9f;
        p[i * 3 + 2] = (i & 1) ? 0.3f : -0.3f;
        /* UBYTE4N is x, y, z, w in byte order: red, green, blue, alpha. */
        c[i * 4] = (BYTE)(255 - i * 60); c[i * 4 + 1] = (BYTE)(80 + i * 50); c[i * 4 + 2] = (BYTE)(i * 70); c[i * 4 + 3] = 255;
        uv[i * 2] = f16((i & 1) ? 2.0f : 0.0f);
        uv[i * 2 + 1] = f16((i & 2) ? 1.5f : 0.0f);
    }
    for (i = 0; i < 3; i++) IDirect3DVertexBuffer9_Unlock(vb[i]);
    IDirect3DDevice9_CreateVertexDeclaration(dev, el, &d);
    IDirect3DDevice9_SetVertexDeclaration(dev, d);
    IDirect3DDevice9_SetStreamSource(dev, 0, vb[0], 0, 12);
    IDirect3DDevice9_SetStreamSource(dev, 1, vb[1], 0, 4);
    IDirect3DDevice9_SetStreamSource(dev, 2, vb[2], 0, 4);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
    IDirect3DDevice9_SetTextureStageState(dev, 0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    matrices(translate(0, 0, 2.5f));
    IDirect3DDevice9_DrawPrimitive(dev, D3DPT_TRIANGLESTRIP, 0, 2);
    for (i = 0; i < 3; i++) {
        IDirect3DDevice9_SetStreamSource(dev, i, NULL, 0, 0);
        IDirect3DVertexBuffer9_Release(vb[i]);
    }
    IDirect3DDevice9_SetVertexDeclaration(dev, NULL);
    IDirect3DVertexDeclaration9_Release(d);
}

static void misc(void)
{
    panel(10);
    use(SHADER(vs20_misc));
    sphere(translate(0, 0, 3));
}

static void call(void)
{
    BOOL b;
    panel(11);
    use(SHADER(vs20_call));
    b = TRUE;
    IDirect3DDevice9_SetVertexShaderConstantB(dev, 1, &b, 1);
    sphere(mul(mul(identity(), translate(-0.55f, 0, 0)), translate(0, 0, 3.4f)));
    b = FALSE;
    IDirect3DDevice9_SetVertexShaderConstantB(dev, 1, &b, 1);
    sphere(mul(mul(identity(), translate(0.55f, 0, 0)), translate(0, 0, 3.4f)));
}

/* -----| checks that print |----- */

static void checks(void)
{
    D3DCAPS9 caps;
    IDirect3DVertexShader9 *s = SHADER(vs20_plain), *got = NULL;
    DWORD bad[3] = {0xFFFE0200, 0x12345678, 0x0000FFFF}, size = 0;
    float f[8];
    int i[4];
    BOOL b[2];
    HRESULT hr;

    IDirect3DDevice9_GetDeviceCaps(dev, &caps);
    printf("vertex shaders 2.0 or later: %s\n", caps.VertexShaderVersion >= D3DVS_VERSION(2, 0) ? "yes" : "no");
    printf("vertex shader constants >= 256: %s\n", caps.MaxVertexShaderConst >= 256 ? "yes" : "no");
    IDirect3DVertexShader9_GetFunction(s, NULL, &size);
    printf("GetFunction size matches: %s\n", size == sizeof vs20_plain ? "yes" : "no");
    use(s);
    IDirect3DDevice9_GetVertexShader(dev, &got);
    printf("GetVertexShader: %s\n", got == s ? "the same" : "different");
    if (got) IDirect3DVertexShader9_Release(got);
    use(NULL);
    IDirect3DDevice9_GetVertexShader(dev, &got);
    printf("after setting none: %s\n", got ? "a shader" : "none");
    vconst(100, 1.5f, -2, 3, 4);
    vconst(101, 5, 6, 7, 8);
    IDirect3DDevice9_GetVertexShaderConstantF(dev, 100, f, 2);
    printf("constants back: %.1f %.1f %.1f %.1f %.1f\n", f[0], f[1], f[2], f[3], f[7]);
    i[0] = 7; i[1] = -2; i[2] = 3; i[3] = 0;
    IDirect3DDevice9_SetVertexShaderConstantI(dev, 5, i, 1);
    memset(i, 0, sizeof i);
    IDirect3DDevice9_GetVertexShaderConstantI(dev, 5, i, 1);
    printf("integer constant back: %d %d %d\n", i[0], i[1], i[2]);
    b[0] = TRUE; b[1] = FALSE;
    IDirect3DDevice9_SetVertexShaderConstantB(dev, 3, b, 2);
    b[0] = FALSE; b[1] = TRUE;
    IDirect3DDevice9_GetVertexShaderConstantB(dev, 3, b, 2);
    printf("boolean constants back: %d %d\n", b[0], b[1]);
    hr = IDirect3DDevice9_CreateVertexShader(dev, bad, &got);
    printf("a bad shader: %s\n", FAILED(hr) ? "refused" : "accepted");
    printf("constant 300: 0x%lx\n", (unsigned long)IDirect3DDevice9_SetVertexShaderConstantF(dev, 300, f, 1));
    IDirect3DVertexShader9_Release(s);
}

static int compare(const unsigned char *img)
{
    static const char *names[PANELS] = {"vs_1_1 lighting", "loop over lights", "matrix palette", "wave", "shader fog", "point sizes",
                                        "texture coordinates", "dynamic branches", "clip plane", "three streams", "misc operations", "boolean branch"};
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
    IDirect3DTexture9 *tex;
    D3DLOCKED_RECT r;
    static unsigned char img[W * H * 3];
    HWND hwnd;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    hwnd = CreateWindowExA(0, "STATIC", "d3d2", WS_POPUP, 0, 0, W, H, NULL, NULL, NULL, NULL);
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
    pp.EnableAutoDepthStencil = TRUE;
    pp.AutoDepthStencilFormat = D3DFMT_D24S8;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    hr = IDirect3D9_CreateDevice(d3d, 0, D3DDEVTYPE_HAL, hwnd, D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &dev);
    printf("device: %s\n", SUCCEEDED(hr) ? "created" : "FAILED");
    if (FAILED(hr))
        return 1;
    make_sphere();
    tex = checker();
    IDirect3DDevice9_GetRenderTarget(dev, 0, &back);
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, 0xFF101010, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    lit();
    loop();
    skin();
    wave();
    fog();
    points();
    texgen(tex);
    dyn();
    clip();
    streams(tex);
    misc();
    call();
    IDirect3DDevice9_EndScene(dev);
    use(NULL);
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
    checks();
    IDirect3DTexture9_Release(tex);
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
    strcpy(slash ? slash + 1 : path, "d3d2.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
