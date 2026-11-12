/* G5d test: Direct3D 9's fixed-function pipeline, compared pixel by pixel
 * with what this machine's GPU draws (recorded on Windows).
 *
 * Draws 384x128 into the back buffer, in twelve 64x64 panels:
 *
 *   0  a lit sphere: a directional and a point light, specular, ambient
 *   1  a mipmapped floor in perspective (trilinear)
 *   2  texture stages: MODULATE, BLENDTEXTUREALPHA, MODULATE2X by TFACTOR
 *   3  more stage operations: DOTPRODUCT3, LERP, ADDSIGNED, a TEMP result
 *      with MULTIPLYADD
 *   4  Clear with rectangles, blending (factors, REVSUBTRACT, BLENDFACTOR),
 *      the alpha test on an A4R4G4B4 texture
 *   5  DXT1 (with its transparent colour and the alpha test), DXT3, DXT5,
 *      A8L8 textures
 *   6  fog: linear per vertex, and EXP2 per pixel by eye depth
 *   7  the stencil buffer (a disc, then EQUAL and NOTEQUAL) and depth
 *   8  points, sizes, point sprites, lines and strips
 *   9  a texture drawn into (a render target) and then drawn with
 *  10  ColorFill, StretchRect (linear), UpdateSurface into a texture
 *  11  flat shading, wireframe, culling, a user clip plane
 *
 * and prints defaults, state blocks, formats, locks and errors, the same on
 * Windows and on VEILOS.
 *
 *   d3d1                compare with d3d1.ref next to the program
 *   d3d1 record FILE    write what this machine draws as FILE (a PPM)
 *   d3d1 stats          as d3d1, with each panel's differences counted
 *
 * Tolerance: GPUs differ among themselves (rasterization ties, filtering
 * and colour precision), so in each panel at most 2% of pixels (81) may
 * differ by more than 8 in any channel.
 */
#define COBJMACROS
#include <windows.h>
#include <d3d9.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

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

/* Left-handed perspective, as D3DXMatrixPerspectiveFovLH. */
static M perspective(float fov, float zn, float zf)
{
    M r;
    float h = 1.0f / (float)tan(fov / 2);
    memset(&r, 0, sizeof r);
    r.m[0] = h;
    r.m[5] = h;
    r.m[10] = zf / (zf - zn);
    r.m[11] = 1;
    r.m[14] = -zn * zf / (zf - zn);
    return r;
}

static M translate(float x, float y, float z)
{
    M r = identity();
    r.m[12] = x; r.m[13] = y; r.m[14] = z;
    return r;
}

static M rot_y(float a)
{
    M r = identity();
    r.m[0] = (float)cos(a); r.m[2] = -(float)sin(a);
    r.m[8] = (float)sin(a); r.m[10] = (float)cos(a);
    return r;
}

static M rot_x(float a)
{
    M r = identity();
    r.m[5] = (float)cos(a); r.m[6] = (float)sin(a);
    r.m[9] = -(float)sin(a); r.m[10] = (float)cos(a);
    return r;
}

static void transform(D3DTRANSFORMSTATETYPE t, M m)
{
    IDirect3DDevice9_SetTransform(dev, t, (D3DMATRIX *)&m);
}

static void rs(D3DRENDERSTATETYPE s, DWORD v) { IDirect3DDevice9_SetRenderState(dev, s, v); }
static void tss(DWORD st, D3DTEXTURESTAGESTATETYPE s, DWORD v) { IDirect3DDevice9_SetTextureStageState(dev, st, s, v); }
static void samp(DWORD st, D3DSAMPLERSTATETYPE s, DWORD v) { IDirect3DDevice9_SetSamplerState(dev, st, s, v); }
static DWORD fbits(float f) { DWORD d; memcpy(&d, &f, 4); return d; }

/* Panel n's viewport; its corner in the window. */
static int px, py;
static void panel(int n)
{
    D3DVIEWPORT9 v;
    px = (n % 6) * 64;
    py = (n / 6) * 64;
    v.X = px; v.Y = py; v.Width = 64; v.Height = 64; v.MinZ = 0; v.MaxZ = 1;
    IDirect3DDevice9_SetViewport(dev, &v);
}

/* Back to the defaults this test leans on. */
static void reset(void)
{
    int i;
    rs(D3DRS_LIGHTING, FALSE);
    rs(D3DRS_ZENABLE, TRUE);
    rs(D3DRS_ZWRITEENABLE, TRUE);
    rs(D3DRS_CULLMODE, D3DCULL_NONE);
    rs(D3DRS_ALPHABLENDENABLE, FALSE);
    rs(D3DRS_ALPHATESTENABLE, FALSE);
    rs(D3DRS_FOGENABLE, FALSE);
    rs(D3DRS_SPECULARENABLE, FALSE);
    rs(D3DRS_STENCILENABLE, FALSE);
    rs(D3DRS_SHADEMODE, D3DSHADE_GOURAUD);
    rs(D3DRS_FILLMODE, D3DFILL_SOLID);
    rs(D3DRS_CLIPPLANEENABLE, 0);
    rs(D3DRS_COLORWRITEENABLE, 15);
    rs(D3DRS_BLENDOP, D3DBLENDOP_ADD);
    rs(D3DRS_POINTSPRITEENABLE, FALSE);
    rs(D3DRS_POINTSIZE, fbits(1));
    for (i = 0; i < 4; i++) {
        IDirect3DDevice9_SetTexture(dev, i, NULL);
        tss(i, D3DTSS_COLOROP, i ? D3DTOP_DISABLE : D3DTOP_MODULATE);
        tss(i, D3DTSS_ALPHAOP, i ? D3DTOP_DISABLE : D3DTOP_SELECTARG1);
        tss(i, D3DTSS_COLORARG1, D3DTA_TEXTURE);
        tss(i, D3DTSS_COLORARG2, D3DTA_CURRENT);
        tss(i, D3DTSS_ALPHAARG1, D3DTA_TEXTURE);
        tss(i, D3DTSS_ALPHAARG2, D3DTA_CURRENT);
        tss(i, D3DTSS_RESULTARG, D3DTA_CURRENT);
        tss(i, D3DTSS_TEXCOORDINDEX, i);
        samp(i, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        samp(i, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        samp(i, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        samp(i, D3DSAMP_ADDRESSU, D3DTADDRESS_WRAP);
        samp(i, D3DSAMP_ADDRESSV, D3DTADDRESS_WRAP);
    }
    transform(D3DTS_WORLD, identity());
    transform(D3DTS_VIEW, identity());
    transform(D3DTS_PROJECTION, perspective(1.0f, 0.5f, 50.0f));
}

/* -----| vertices |----- */

typedef struct { float x, y, z, nx, ny, nz; } VN;
#define FVF_VN (D3DFVF_XYZ | D3DFVF_NORMAL)
typedef struct { float x, y, z; DWORD c; float u, v; } VCT;
#define FVF_VCT (D3DFVF_XYZ | D3DFVF_DIFFUSE | D3DFVF_TEX1)
typedef struct { float x, y, z, rhw; DWORD c, s; float u, v, u2, v2; } VT;
#define FVF_VT (D3DFVF_XYZRHW | D3DFVF_DIFFUSE | D3DFVF_SPECULAR | D3DFVF_TEX2)

/* A pretransformed vertex in this panel. */
static VT tv(float x, float y, float z, DWORD c, float u, float v)
{
    VT r;
    r.x = px + x; r.y = py + y; r.z = z; r.rhw = 1;
    r.c = c; r.s = 0; r.u = u; r.v = v; r.u2 = u; r.v2 = v;
    return r;
}

/* A pretransformed quad x0..x1, y0..y1 with texture coordinates 0..1. */
static void quad(float x0, float y0, float x1, float y1, DWORD c0, DWORD c1, DWORD c2, DWORD c3)
{
    VT q[4];
    q[0] = tv(x0, y0, 0.5f, c0, 0, 0);
    q[1] = tv(x1, y0, 0.5f, c1, 1, 0);
    q[2] = tv(x0, y1, 0.5f, c2, 0, 1);
    q[3] = tv(x1, y1, 0.5f, c3, 1, 1);
    IDirect3DDevice9_SetFVF(dev, FVF_VT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof(VT));
}

/* -----| textures |----- */

static IDirect3DTexture9 *texture(int w, int h, int levels, D3DFORMAT f, D3DPOOL pool)
{
    IDirect3DTexture9 *t = NULL;
    if (FAILED(IDirect3DDevice9_CreateTexture(dev, w, h, levels, 0, f, pool, &t, NULL)))
        printf("CreateTexture %dx%d format %d: FAILED\n", w, h, (int)f);
    return t;
}

/* Fill level l of an A8R8G8B8 texture through `f(x, y, size)`. */
static void fill(IDirect3DTexture9 *t, int l, DWORD (*f)(int, int, int))
{
    D3DLOCKED_RECT r;
    D3DSURFACE_DESC d;
    unsigned x, y;
    IDirect3DTexture9_GetLevelDesc(t, l, &d);
    IDirect3DTexture9_LockRect(t, l, &r, NULL, 0);
    for (y = 0; y < d.Height; y++)
        for (x = 0; x < d.Width; x++)
            ((DWORD *)((char *)r.pBits + y * r.Pitch))[x] = f(x, y, d.Width);
    IDirect3DTexture9_UnlockRect(t, l);
}

static DWORD checker(int x, int y, int n)
{
    int s = n / 8 ? n / 8 : 1;
    return ((x / s + y / s) & 1) ? 0xFFE0E0E0 : 0xFF2040A0;
}

static DWORD ramp(int x, int y, int n)
{
    return (DWORD)(x * 255 / (n - 1)) << 24 | 0x30C030 | (DWORD)(y * 255 / (n - 1)) << 16;
}

/* A normal map: a bump, as colours (0.5 + n/2). */
static DWORD bumps(int x, int y, int n)
{
    float fx = (x + 0.5f) / n * 2 - 1, fy = (y + 0.5f) / n * 2 - 1, fz = 1 - fx * fx - fy * fy;
    float l;
    if (fz < 0.1f) fz = 0.1f;
    l = (float)sqrt(fx * fx + fy * fy + fz * fz);
    return 0xFF000000 | (DWORD)((fx / l * 0.5f + 0.5f) * 255) << 16 | (DWORD)((fy / l * 0.5f + 0.5f) * 255) << 8 | (DWORD)((fz / l * 0.5f + 0.5f) * 255);
}

/* The checker's mipmaps, each level 2x2 averaged from the one above. */
static IDirect3DTexture9 *mipmapped(void)
{
    IDirect3DTexture9 *t = texture(64, 64, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED);
    int l, n = IDirect3DTexture9_GetLevelCount(t);
    fill(t, 0, checker);
    for (l = 1; l < n; l++) {
        D3DLOCKED_RECT a, b;
        D3DSURFACE_DESC d;
        unsigned x, y, k;
        IDirect3DTexture9_GetLevelDesc(t, l, &d);
        IDirect3DTexture9_LockRect(t, l - 1, &a, NULL, D3DLOCK_READONLY);
        IDirect3DTexture9_LockRect(t, l, &b, NULL, 0);
        for (y = 0; y < d.Height; y++)
            for (x = 0; x < d.Width; x++) {
                DWORD *r0 = (DWORD *)((char *)a.pBits + 2 * y * a.Pitch), *r1 = (DWORD *)((char *)a.pBits + (2 * y + 1) * a.Pitch), out = 0;
                for (k = 0; k < 32; k += 8) {
                    DWORD s = (r0[2 * x] >> k & 255) + (r0[2 * x + 1] >> k & 255) + (r1[2 * x] >> k & 255) + (r1[2 * x + 1] >> k & 255);
                    out |= ((s + 2) / 4) << k;
                }
                ((DWORD *)((char *)b.pBits + y * b.Pitch))[x] = out;
            }
        IDirect3DTexture9_UnlockRect(t, l);
        IDirect3DTexture9_UnlockRect(t, l - 1);
    }
    return t;
}

/* -----| the panels |----- */

static void lit_sphere(void)
{
    static VN v[13 * 17];
    static WORD idx[12 * 16 * 6];
    D3DLIGHT9 l;
    D3DMATERIAL9 m;
    int i, j, n = 0;
    for (i = 0; i <= 12; i++)
        for (j = 0; j <= 16; j++) {
            float a = 3.14159265f * i / 12, b = 2 * 3.14159265f * j / 16;
            VN *p = &v[i * 17 + j];
            p->nx = (float)(sin(a) * cos(b)); p->ny = (float)cos(a); p->nz = (float)(sin(a) * sin(b));
            p->x = p->nx; p->y = p->ny; p->z = p->nz;
        }
    for (i = 0; i < 12; i++)
        for (j = 0; j < 16; j++) {
            WORD a = (WORD)(i * 17 + j), b = (WORD)(a + 17);
            idx[n++] = a; idx[n++] = a + 1; idx[n++] = b;
            idx[n++] = a + 1; idx[n++] = b + 1; idx[n++] = b;
        }
    panel(0);
    reset();
    rs(D3DRS_LIGHTING, TRUE);
    rs(D3DRS_SPECULARENABLE, TRUE);
    rs(D3DRS_AMBIENT, 0x00202020);
    memset(&m, 0, sizeof m);
    m.Diffuse.r = 0.7f; m.Diffuse.g = 0.7f; m.Diffuse.b = 0.9f; m.Diffuse.a = 1;
    m.Ambient.r = m.Ambient.g = m.Ambient.b = m.Ambient.a = 1;
    m.Specular.r = m.Specular.g = m.Specular.b = m.Specular.a = 1;
    m.Power = 20;
    IDirect3DDevice9_SetMaterial(dev, &m);
    memset(&l, 0, sizeof l);
    l.Type = D3DLIGHT_DIRECTIONAL;
    l.Diffuse.r = l.Diffuse.g = l.Diffuse.b = 0.8f;
    l.Specular.r = l.Specular.g = l.Specular.b = 0.6f;
    l.Direction.x = 0.6f; l.Direction.y = -0.5f; l.Direction.z = 0.6f;
    IDirect3DDevice9_SetLight(dev, 0, &l);
    IDirect3DDevice9_LightEnable(dev, 0, TRUE);
    memset(&l, 0, sizeof l);
    l.Type = D3DLIGHT_POINT;
    l.Diffuse.r = 0.9f; l.Diffuse.g = 0.3f; l.Diffuse.b = 0.1f;
    l.Ambient.b = 0.2f;
    l.Position.x = -2; l.Position.y = 1; l.Position.z = 1.5f;
    l.Range = 10;
    l.Attenuation0 = 0.5f; l.Attenuation1 = 0.2f;
    IDirect3DDevice9_SetLight(dev, 1, &l);
    IDirect3DDevice9_LightEnable(dev, 1, TRUE);
    transform(D3DTS_WORLD, translate(0, 0, 3));
    rs(D3DRS_CULLMODE, D3DCULL_CW);
    IDirect3DDevice9_SetFVF(dev, FVF_VN);
    IDirect3DDevice9_DrawIndexedPrimitiveUP(dev, D3DPT_TRIANGLELIST, 0, 13 * 17, n / 3, idx, D3DFMT_INDEX16, v, sizeof(VN));
    IDirect3DDevice9_LightEnable(dev, 0, FALSE);
    IDirect3DDevice9_LightEnable(dev, 1, FALSE);
}

static void floor_panel(IDirect3DTexture9 *t)
{
    IDirect3DVertexBuffer9 *vb;
    IDirect3DIndexBuffer9 *ib;
    VCT *v;
    WORD *ix;
    panel(1);
    reset();
    IDirect3DDevice9_CreateVertexBuffer(dev, 4 * sizeof(VCT), D3DUSAGE_WRITEONLY, FVF_VCT, D3DPOOL_MANAGED, &vb, NULL);
    IDirect3DDevice9_CreateIndexBuffer(dev, 6 * 2, D3DUSAGE_WRITEONLY, D3DFMT_INDEX16, D3DPOOL_MANAGED, &ib, NULL);
    IDirect3DVertexBuffer9_Lock(vb, 0, 0, (void **)&v, 0);
    v[0].x = -4; v[0].y = -1; v[0].z = 0.7f; v[0].u = 0; v[0].v = 0;
    v[1].x = 4; v[1].y = -1; v[1].z = 0.7f; v[1].u = 8; v[1].v = 0;
    v[2].x = -4; v[2].y = -1; v[2].z = 30; v[2].u = 0; v[2].v = 30;
    v[3].x = 4; v[3].y = -1; v[3].z = 30; v[3].u = 8; v[3].v = 30;
    v[0].c = v[1].c = v[2].c = v[3].c = 0xFFFFFFFF;
    IDirect3DVertexBuffer9_Unlock(vb);
    IDirect3DIndexBuffer9_Lock(ib, 0, 0, (void **)&ix, 0);
    ix[0] = 0; ix[1] = 2; ix[2] = 1; ix[3] = 1; ix[4] = 2; ix[5] = 3;
    IDirect3DIndexBuffer9_Unlock(ib);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    samp(0, D3DSAMP_MIPFILTER, D3DTEXF_LINEAR);
    IDirect3DDevice9_SetStreamSource(dev, 0, vb, 0, sizeof(VCT));
    IDirect3DDevice9_SetIndices(dev, ib);
    IDirect3DDevice9_SetFVF(dev, FVF_VCT);
    IDirect3DDevice9_DrawIndexedPrimitive(dev, D3DPT_TRIANGLELIST, 0, 0, 4, 0, 2);
    IDirect3DDevice9_SetStreamSource(dev, 0, NULL, 0, 0);
    IDirect3DDevice9_SetIndices(dev, NULL);
    IDirect3DVertexBuffer9_Release(vb);
    IDirect3DIndexBuffer9_Release(ib);
}

static void stages(IDirect3DTexture9 *check, IDirect3DTexture9 *alpha)
{
    panel(2);
    reset();
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)check);
    IDirect3DDevice9_SetTexture(dev, 1, (IDirect3DBaseTexture9 *)alpha);
    tss(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    tss(1, D3DTSS_COLOROP, D3DTOP_BLENDTEXTUREALPHA);
    tss(1, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    tss(1, D3DTSS_ALPHAARG1, D3DTA_CURRENT);
    tss(2, D3DTSS_COLOROP, D3DTOP_MODULATE2X);
    tss(2, D3DTSS_COLORARG1, D3DTA_CURRENT);
    tss(2, D3DTSS_COLORARG2, D3DTA_TFACTOR);
    tss(2, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    tss(2, D3DTSS_ALPHAARG1, D3DTA_CURRENT);
    rs(D3DRS_TEXTUREFACTOR, 0xFF9080C0);
    quad(4, 4, 60, 60, 0xFFFFFFFF, 0xFFFF8080, 0xFF80FF80, 0xFF8080FF);
}

static void more_ops(IDirect3DTexture9 *bump, IDirect3DTexture9 *check, IDirect3DTexture9 *alpha)
{
    panel(3);
    reset();
    /* DOTPRODUCT3: the bump lit from up and to the left. */
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)bump);
    tss(0, D3DTSS_COLOROP, D3DTOP_DOTPRODUCT3);
    tss(0, D3DTSS_COLORARG2, D3DTA_TFACTOR);
    rs(D3DRS_TEXTUREFACTOR, 0xFF5070D0);
    quad(0, 0, 32, 32, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF);
    /* LERP: from the diffuse colour to TFACTOR by the texture. */
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)check);
    tss(0, D3DTSS_COLOROP, D3DTOP_LERP);
    tss(0, D3DTSS_COLORARG0, D3DTA_TEXTURE);
    tss(0, D3DTSS_COLORARG1, D3DTA_TFACTOR);
    tss(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    rs(D3DRS_TEXTUREFACTOR, 0xFFC03020);
    quad(32, 0, 64, 32, 0xFF00FF00, 0xFF0000FF, 0xFFFFFF00, 0xFF00FFFF);
    /* ADDSIGNED. */
    tss(0, D3DTSS_COLOROP, D3DTOP_ADDSIGNED);
    tss(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
    tss(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    quad(0, 32, 32, 64, 0xFF404040, 0xFFC08040, 0xFF4080C0, 0xFF808080);
    /* A TEMP result, the diffuse colour as current, then MULTIPLYADD:
     * diffuse + temp * TFACTOR. */
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)alpha);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    tss(0, D3DTSS_COLORARG1, D3DTA_TEXTURE);
    tss(0, D3DTSS_RESULTARG, D3DTA_TEMP);
    tss(1, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    tss(1, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
    tss(1, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    tss(1, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
    tss(2, D3DTSS_COLOROP, D3DTOP_MULTIPLYADD);
    tss(2, D3DTSS_COLORARG0, D3DTA_CURRENT);
    tss(2, D3DTSS_COLORARG1, D3DTA_TEMP);
    tss(2, D3DTSS_COLORARG2, D3DTA_TFACTOR);
    tss(2, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    tss(2, D3DTSS_ALPHAARG1, D3DTA_CURRENT);
    rs(D3DRS_TEXTUREFACTOR, 0xFF606060);
    quad(32, 32, 64, 64, 0xFF200000, 0xFF002000, 0xFF000020, 0xFF202020);
}

static DWORD disc(int x, int y, int n)
{
    int dx = 2 * x + 1 - n, dy = 2 * y + 1 - n, r2 = dx * dx + dy * dy, a = 255 - r2 * 255 / (n * n);
    if (a < 0) a = 0;
    return (DWORD)a << 24 | (DWORD)(x * 255 / n) << 16 | 0x8000 | (DWORD)(y * 255 / n);
}

static void blending(void)
{
    IDirect3DTexture9 *t = texture(16, 16, 1, D3DFMT_A4R4G4B4, D3DPOOL_MANAGED);
    D3DLOCKED_RECT r;
    D3DRECT rects[2];
    int x, y;
    panel(4);
    reset();
    rects[0].x1 = px; rects[0].y1 = py; rects[0].x2 = px + 64; rects[0].y2 = py + 64;
    rects[1].x1 = px + 40; rects[1].y1 = py + 40; rects[1].x2 = px + 64; rects[1].y2 = py + 64;
    IDirect3DDevice9_Clear(dev, 1, rects, D3DCLEAR_TARGET, 0xFF606060, 1, 0);
    IDirect3DDevice9_Clear(dev, 1, rects + 1, D3DCLEAR_TARGET, 0xFFE0C020, 1, 0);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    tss(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG2);
    rs(D3DRS_ALPHABLENDENABLE, TRUE);
    rs(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    rs(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    quad(4, 4, 40, 28, 0x80FF0000, 0x80FF0000, 0x20FF0000, 0x20FF0000);
    quad(20, 10, 52, 36, 0xC00000FF, 0x4000FF00, 0xC00000FF, 0x4000FF00);
    rs(D3DRS_SRCBLEND, D3DBLEND_ONE);
    rs(D3DRS_DESTBLEND, D3DBLEND_ONE);
    rs(D3DRS_BLENDOP, D3DBLENDOP_REVSUBTRACT);
    quad(44, 4, 60, 36, 0xFF402010, 0xFF402010, 0xFF804020, 0xFF804020);
    rs(D3DRS_BLENDOP, D3DBLENDOP_ADD);
    rs(D3DRS_SRCBLEND, D3DBLEND_BLENDFACTOR);
    rs(D3DRS_DESTBLEND, D3DBLEND_INVBLENDFACTOR);
    rs(D3DRS_BLENDFACTOR, 0xFF4080C0);
    quad(4, 40, 24, 60, 0xFFFFFFFF, 0xFFFFFFFF, 0xFF000000, 0xFF000000);
    /* The alpha test on a disc of alpha. */
    rs(D3DRS_ALPHABLENDENABLE, FALSE);
    IDirect3DTexture9_LockRect(t, 0, &r, NULL, 0);
    for (y = 0; y < 16; y++)
        for (x = 0; x < 16; x++) {
            DWORD c = disc(x, y, 16);
            ((WORD *)((char *)r.pBits + y * r.Pitch))[x] = (WORD)((c >> 28) << 12 | (c >> 20 & 15) << 8 | (c >> 12 & 15) << 4 | (c >> 4 & 15));
        }
    IDirect3DTexture9_UnlockRect(t, 0);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    tss(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    samp(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    samp(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    rs(D3DRS_ALPHATESTENABLE, TRUE);
    rs(D3DRS_ALPHAREF, 0x60);
    rs(D3DRS_ALPHAFUNC, D3DCMP_GREATER);
    quad(28, 30, 60, 62, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF, 0xFFFFFFFF);
    IDirect3DTexture9_Release(t);
}

/* A DXT colour block: two 565 colours and 2-bit indices. */
static void color_block(unsigned char *b, WORD c0, WORD c1, DWORD bits)
{
    b[0] = (unsigned char)c0; b[1] = (unsigned char)(c0 >> 8);
    b[2] = (unsigned char)c1; b[3] = (unsigned char)(c1 >> 8);
    memcpy(b + 4, &bits, 4);
}

/* A 16x16 compressed texture: 4x4 blocks, made by `block(bx, by, out)`. */
static IDirect3DTexture9 *compressed(D3DFORMAT f, void (*block)(int, int, unsigned char *))
{
    IDirect3DTexture9 *t = texture(16, 16, 1, f, D3DPOOL_MANAGED);
    D3DLOCKED_RECT r;
    int bx, by, size = f == D3DFMT_DXT1 ? 8 : 16;
    IDirect3DTexture9_LockRect(t, 0, &r, NULL, 0);
    for (by = 0; by < 4; by++)
        for (bx = 0; bx < 4; bx++)
            block(bx, by, (unsigned char *)r.pBits + by * r.Pitch + bx * size);
    IDirect3DTexture9_UnlockRect(t, 0);
    return t;
}

static void dxt1(int bx, int by, unsigned char *b)
{
    /* Blocks on the diagonal: three colours and transparent (c0 <= c1). */
    if (bx == by)
        color_block(b, 0x001F, 0xF800, 0xE4E4E4E4);
    else
        color_block(b, (WORD)(0xF800 >> bx | by << 5), (WORD)(0x07E0 + bx * 3), 0x1B1B1B1B ^ (DWORD)(bx * 0x01010101 * by));
}

static void dxt3(int bx, int by, unsigned char *b)
{
    int i;
    for (i = 0; i < 8; i++) b[i] = (unsigned char)(0x10 * (i + bx * 2) | (by * 4));
    color_block(b + 8, 0xFFE0, (WORD)(0x001F | bx << 11), 0x50505050);
}

static void dxt5(int bx, int by, unsigned char *b)
{
    b[0] = (unsigned char)(255 - bx * 40);
    b[1] = (unsigned char)(by * 30);
    b[2] = 0x88; b[3] = 0xC6; b[4] = 0xFA; b[5] = 0x05; b[6] = 0x39; b[7] = 0x77;
    color_block(b + 8, (WORD)(0x8410 + by), (WORD)(0x07FF - bx * 0x40), 0xA5A5F00F);
}

static void textures_panel(void)
{
    IDirect3DTexture9 *t1 = compressed(D3DFMT_DXT1, dxt1), *t3 = compressed(D3DFMT_DXT3, dxt3), *t5 = compressed(D3DFMT_DXT5, dxt5);
    IDirect3DTexture9 *al = texture(8, 8, 1, D3DFMT_A8L8, D3DPOOL_MANAGED);
    D3DLOCKED_RECT r;
    int x, y;
    panel(5);
    reset();
    samp(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    samp(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    tss(0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    rs(D3DRS_ALPHATESTENABLE, TRUE);
    rs(D3DRS_ALPHAREF, 0x80);
    rs(D3DRS_ALPHAFUNC, D3DCMP_GREATEREQUAL);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t1);
    quad(0, 0, 32, 32, 0, 0, 0, 0);
    rs(D3DRS_ALPHATESTENABLE, FALSE);
    rs(D3DRS_ALPHABLENDENABLE, TRUE);
    rs(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    rs(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t3);
    quad(32, 0, 64, 32, 0, 0, 0, 0);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t5);
    quad(0, 32, 32, 64, 0, 0, 0, 0);
    IDirect3DTexture9_LockRect(al, 0, &r, NULL, 0);
    for (y = 0; y < 8; y++)
        for (x = 0; x < 8; x++)
            ((WORD *)((char *)r.pBits + y * r.Pitch))[x] = (WORD)((x * 36) << 8 | (y * 36));
    IDirect3DTexture9_UnlockRect(al, 0);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)al);
    tss(0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    tss(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    quad(32, 32, 64, 64, 0xFFFF8040, 0xFFFF8040, 0xFF40FF80, 0xFF40FF80);
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DTexture9_Release(t1);
    IDirect3DTexture9_Release(t3);
    IDirect3DTexture9_Release(t5);
    IDirect3DTexture9_Release(al);
}

static void fog_panel(void)
{
    VCT v[4];
    int i, half;
    panel(6);
    reset();
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    rs(D3DRS_FOGENABLE, TRUE);
    rs(D3DRS_FOGCOLOR, 0x003050A0);
    for (half = 0; half < 2; half++) {
        if (half == 0) {
            rs(D3DRS_FOGVERTEXMODE, D3DFOG_LINEAR);
            rs(D3DRS_FOGTABLEMODE, D3DFOG_NONE);
            rs(D3DRS_FOGSTART, fbits(1));
            rs(D3DRS_FOGEND, fbits(12));
        } else {
            rs(D3DRS_FOGVERTEXMODE, D3DFOG_NONE);
            rs(D3DRS_FOGTABLEMODE, D3DFOG_EXP2);
            rs(D3DRS_FOGDENSITY, fbits(0.12f));
        }
        for (i = 0; i < 4; i++) {
            float x0 = half ? 0.1f : -2.1f, x1 = half ? 2.1f : -0.1f;
            v[i].c = (i & 1) ? 0xFFFFE0A0 : 0xFFE0FFE0;
            v[i].u = v[i].v = 0;
        }
        v[0].x = half ? 0.1f : -2.1f; v[0].y = -0.5f; v[0].z = 0.8f;
        v[1].x = half ? 2.1f : -0.1f; v[1].y = -0.5f; v[1].z = 0.8f;
        v[2].x = half ? 0.1f : -2.1f; v[2].y = 1.5f; v[2].z = 20;
        v[3].x = half ? 2.1f : -0.1f; v[3].y = 1.5f; v[3].z = 20;
        IDirect3DDevice9_SetFVF(dev, FVF_VCT);
        IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, v, sizeof(VCT));
    }
}

static void stencil_panel(void)
{
    VT fan[34];
    VCT tri[6];
    D3DRECT r;
    int i;
    panel(7);
    reset();
    r.x1 = px; r.y1 = py; r.x2 = px + 64; r.y2 = py + 64;
    IDirect3DDevice9_Clear(dev, 1, &r, D3DCLEAR_STENCIL | D3DCLEAR_ZBUFFER, 0, 1, 0);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    rs(D3DRS_STENCILENABLE, TRUE);
    rs(D3DRS_STENCILFUNC, D3DCMP_ALWAYS);
    rs(D3DRS_STENCILPASS, D3DSTENCILOP_REPLACE);
    rs(D3DRS_STENCILREF, 1);
    rs(D3DRS_COLORWRITEENABLE, 0);
    rs(D3DRS_ZWRITEENABLE, FALSE);
    fan[0] = tv(32, 32, 0.5f, 0, 0, 0);
    for (i = 0; i <= 32; i++)
        fan[i + 1] = tv(32 + 22.3f * (float)cos(i * 3.14159265f / 16), 32 + 22.3f * (float)sin(i * 3.14159265f / 16), 0.5f, 0, 0, 0);
    IDirect3DDevice9_SetFVF(dev, FVF_VT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLEFAN, 32, fan, sizeof(VT));
    rs(D3DRS_COLORWRITEENABLE, 15);
    rs(D3DRS_STENCILPASS, D3DSTENCILOP_KEEP);
    rs(D3DRS_STENCILFUNC, D3DCMP_EQUAL);
    quad(0, 0, 64, 64, 0xFFC02020, 0xFFC02020, 0xFF802020, 0xFF802020);
    rs(D3DRS_STENCILFUNC, D3DCMP_NOTEQUAL);
    quad(0, 0, 64, 64, 0xFF2020C0, 0xFF2040C0, 0xFF2020C0, 0xFF2040C0);
    rs(D3DRS_STENCILENABLE, FALSE);
    rs(D3DRS_ZWRITEENABLE, TRUE);
    /* Two triangles through each other. */
    for (i = 0; i < 6; i++) tri[i].u = tri[i].v = 0;
    tri[0].x = -1.2f; tri[0].y = -0.8f; tri[0].z = 2; tri[0].c = 0xFFFFFF00;
    tri[1].x = 0.0f; tri[1].y = 1.0f; tri[1].z = 4; tri[1].c = 0xFFFFFF00;
    tri[2].x = 1.2f; tri[2].y = -0.6f; tri[2].z = 5; tri[2].c = 0xFFFFC000;
    tri[3].x = -1.2f; tri[3].y = 0.6f; tri[3].z = 5; tri[3].c = 0xFF00FFFF;
    tri[4].x = 1.2f; tri[4].y = 0.8f; tri[4].z = 2; tri[4].c = 0xFF00FFFF;
    tri[5].x = 0.1f; tri[5].y = -1.0f; tri[5].z = 3; tri[5].c = 0xFF00C0FF;
    IDirect3DDevice9_SetFVF(dev, FVF_VCT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 2, tri, sizeof(VCT));
}

static void points_lines(IDirect3DTexture9 *spr)
{
    VT p[8], l[6];
    int i;
    panel(8);
    reset();
    rs(D3DRS_ZENABLE, FALSE);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    for (i = 0; i < 8; i++) p[i] = tv(5.5f + i * 7, 6.5f, 0.5f, 0xFF000000 | (DWORD)(i * 32) << 16 | 0xFF00 | (DWORD)(255 - i * 32), 0, 0);
    IDirect3DDevice9_SetFVF(dev, FVF_VT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_POINTLIST, 8, p, sizeof(VT));
    rs(D3DRS_POINTSIZE, fbits(4));
    for (i = 0; i < 8; i++) p[i].y = py + 15.5f;
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_POINTLIST, 8, p, sizeof(VT));
    /* Point sprites, textured. */
    rs(D3DRS_POINTSPRITEENABLE, TRUE);
    rs(D3DRS_POINTSIZE, fbits(14));
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)spr);
    tss(0, D3DTSS_COLOROP, D3DTOP_MODULATE);
    tss(0, D3DTSS_COLORARG2, D3DTA_DIFFUSE);
    p[0] = tv(12, 30, 0.5f, 0xFFFFFFFF, 0, 0);
    p[1] = tv(32, 30, 0.5f, 0xFFFF8080, 0, 0);
    p[2] = tv(52, 30, 0.5f, 0xFF80FF80, 0, 0);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_POINTLIST, 3, p, sizeof(VT));
    rs(D3DRS_POINTSPRITEENABLE, FALSE);
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    l[0] = tv(3.2f, 41.3f, 0.5f, 0xFFFF0000, 0, 0);
    l[1] = tv(60.7f, 47.6f, 0.5f, 0xFF00FF00, 0, 0);
    l[2] = tv(6.4f, 60.2f, 0.5f, 0xFFFFFF00, 0, 0);
    l[3] = tv(20.3f, 39.7f, 0.5f, 0xFF00FFFF, 0, 0);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_LINELIST, 2, l, sizeof(VT));
    l[0] = tv(30.2f, 60.3f, 0.5f, 0xFFFFFFFF, 0, 0);
    l[1] = tv(40.6f, 51.2f, 0.5f, 0xFFFF00FF, 0, 0);
    l[2] = tv(52.3f, 61.4f, 0.5f, 0xFF8080FF, 0, 0);
    l[3] = tv(61.2f, 53.3f, 0.5f, 0xFFFFFF80, 0, 0);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_LINESTRIP, 3, l, sizeof(VT));
}

static IDirect3DTexture9 *rt_tex;

static void render_to_texture(IDirect3DSurface9 *back, IDirect3DSurface9 *depth)
{
    IDirect3DSurface9 *s;
    VT t[3];
    VCT q[4];
    D3DVIEWPORT9 v;
    if (FAILED(IDirect3DDevice9_CreateTexture(dev, 64, 64, 1, D3DUSAGE_RENDERTARGET, D3DFMT_X8R8G8B8, D3DPOOL_DEFAULT, &rt_tex, NULL))) {
        printf("render target texture: FAILED\n");
        return;
    }
    IDirect3DTexture9_GetSurfaceLevel(rt_tex, 0, &s);
    IDirect3DDevice9_SetRenderTarget(dev, 0, s);
    IDirect3DDevice9_SetDepthStencilSurface(dev, NULL);
    IDirect3DDevice9_GetViewport(dev, &v);
    printf("viewport after SetRenderTarget: %lux%lu\n", v.Width, v.Height);
    reset();
    rs(D3DRS_ZENABLE, FALSE);
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET, 0xFF102030, 1, 0);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    px = py = 0;
    t[0] = tv(32, 3, 0.5f, 0xFFFF0000, 0, 0);
    t[1] = tv(61, 58, 0.5f, 0xFF00FF00, 0, 0);
    t[2] = tv(4, 50, 0.5f, 0xFF0000FF, 0, 0);
    IDirect3DDevice9_SetFVF(dev, FVF_VT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, t, sizeof(VT));
    IDirect3DDevice9_SetRenderTarget(dev, 0, back);
    IDirect3DDevice9_SetDepthStencilSurface(dev, depth);
    IDirect3DSurface9_Release(s);
    /* Drawn with, turned and in perspective. */
    panel(9);
    reset();
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)rt_tex);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    q[0].x = -1; q[0].y = 1; q[0].u = 0; q[0].v = 0;
    q[1].x = 1; q[1].y = 1; q[1].u = 1; q[1].v = 0;
    q[2].x = -1; q[2].y = -1; q[2].u = 0; q[2].v = 1;
    q[3].x = 1; q[3].y = -1; q[3].u = 1; q[3].v = 1;
    q[0].z = q[1].z = q[2].z = q[3].z = 0;
    q[0].c = q[1].c = q[2].c = q[3].c = 0xFFFFFFFF;
    transform(D3DTS_WORLD, mul(mul(rot_y(0.6f), rot_x(0.3f)), translate(0, 0, 2.6f)));
    IDirect3DDevice9_SetFVF(dev, FVF_VCT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 2, q, sizeof(VCT));
}

static void copies(IDirect3DSurface9 *back)
{
    IDirect3DSurface9 *src, *sys;
    IDirect3DTexture9 *t;
    D3DLOCKED_RECT r;
    RECT a, b;
    int x, y;
    panel(10);
    reset();
    a.left = px; a.top = py; a.right = px + 64; a.bottom = py + 32;
    IDirect3DDevice9_ColorFill(dev, back, &a, 0xFF208040);
    a.left = px + 8; a.top = py + 8; a.right = px + 24; a.bottom = py + 24;
    IDirect3DDevice9_ColorFill(dev, back, &a, 0xFFF0F0F0);
    /* The rendered texture, shrunk into a corner. */
    IDirect3DTexture9_GetSurfaceLevel(rt_tex, 0, &src);
    a.left = 0; a.top = 0; a.right = 64; a.bottom = 64;
    b.left = px + 32; b.top = py; b.right = px + 64; b.bottom = py + 32;
    printf("StretchRect: 0x%lx\n", (unsigned long)IDirect3DDevice9_StretchRect(dev, src, &a, back, &b, D3DTEXF_LINEAR));
    IDirect3DSurface9_Release(src);
    /* A system memory surface into a texture, drawn. */
    IDirect3DDevice9_CreateOffscreenPlainSurface(dev, 16, 16, D3DFMT_A8R8G8B8, D3DPOOL_SYSTEMMEM, &sys, NULL);
    IDirect3DSurface9_LockRect(sys, &r, NULL, 0);
    for (y = 0; y < 16; y++)
        for (x = 0; x < 16; x++)
            ((DWORD *)((char *)r.pBits + y * r.Pitch))[x] = 0xFF000000 | (DWORD)(x * 16) << 16 | (DWORD)(y * 16) << 8 | (DWORD)((x ^ y) * 16);
    IDirect3DSurface9_UnlockRect(sys);
    IDirect3DDevice9_CreateTexture(dev, 16, 16, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_DEFAULT, &t, NULL);
    IDirect3DTexture9_GetSurfaceLevel(t, 0, &src);
    printf("UpdateSurface: 0x%lx\n", (unsigned long)IDirect3DDevice9_UpdateSurface(dev, sys, NULL, src, NULL));
    IDirect3DSurface9_Release(src);
    IDirect3DSurface9_Release(sys);
    IDirect3DDevice9_SetTexture(dev, 0, (IDirect3DBaseTexture9 *)t);
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    samp(0, D3DSAMP_MINFILTER, D3DTEXF_POINT);
    samp(0, D3DSAMP_MAGFILTER, D3DTEXF_POINT);
    quad(0, 32, 64, 64, 0, 0, 0, 0);
    IDirect3DDevice9_SetTexture(dev, 0, NULL);
    IDirect3DTexture9_Release(t);
}

static void shading(void)
{
    VT s[6], f[5];
    VCT c[3];
    float plane[4] = {1, 0.4f, 0, -0.1f};
    int i;
    panel(11);
    reset();
    tss(0, D3DTSS_COLOROP, D3DTOP_SELECTARG2);
    rs(D3DRS_ZENABLE, FALSE);
    /* A flat-shaded strip: each triangle its first vertex's colour. */
    rs(D3DRS_SHADEMODE, D3DSHADE_FLAT);
    for (i = 0; i < 6; i++)
        s[i] = tv(4.3f + (i / 2) * 18.1f, (i & 1) ? 18.4f : 3.6f, 0.5f, 0xFF000000 | (DWORD)(i * 50) << 16 | (DWORD)(250 - i * 40) << 8 | 0x40, 0, 0);
    IDirect3DDevice9_SetFVF(dev, FVF_VT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, 4, s, sizeof(VT));
    rs(D3DRS_SHADEMODE, D3DSHADE_GOURAUD);
    /* Wireframe. */
    rs(D3DRS_FILLMODE, D3DFILL_WIREFRAME);
    f[0] = tv(10.4f, 24.3f, 0.5f, 0xFFFFFFFF, 0, 0);
    f[1] = tv(28.6f, 25.1f, 0.5f, 0xFFFF4040, 0, 0);
    f[2] = tv(27.2f, 38.7f, 0.5f, 0xFF40FF40, 0, 0);
    f[3] = tv(14.1f, 41.4f, 0.5f, 0xFF4040FF, 0, 0);
    f[4] = tv(5.3f, 33.2f, 0.5f, 0xFFFFFF40, 0, 0);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLEFAN, 3, f, sizeof(VT));
    rs(D3DRS_FILLMODE, D3DFILL_SOLID);
    /* Culling: only the clockwise one stays. */
    rs(D3DRS_CULLMODE, D3DCULL_CCW);
    f[0] = tv(36.2f, 22.3f, 0.5f, 0xFFFF8000, 0, 0);
    f[1] = tv(60.4f, 24.1f, 0.5f, 0xFFFF8000, 0, 0);
    f[2] = tv(44.3f, 40.2f, 0.5f, 0xFFFF8000, 0, 0);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, f, sizeof(VT));
    f[0] = tv(38.1f, 42.4f, 0.5f, 0xFF0080FF, 0, 0);
    f[1] = tv(48.3f, 60.2f, 0.5f, 0xFF0080FF, 0, 0);
    f[2] = tv(61.2f, 44.1f, 0.5f, 0xFF0080FF, 0, 0);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, f, sizeof(VT));
    rs(D3DRS_CULLMODE, D3DCULL_NONE);
    /* A user clip plane (world space) through a triangle. */
    IDirect3DDevice9_SetClipPlane(dev, 0, plane);
    rs(D3DRS_CLIPPLANEENABLE, 1);
    rs(D3DRS_ZENABLE, TRUE);
    for (i = 0; i < 3; i++) c[i].u = c[i].v = 0;
    c[0].x = -1.1f; c[0].y = -0.4f; c[0].z = 3; c[0].c = 0xFFFF00FF;
    c[1].x = 0.3f; c[1].y = -1.3f; c[1].z = 3; c[1].c = 0xFF00FF00;
    c[2].x = 0.9f; c[2].y = -0.2f; c[2].z = 3; c[2].c = 0xFFFFFFFF;
    IDirect3DDevice9_SetFVF(dev, FVF_VCT);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLELIST, 1, c, sizeof(VCT));
    rs(D3DRS_CLIPPLANEENABLE, 0);
}

/* -----| checks that print |----- */

static void defaults(void)
{
    DWORD v;
    float f;
#define RS(name) IDirect3DDevice9_GetRenderState(dev, name, &v); printf(#name " = %lu\n", v)
    RS(D3DRS_LIGHTING); RS(D3DRS_ZENABLE); RS(D3DRS_CULLMODE); RS(D3DRS_ZFUNC); RS(D3DRS_SRCBLEND); RS(D3DRS_DESTBLEND);
    RS(D3DRS_ALPHAFUNC); RS(D3DRS_SHADEMODE); RS(D3DRS_COLORVERTEX); RS(D3DRS_DIFFUSEMATERIALSOURCE); RS(D3DRS_STENCILMASK);
    RS(D3DRS_COLORWRITEENABLE); RS(D3DRS_TEXTUREFACTOR); RS(D3DRS_BLENDOP);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_FOGEND, &v); memcpy(&f, &v, 4); printf("D3DRS_FOGEND = %.2f\n", f);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_POINTSIZE_MAX, &v); memcpy(&f, &v, 4); printf("D3DRS_POINTSIZE_MAX = %s\n", f >= 64 ? "64 or more" : "less than 64");
    IDirect3DDevice9_GetTextureStageState(dev, 0, D3DTSS_COLOROP, &v); printf("stage 0 COLOROP = %lu\n", v);
    IDirect3DDevice9_GetTextureStageState(dev, 1, D3DTSS_COLOROP, &v); printf("stage 1 COLOROP = %lu\n", v);
    IDirect3DDevice9_GetTextureStageState(dev, 0, D3DTSS_ALPHAOP, &v); printf("stage 0 ALPHAOP = %lu\n", v);
    IDirect3DDevice9_GetTextureStageState(dev, 3, D3DTSS_TEXCOORDINDEX, &v); printf("stage 3 TEXCOORDINDEX = %lu\n", v);
    IDirect3DDevice9_GetSamplerState(dev, 0, D3DSAMP_ADDRESSU, &v); printf("sampler 0 ADDRESSU = %lu\n", v);
    IDirect3DDevice9_GetSamplerState(dev, 0, D3DSAMP_MAGFILTER, &v); printf("sampler 0 MAGFILTER = %lu\n", v);
#undef RS
}

static void checks(IDirect3D9 *d3d)
{
    D3DCAPS9 caps;
    IDirect3DStateBlock9 *sb;
    IDirect3DQuery9 *q;
    IDirect3DTexture9 *t;
    IDirect3DSurface9 *s;
    D3DSURFACE_DESC sd;
    D3DLOCKED_RECT r;
    D3DLIGHT9 l;
    DWORD v, data = 0;
    BOOL on;
    HRESULT hr;
    int spins = 0;

    IDirect3DDevice9_GetDeviceCaps(dev, &caps);
    printf("blend stages >= 4: %s\n", caps.MaxTextureBlendStages >= 4 ? "yes" : "no");
    printf("simultaneous textures >= 2: %s\n", caps.MaxSimultaneousTextures >= 2 ? "yes" : "no");
    printf("lights >= 8: %s\n", caps.MaxActiveLights >= 8 ? "yes" : "no");
    printf("DOTPRODUCT3, LERP, MULTIPLYADD: %s\n", (caps.TextureOpCaps & (D3DTEXOPCAPS_DOTPRODUCT3 | D3DTEXOPCAPS_LERP | D3DTEXOPCAPS_MULTIPLYADD)) == (D3DTEXOPCAPS_DOTPRODUCT3 | D3DTEXOPCAPS_LERP | D3DTEXOPCAPS_MULTIPLYADD) ? "yes" : "no");
    printf("two-sided stencil: %s\n", caps.StencilCaps & D3DSTENCILCAPS_TWOSIDED ? "yes" : "no");
    printf("A8R8G8B8 textures: 0x%lx\n", (unsigned long)IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, 0, D3DRTYPE_TEXTURE, D3DFMT_A8R8G8B8));
    printf("DXT5 textures: 0x%lx\n", (unsigned long)IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, 0, D3DRTYPE_TEXTURE, D3DFMT_DXT5));
    printf("D24S8 depth: 0x%lx\n", (unsigned long)IDirect3D9_CheckDeviceFormat(d3d, 0, D3DDEVTYPE_HAL, D3DFMT_X8R8G8B8, D3DUSAGE_DEPTHSTENCIL, D3DRTYPE_SURFACE, D3DFMT_D24S8));
    printf("adapters >= 1: %s\n", IDirect3D9_GetAdapterCount(d3d) >= 1 ? "yes" : "no");

    IDirect3DDevice9_GetBackBuffer(dev, 0, 0, D3DBACKBUFFER_TYPE_MONO, &s);
    IDirect3DSurface9_GetDesc(s, &sd);
    printf("back buffer: format %d, %ux%u, usage %lu\n", (int)sd.Format, sd.Width, sd.Height, sd.Usage);
    IDirect3DSurface9_Release(s);

    /* State blocks: recorded, then applied over a change. */
    IDirect3DDevice9_BeginStateBlock(dev);
    rs(D3DRS_AMBIENT, 0x00123456);
    tss(1, D3DTSS_COLOROP, D3DTOP_ADD);
    IDirect3DDevice9_EndStateBlock(dev, &sb);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_AMBIENT, &v);
    printf("ambient after recording: 0x%lx\n", v);
    rs(D3DRS_AMBIENT, 0x00FFFFFF);
    rs(D3DRS_FOGCOLOR, 0x00ABCDEF);
    IDirect3DStateBlock9_Apply(sb);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_AMBIENT, &v);
    printf("ambient after Apply: 0x%lx\n", v);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_FOGCOLOR, &v);
    printf("fog colour after Apply (not recorded): 0x%lx\n", v);
    IDirect3DDevice9_GetTextureStageState(dev, 1, D3DTSS_COLOROP, &v);
    printf("stage 1 COLOROP after Apply: %lu\n", v);
    rs(D3DRS_AMBIENT, 0);
    IDirect3DStateBlock9_Capture(sb);
    rs(D3DRS_AMBIENT, 0x00777777);
    IDirect3DStateBlock9_Apply(sb);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_AMBIENT, &v);
    printf("ambient after Capture and Apply: 0x%lx\n", v);
    IDirect3DStateBlock9_Release(sb);
    tss(1, D3DTSS_COLOROP, D3DTOP_DISABLE);
    IDirect3DDevice9_CreateStateBlock(dev, D3DSBT_ALL, &sb);
    rs(D3DRS_ZFUNC, D3DCMP_ALWAYS);
    IDirect3DStateBlock9_Apply(sb);
    IDirect3DDevice9_GetRenderState(dev, D3DRS_ZFUNC, &v);
    printf("ZFUNC after an ALL block: %lu\n", v);
    IDirect3DStateBlock9_Release(sb);

    /* Lights. */
    memset(&l, 0, sizeof l);
    l.Type = D3DLIGHT_SPOT; l.Range = 5; l.Theta = 0.5f; l.Phi = 1; l.Falloff = 1; l.Direction.z = 1;
    IDirect3DDevice9_SetLight(dev, 3, &l);
    IDirect3DDevice9_LightEnable(dev, 3, TRUE);
    IDirect3DDevice9_GetLightEnable(dev, 3, &on);
    printf("light 3 enabled: %s\n", on ? "yes" : "no");
    printf("light 7 (never set): 0x%lx\n", (unsigned long)IDirect3DDevice9_GetLight(dev, 7, &l));
    IDirect3DDevice9_LightEnable(dev, 3, FALSE);

    /* A lock round trip. */
    t = texture(4, 4, 1, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED);
    IDirect3DTexture9_LockRect(t, 0, &r, NULL, 0);
    ((DWORD *)r.pBits)[5] = 0x12345678;
    IDirect3DTexture9_UnlockRect(t, 0);
    IDirect3DTexture9_LockRect(t, 0, &r, NULL, D3DLOCK_READONLY);
    printf("texel back: 0x%lx, pitch >= 16: %s\n", ((DWORD *)r.pBits)[5], r.Pitch >= 16 ? "yes" : "no");
    IDirect3DTexture9_UnlockRect(t, 0);
    printf("levels: %lu\n", IDirect3DTexture9_GetLevelCount(t));
    IDirect3DTexture9_Release(t);
    t = texture(32, 8, 0, D3DFMT_X8R8G8B8, D3DPOOL_MANAGED);
    printf("levels of a full 32x8 chain: %lu\n", IDirect3DTexture9_GetLevelCount(t));
    IDirect3DTexture9_GetLevelDesc(t, 4, &sd);
    printf("level 4: %ux%u\n", sd.Width, sd.Height);
    IDirect3DTexture9_Release(t);

    /* A query. */
    if (SUCCEEDED(IDirect3DDevice9_CreateQuery(dev, D3DQUERYTYPE_EVENT, &q))) {
        IDirect3DQuery9_Issue(q, D3DISSUE_END);
        while ((hr = IDirect3DQuery9_GetData(q, &data, sizeof data, D3DGETDATA_FLUSH)) == S_FALSE && spins < 10000000) spins++;
        printf("event query: 0x%lx, %lu\n", (unsigned long)hr, data);
        IDirect3DQuery9_Release(q);
    }

    /* Errors. */
    printf("a 0-wide texture: 0x%lx\n", (unsigned long)IDirect3DDevice9_CreateTexture(dev, 0, 4, 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL));
    printf("a managed render target texture: 0x%lx\n", (unsigned long)IDirect3DDevice9_CreateTexture(dev, 4, 4, 1, D3DUSAGE_RENDERTARGET, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL));
    printf("a light of type 0: 0x%lx\n", (unsigned long)IDirect3DDevice9_SetLight(dev, 0, (memset(&l, 0, sizeof l), &l)));
}

static int compare(const unsigned char *img)
{
    static const char *names[PANELS] = {"lit sphere", "mipmapped floor", "texture stages", "more stage operations", "blending and alpha test", "compressed textures",
                                        "fog", "stencil and depth", "points and lines", "render to texture", "copies", "shading, culling, clip plane"};
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
    IDirect3DSurface9 *back, *depth, *sys;
    IDirect3DTexture9 *mip, *check, *alpha, *bump;
    D3DLOCKED_RECT r;
    static unsigned char img[W * H * 3];
    HWND hwnd;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    hwnd = CreateWindowExA(0, "STATIC", "d3d1", WS_POPUP, 0, 0, W, H, NULL, NULL, NULL, NULL);
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
    defaults();

    IDirect3DDevice9_GetRenderTarget(dev, 0, &back);
    IDirect3DDevice9_GetDepthStencilSurface(dev, &depth);
    mip = mipmapped();
    check = texture(32, 32, 1, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED);
    fill(check, 0, checker);
    alpha = texture(32, 32, 1, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED);
    fill(alpha, 0, ramp);
    bump = texture(32, 32, 1, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED);
    fill(bump, 0, bumps);

    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER | D3DCLEAR_STENCIL, 0xFF000000, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    lit_sphere();
    floor_panel(mip);
    stages(check, alpha);
    more_ops(bump, check, alpha);
    blending();
    textures_panel();
    fog_panel();
    stencil_panel();
    points_lines(alpha);
    render_to_texture(back, depth);
    copies(back);
    shading();
    IDirect3DDevice9_EndScene(dev);
    printf("Present: 0x%lx\n", (unsigned long)IDirect3DDevice9_Present(dev, NULL, NULL, NULL, NULL));

    /* What was drawn, read back. */
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

    checks(d3d);

    IDirect3DTexture9_Release(mip);
    IDirect3DTexture9_Release(check);
    IDirect3DTexture9_Release(alpha);
    IDirect3DTexture9_Release(bump);
    if (rt_tex) IDirect3DTexture9_Release(rt_tex);
    IDirect3DSurface9_Release(back);
    IDirect3DSurface9_Release(depth);
    printf("device released: %s\n", IDirect3DDevice9_Release(dev) == 0 ? "yes" : "no");
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
    strcpy(slash ? slash + 1 : path, "d3d1.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
