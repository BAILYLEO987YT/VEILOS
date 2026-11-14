/* G5e2 test: Direct3D 9 pixel shaders (ps_2_0, ps_3_0), compared pixel by
 * pixel with what this machine's GPU draws (recorded on Windows). The
 * shaders are d3d3.hlsl, compiled with fxc by build.sh.
 *
 * Draws 384x128 into the back buffer, in twelve 64x64 panels:
 *
 *   0  a texture times the colour, mipmapped, in perspective
 *   1  per-pixel lighting: normalize, diffuse, a specular power
 *   2  two textures mixed by a constant, alpha blended
 *   3  texkill outside a ring, cmp choosing colours
 *   4  a dependent texture read
 *   5  a projected texture (texldp)
 *   6  a level-of-detail bias (texldb)
 *   7  frac, exp2, log2, sin and cos, rsq, min, max
 *   8  depth written by the shader, two quads crossing
 *   9  fixed-function fog after a ps_2_0 shader
 *  10  ps_3_0: a loop over lights by aL, vFace, vPos
 *  11  ps_3_0: a dynamic branch and an early break
 *
 * and prints what shader objects and caps give, the same on Windows and on
 * VEILOS.
 *
 *   d3d3                compare with d3d3.ref next to the program
 *   d3d3 record FILE    write what this machine draws as FILE (a PPM)
 *   d3d3 stats          as d3d3, with each panel's differences counted
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

#include "build/d3d3_vs20_main.h"
#include "build/d3d3_vs20_proj.h"
#include "build/d3d3_vs20_fog.h"
#include "build/d3d3_vs30_main.h"
#include "build/d3d3_ps20_modulate.h"
#include "build/d3d3_ps20_light.h"
#include "build/d3d3_ps20_mix.h"
#include "build/d3d3_ps20_kill.h"
#include "build/d3d3_ps20_dependent.h"
#include "build/d3d3_ps20_proj.h"
#include "build/d3d3_ps20_bias.h"
#include "build/d3d3_ps20_math.h"
#include "build/d3d3_ps20_depth.h"
#include "build/d3d3_ps20_fog.h"
#include "build/d3d3_ps30_main.h"
#include "build/d3d3_ps30_branch.h"

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

static M rot_y(float a)
{
    M r = identity();
    r.m[0] = (float)cos(a); r.m[2] = -(float)sin(a);
    r.m[8] = (float)sin(a); r.m[10] = (float)cos(a);
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

static void pconst(int reg, float x, float y, float z, float w)
{
    float v[4];
    v[0] = x; v[1] = y; v[2] = z; v[3] = w;
    IDirect3DDevice9_SetPixelShaderConstantF(dev, reg, v, 1);
}

static void matrices(M world)
{
    M wvp = mul(world, perspective(1.0f, 0.5f, 50.0f));
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 0, wvp.m, 4);
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 4, world.m, 4);
}

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
#define VS(name) vshader(name, sizeof name)
#define PS(name) pshader(name, sizeof name)

/* The shaders in use, released when replaced. */
static IDirect3DVertexShader9 *cur_vs;
static IDirect3DPixelShader9 *cur_ps;

static void use(IDirect3DVertexShader9 *v, IDirect3DPixelShader9 *p)
{
    IDirect3DDevice9_SetVertexShader(dev, v);
    IDirect3DDevice9_SetPixelShader(dev, p);
    if (cur_vs) IDirect3DVertexShader9_Release(cur_vs);
    if (cur_ps) IDirect3DPixelShader9_Release(cur_ps);
    cur_vs = v;
    cur_ps = p;
}

static int px, py;
static void panel(int n)
{
    D3DVIEWPORT9 v;
    int i;
    px = (n % 6) * 64;
    py = (n / 6) * 64;
    v.X = px; v.Y = py; v.Width = 64; v.Height = 64; v.MinZ = 0; v.MaxZ = 1;
    IDirect3DDevice9_SetViewport(dev, &v);
    rs(D3DRS_CULLMODE, D3DCULL_NONE);
    rs(D3DRS_FOGENABLE, FALSE);
    rs(D3DRS_ALPHABLENDENABLE, FALSE);
    for (i = 0; i < 2; i++) {
        IDirect3DDevice9_SetTexture(dev, i, NULL);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_MIPFILTER, D3DTEXF_NONE);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_ADDRESSU, D3DTADDRESS_WRAP);
        IDirect3DDevice9_SetSamplerState(dev, i, D3DSAMP_ADDRESSV, D3DTADDRESS_WRAP);
    }
}

/* -----| geometry |----- */

typedef struct { float x, y, z, nx, ny, nz; DWORD c; float u, v; } VX;
#define FVF_VX (D3DFVF_XYZ | D3DFVF_NORMAL | D3DFVF_DIFFUSE | D3DFVF_TEX1)

static VX vx(float x, float y, float z, DWORD c, float u, float v)
{
    VX r;
    r.x = x; r.y = y; r.z = z; r.nx = 0; r.ny = 0; r.nz = -1; r.c = c; r.u = u; r.v = v;
    return r;
}

static void draw_strip(VX *v, int n)
{
    IDirect3DDevice9_SetFVF(dev, FVF_VX);
    IDirect3DDevice9_DrawPrimitiveUP(dev, D3DPT_TRIANGLESTRIP, n - 2, v, sizeof(VX));
}

/* A quad at depth z: corners (x0, y0) .. (x1, y1), texture 0..1 times rep. */
static void quad(float x0, float y0, float x1, float y1, float z, DWORD c0, DWORD c1, float rep)
{
    VX q[4];
    q[0] = vx(x0, y1, z, c0, 0, 0);
    q[1] = vx(x1, y1, z, c0, rep, 0);
    q[2] = vx(x0, y0, z, c1, 0, rep);
    q[3] = vx(x1, y0, z, c1, rep, rep);
    draw_strip(q, 4);
}

/* A floor at y, from z = 0.7 to 30, texture repeated. */
static void floor_quad(float y, float rep_u, float rep_v, DWORD c)
{
    VX q[4];
    q[0] = vx(-4, y, 30, c, 0, 0);
    q[1] = vx(4, y, 30, c, rep_u, 0);
    q[2] = vx(-4, y, 0.7f, c, 0, rep_v);
    q[3] = vx(4, y, 0.7f, c, rep_u, rep_v);
    q[0].ny = q[1].ny = q[2].ny = q[3].ny = 1;
    q[0].nz = q[1].nz = q[2].nz = q[3].nz = 0;
    draw_strip(q, 4);
}

static VX sphere_v[13 * 17];
static WORD sphere_i[12 * 16 * 6];
static int sphere_n;

static void make_sphere(void)
{
    int i, j;
    for (i = 0; i <= 12; i++)
        for (j = 0; j <= 16; j++) {
            float a = 3.14159265f * i / 12, b = 2 * 3.14159265f * j / 16;
            VX *p = &sphere_v[i * 17 + j];
            p->nx = (float)(sin(a) * cos(b)); p->ny = (float)cos(a); p->nz = (float)(sin(a) * sin(b));
            p->x = p->nx; p->y = p->ny; p->z = p->nz;
            p->c = 0xFFFFFFFF;
            p->u = (float)j / 16; p->v = (float)i / 12;
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
    vconst(8, 0, 0, 0, 1);
    IDirect3DDevice9_SetFVF(dev, FVF_VX);
    IDirect3DDevice9_DrawIndexedPrimitiveUP(dev, D3DPT_TRIANGLELIST, 0, 13 * 17, sphere_n / 3, sphere_i, D3DFMT_INDEX16, sphere_v, sizeof(VX));
}

/* -----| textures |----- */

static IDirect3DTexture9 *make_texture(int size, int mips, DWORD (*f)(int, int, int))
{
    IDirect3DTexture9 *t;
    int l, n;
    IDirect3DDevice9_CreateTexture(dev, size, size, mips ? 0 : 1, 0, D3DFMT_A8R8G8B8, D3DPOOL_MANAGED, &t, NULL);
    n = IDirect3DTexture9_GetLevelCount(t);
    for (l = 0; l < n; l++) {
        D3DLOCKED_RECT r;
        int s = size >> l, x, y;
        IDirect3DTexture9_LockRect(t, l, &r, NULL, 0);
        for (y = 0; y < s; y++)
            for (x = 0; x < s; x++)
                ((DWORD *)((char *)r.pBits + y * r.Pitch))[x] = f(x << l, y << l, size);
        IDirect3DTexture9_UnlockRect(t, l);
    }
    return t;
}

static DWORD checker(int x, int y, int n)
{
    int s = n / 8;
    return ((x / s + y / s) & 1) ? 0xFFE8E0D0 : 0xFF305080;
}

/* Mipmap levels in their own colours, so the bias shows. */
static DWORD levels(int x, int y, int n)
{
    (void)y;
    return 0xFF000000 | (DWORD)((x * 255 / n) & 0xF0) << 16 | 0x40A0;
}

static DWORD waves(int x, int y, int n)
{
    float fx = (float)x / n, fy = (float)y / n;
    int r = (int)(128 + 120 * sin(fx * 6.2832 * 2)), g = (int)(128 + 120 * cos(fy * 6.2832 * 3));
    return (DWORD)((x + y) * 255 / (2 * n)) << 24 | (DWORD)r << 16 | (DWORD)g << 8 | 0x60;
}

static DWORD spot(int x, int y, int n)
{
    float dx = (x + 0.5f) / n - 0.5f, dy = (y + 0.5f) / n - 0.5f, d = 1 - (dx * dx + dy * dy) * 6;
    int v = d < 0 ? 0 : (int)(d * 255);
    return 0xFF000000 | (DWORD)v << 16 | (DWORD)(v * 3 / 4) << 8 | (DWORD)(((x / 4 + y / 4) & 1) ? v / 3 : v);
}

static void tex(int s, IDirect3DTexture9 *t)
{
    IDirect3DDevice9_SetTexture(dev, s, (IDirect3DBaseTexture9 *)t);
}

/* -----| the panels |----- */

static IDirect3DTexture9 *t_check, *t_check_mip, *t_levels, *t_waves, *t_spot;

static void p_modulate(void)
{
    panel(0);
    use(VS(vs20_main), PS(ps20_modulate));
    tex(0, t_check_mip);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MIPFILTER, D3DTEXF_LINEAR);
    matrices(identity());
    floor_quad(-1, 8, 24, 0xFFFFE0C0);
}

static void p_light(void)
{
    panel(1);
    use(VS(vs20_main), PS(ps20_light));
    pconst(0, 0.5f, 0.6f, -0.62f, 0);
    pconst(1, 0.8f, 0.6f, 0.4f, 1);
    pconst(2, 0.05f, 0.05f, 0.12f, 1);
    sphere(mul(rot_y(0.4f), translate(0, 0, 3)));
}

static void p_mix(void)
{
    panel(2);
    use(VS(vs20_main), PS(ps20_mix));
    tex(0, t_check);
    tex(1, t_waves);
    pconst(0, 0.4f, 0.8f, 0, 0);
    rs(D3DRS_ALPHABLENDENABLE, TRUE);
    rs(D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
    rs(D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
    matrices(translate(0, 0, 2.5f));
    quad(-0.95f, -0.95f, 0.95f, 0.95f, 0, 0xFFFFFFFF, 0xFFFFFFFF, 1);
}

static void p_kill(void)
{
    panel(3);
    use(VS(vs20_main), PS(ps20_kill));
    matrices(mul(rot_x(0.3f), translate(0, 0, 2.4f)));
    quad(-1, -1, 1, 1, 0, 0xFFFFFFFF, 0xFF8080FF, 1);
}

static void p_dependent(void)
{
    panel(4);
    use(VS(vs20_main), PS(ps20_dependent));
    tex(0, t_check);
    tex(1, t_waves);
    matrices(translate(0, 0, 2.5f));
    quad(-0.95f, -0.95f, 0.95f, 0.95f, 0, 0xFFFFFFFF, 0xFFFFFFFF, 1);
}

static void p_proj(void)
{
    /* Row vectors: q = 0.12 z + 0.6 grows along the floor; s and t are
     * texture positions times q, so texldp's division by q undoes it. */
    M pt;
    panel(5);
    use(VS(vs20_proj), PS(ps20_proj));
    tex(0, t_spot);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSU, D3DTADDRESS_CLAMP);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_ADDRESSV, D3DTADDRESS_CLAMP);
    memset(&pt, 0, sizeof pt);
    pt.m[0 * 4 + 0] = 0.3f; pt.m[2 * 4 + 0] = 0.06f; pt.m[3 * 4 + 0] = 0.3f;
    pt.m[2 * 4 + 1] = 0.074f; pt.m[3 * 4 + 1] = 0.12f;
    pt.m[2 * 4 + 3] = 0.12f; pt.m[3 * 4 + 3] = 0.6f;
    IDirect3DDevice9_SetVertexShaderConstantF(dev, 12, pt.m, 4);
    matrices(identity());
    floor_quad(-1, 1, 1, 0xFFFFFFFF);
}

static void p_bias(void)
{
    panel(6);
    use(VS(vs20_main), PS(ps20_bias));
    tex(0, t_levels);
    IDirect3DDevice9_SetSamplerState(dev, 0, D3DSAMP_MIPFILTER, D3DTEXF_POINT);
    pconst(0, 1.5f, 0, 0, 0);
    matrices(identity());
    floor_quad(-1, 4, 12, 0xFFFFFFFF);
}

static void p_math(void)
{
    panel(7);
    use(VS(vs20_main), PS(ps20_math));
    matrices(translate(0, 0, 2.5f));
    quad(-0.95f, -0.95f, 0.95f, 0.95f, 0, 0xFFFFFFFF, 0xFFFFFFFF, 1);
}

static void p_depth(void)
{
    D3DRECT r;
    panel(8);
    r.x1 = px; r.y1 = py; r.x2 = px + 64; r.y2 = py + 64;
    IDirect3DDevice9_Clear(dev, 1, &r, D3DCLEAR_ZBUFFER, 0, 1, 0);
    use(VS(vs20_main), PS(ps20_depth));
    matrices(translate(0, 0, 2.5f));
    pconst(0, 0.6f, 0.2f, 0, 0);
    quad(-0.9f, -0.5f, 0.9f, 0.9f, 0, 0xFFE04040, 0xFFE0A040, 1);
    pconst(0, -0.6f, 0.75f, 0, 0);
    quad(-0.9f, -0.9f, 0.9f, 0.5f, 0, 0xFF4060E0, 0xFF40C0E0, 1);
}

static void p_fog(void)
{
    panel(9);
    use(VS(vs20_fog), PS(ps20_fog));
    tex(0, t_check);
    rs(D3DRS_FOGENABLE, TRUE);
    rs(D3DRS_FOGCOLOR, 0x00406080);
    rs(D3DRS_FOGVERTEXMODE, D3DFOG_NONE);
    rs(D3DRS_FOGTABLEMODE, D3DFOG_NONE);
    matrices(identity());
    floor_quad(-1, 6, 18, 0xFFFFFFFF);
}

static void p_main30(void)
{
    int n[4] = {3, 0, 1, 0};
    VX q[8];
    int i;
    panel(10);
    use(VS(vs30_main), PS(ps30_main));
    pconst(4, 0.7f, 0.2f, -0.7f, 0); pconst(8, 0.9f, 0.3f, 0.2f, 1);
    pconst(5, -0.7f, 0.2f, -0.7f, 0); pconst(9, 0.2f, 0.8f, 0.3f, 1);
    pconst(6, 0.0f, 0.7f, 0.7f, 0); pconst(10, 0.2f, 0.3f, 0.9f, 1);
    pconst(16, 3, 0, 0, 0);
    IDirect3DDevice9_SetPixelShaderConstantI(dev, 0, n, 1);
    /* A folded strip: both of its sides show. */
    for (i = 0; i < 8; i++) {
        float a = -1.2f + 0.8f * (i / 2) - 0.0f;
        q[i] = vx(a, (i & 1) ? 0.8f : -0.8f, 0.6f * (float)sin(a * 2.2f), 0xFFFFFFFF, 0, 0);
        q[i].nx = -(float)cos(a * 2.2f) * 0.8f;
        q[i].ny = 0;
        q[i].nz = -0.6f;
    }
    matrices(mul(rot_y(0.7f), translate(0, 0, 3)));
    draw_strip(q, 8);
}

static void p_branch30(void)
{
    panel(11);
    use(VS(vs30_main), PS(ps30_branch));
    matrices(translate(0, 0, 2.5f));
    quad(-0.95f, -0.95f, 0.95f, 0.95f, 0, 0xFFFFFFFF, 0xFFFFFFFF, 1);
}

/* -----| checks that print |----- */

static void checks(void)
{
    D3DCAPS9 caps;
    IDirect3DPixelShader9 *s = PS(ps20_math), *got = NULL;
    DWORD vs_only[4] = {0xFFFE0200, 0x0000FFFF, 0, 0}, size = 0;
    float f[4];
    HRESULT hr;
    IDirect3DDevice9_GetDeviceCaps(dev, &caps);
    printf("pixel shaders 2.0 or later: %s\n", caps.PixelShaderVersion >= D3DPS_VERSION(2, 0) ? "yes" : "no");
    printf("pixel shaders 3.0: %s\n", caps.PixelShaderVersion >= D3DPS_VERSION(3, 0) ? "yes" : "no");
    IDirect3DPixelShader9_GetFunction(s, NULL, &size);
    printf("GetFunction size matches: %s\n", size == sizeof ps20_math ? "yes" : "no");
    IDirect3DDevice9_SetPixelShader(dev, s);
    IDirect3DDevice9_GetPixelShader(dev, &got);
    printf("GetPixelShader: %s\n", got == s ? "the same" : "different");
    if (got) IDirect3DPixelShader9_Release(got);
    IDirect3DDevice9_SetPixelShader(dev, NULL);
    pconst(3, 0.25f, -1, 2, 4);
    IDirect3DDevice9_GetPixelShaderConstantF(dev, 3, f, 1);
    printf("pixel constant back: %.2f %.2f %.2f %.2f\n", f[0], f[1], f[2], f[3]);
    hr = IDirect3DDevice9_CreatePixelShader(dev, vs_only, &got);
    printf("a vertex shader as a pixel shader: %s\n", FAILED(hr) ? "refused" : "accepted");
    IDirect3DPixelShader9_Release(s);
}

static int compare(const unsigned char *img)
{
    static const char *names[PANELS] = {"texture times colour", "per-pixel lighting", "two textures mixed", "texkill and cmp", "dependent read", "projected texture",
                                        "bias", "math", "shader depth", "fog after a shader", "ps_3_0 loop, vFace, vPos", "ps_3_0 branches"};
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
    D3DLOCKED_RECT r;
    static unsigned char img[W * H * 3];
    HWND hwnd;
    HRESULT hr;
    int x, y, bad;
    char path[MAX_PATH], *slash;

    setvbuf(stdout, NULL, _IONBF, 0);
    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    hwnd = CreateWindowExA(0, "STATIC", "d3d3", WS_POPUP, 0, 0, W, H, NULL, NULL, NULL, NULL);
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
    t_check = make_texture(32, 0, checker);
    t_check_mip = make_texture(64, 1, checker);
    t_levels = make_texture(64, 1, levels);
    t_waves = make_texture(32, 0, waves);
    t_spot = make_texture(32, 0, spot);
    IDirect3DDevice9_GetRenderTarget(dev, 0, &back);
    IDirect3DDevice9_Clear(dev, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER, 0xFF101010, 1, 0);
    IDirect3DDevice9_BeginScene(dev);
    p_modulate();
    p_light();
    p_mix();
    p_kill();
    p_dependent();
    p_proj();
    p_bias();
    p_math();
    p_depth();
    p_fog();
    p_main30();
    p_branch30();
    IDirect3DDevice9_EndScene(dev);
    use(NULL, NULL);
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
    strcpy(slash ? slash + 1 : path, "d3d3.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    return bad;
}
