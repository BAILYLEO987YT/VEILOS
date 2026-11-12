/* G5c test: the rest of OpenGL 1.1, compared pixel by pixel with Windows'
 * own software OpenGL (its generic implementation), and the extensions
 * games ask for.
 *
 * Draws 384x128 into a DIB section (PFD_DRAW_TO_BITMAP), in twelve 64x64
 * panels:
 *
 *   0  glDrawPixels: plain, zoomed, blended, floats, scale and bias
 *   1  glBitmap with raster colours and moves
 *   2  glCopyPixels, glCopyTexImage2D, glCopyTexSubImage2D, 1D textures
 *   3  texture coordinate generation: object, eye, sphere map
 *   4  user clip planes on a lit sphere and lines
 *   5  polygon offset: a decal, outlines over a sphere
 *   6  polygon and line stipple
 *   7  logic ops, and the attribute stack
 *   8  the accumulation buffer: a blur of four positions
 *   9  evaluators: a lit Bezier patch (GL_AUTO_NORMAL) and a curve
 *  10  a base texture and a light map: multitexture (GL_ARB_multitexture,
 *      GL_MODULATE) where wglGetProcAddress gives it, else two passes
 *      blended as games do without it; both must look the same
 *  11  texture environments: GL_BLEND with a colour, GL_DECAL, GL_MODULATE
 *
 * and prints what selection, feedback and a few getters and GL's errors
 * give, the same on Windows and on VEILOS.
 *
 *   gl3                compare with gl3.ref next to the program
 *   gl3 record FILE    write what this machine draws as FILE (a PPM)
 *   gl3 stats          as gl3, with each panel's differences counted
 *
 * Tolerance: in each panel at most 1% of pixels (40) may differ by more
 * than 4 in any channel. (Until G5c also none by more than 64: dropped,
 * since a pixel centre within float error of an edge can fall either way,
 * as Windows' own rounding decides, and so differ completely.) The scenes keep
 * clear of exact ties, which Windows decides by its float rounding.
 */
#include <windows.h>
#include <GL/gl.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 384
#define H 128
#define PANELS 12

static unsigned char ref[W * H * 3];
static int stats;

typedef void (APIENTRY *ACTIVE_TEXTURE)(GLenum);
typedef void (APIENTRY *MULTI_TC2F)(GLenum, GLfloat, GLfloat);
#define GL_TEXTURE0_ARB 0x84C0
#define GL_TEXTURE1_ARB 0x84C1

static void panel(int n)
{
    int x = (n % 6) * 64, y = (1 - n / 6) * 64;
    glViewport(x, y, 64, 64);
    glScissor(x, y, 64, 64);
    glEnable(GL_SCISSOR_TEST);
    glClearColor(0.1f, 0.1f, 0.15f, 1);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    glDisable(GL_SCISSOR_TEST);
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
}

/* Window coordinates in this panel: (x, y) of its lower-left corner. */
static int px0(int n) { return (n % 6) * 64; }
static int py0(int n) { return (1 - n / 6) * 64; }

static void ortho(void)
{
    glMatrixMode(GL_PROJECTION); glOrtho(0, 64, 0, 64, -1, 1);
    glMatrixMode(GL_MODELVIEW); glTranslatef(0.13f, 0.07f, 0);
}

static void persp(void)
{
    glMatrixMode(GL_PROJECTION); glFrustum(-0.5, 0.5, -0.5, 0.5, 1, 20);
    glMatrixMode(GL_MODELVIEW);
}

static void sphere(int n)
{
    int i, j;
    for (i = 0; i < n; i++) {
        float a0 = 3.14159265f * i / n - 1.5707963f, a1 = 3.14159265f * (i + 1) / n - 1.5707963f;
        glBegin(GL_QUAD_STRIP);
        for (j = 0; j <= 2 * n; j++) {
            float b = 3.14159265f * j / n;
            glNormal3f(cosf(a1) * cosf(b), sinf(a1), cosf(a1) * sinf(b)); glVertex3f(cosf(a1) * cosf(b), sinf(a1), cosf(a1) * sinf(b));
            glNormal3f(cosf(a0) * cosf(b), sinf(a0), cosf(a0) * sinf(b)); glVertex3f(cosf(a0) * cosf(b), sinf(a0), cosf(a0) * sinf(b));
        }
        glEnd();
    }
}

static void checker(GLuint tex, int size, GLint filter)
{
    static unsigned char px[64 * 64 * 3];
    int x, y;
    for (y = 0; y < size; y++)
        for (x = 0; x < size; x++) {
            unsigned char *p = px + (y * size + x) * 3;
            int c = (x ^ y) & 1;
            p[0] = c ? 230 : 40; p[1] = c ? 200 : 90; p[2] = c ? 60 : 220;
        }
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, size, size, 0, GL_RGB, GL_UNSIGNED_BYTE, px);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
}

static void quad(float x0, float y0, float x1, float y1)
{
    glBegin(GL_QUADS);
    glTexCoord2f(0, 0); glVertex2f(x0, y0);
    glTexCoord2f(1, 0); glVertex2f(x1, y0);
    glTexCoord2f(1, 1); glVertex2f(x1, y1);
    glTexCoord2f(0, 1); glVertex2f(x0, y1);
    glEnd();
}

static void draw_pixels(void)
{
    static unsigned char img[20 * 12 * 4];
    static float fimg[8 * 8 * 3];
    int x, y, i;
    for (y = 0; y < 12; y++)
        for (x = 0; x < 20; x++) {
            unsigned char *p = img + (y * 20 + x) * 4;
            p[0] = (unsigned char)(x * 12); p[1] = (unsigned char)(y * 20); p[2] = (unsigned char)((x ^ y) * 9); p[3] = (unsigned char)(x * 12);
        }
    for (i = 0; i < 64; i++) { fimg[i * 3] = (i % 8) / 7.0f; fimg[i * 3 + 1] = 0.5f; fimg[i * 3 + 2] = (i / 8) / 7.0f; }
    panel(0);
    ortho();
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glRasterPos2i(3, 3); glDrawPixels(20, 12, GL_RGBA, GL_UNSIGNED_BYTE, img);
    glRasterPos2i(30, 3); glPixelZoom(1.5f, 2.0f); glDrawPixels(20, 12, GL_RGB, GL_UNSIGNED_BYTE, img); glPixelZoom(1, 1);
    glEnable(GL_BLEND); glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    glColor3f(0.9f, 0.9f, 0.9f); glRectf(2, 30, 40, 60);
    glRasterPos2i(10, 36); glDrawPixels(20, 12, GL_BGRA_EXT, GL_UNSIGNED_BYTE, img);
    glDisable(GL_BLEND);
    glRasterPos2i(48, 40); glDrawPixels(8, 8, GL_RGB, GL_FLOAT, fimg);
    glPixelTransferf(GL_RED_SCALE, 0.5f); glPixelTransferf(GL_BLUE_BIAS, 0.3f);
    glRasterPos2i(48, 52); glDrawPixels(8, 8, GL_RGB, GL_FLOAT, fimg);
    glPixelTransferf(GL_RED_SCALE, 1); glPixelTransferf(GL_BLUE_BIAS, 0);
}

static void bitmaps(void)
{
    static const unsigned char glyph[] = {0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00, 0x7C, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00, 0x00};
    int i, x;
    panel(1);
    ortho();
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    for (i = 0; i < 6; i++) {
        glColor3f(1, 0.2f * i, 1 - 0.15f * i);
        glRasterPos2f(3.3f, 5.6f + i * 9.1f);
        for (x = 0; x < 5; x++) glBitmap(8, 8, 0.4f, 0.0f, 9.7f + i * 0.3f, 0.6f, glyph + (x & 1) * 8);
    }
}

static void copies(GLuint *tex)
{
    static unsigned char ramp[16 * 3];
    int i;
    for (i = 0; i < 16; i++) { ramp[i * 3] = (unsigned char)(i * 16); ramp[i * 3 + 1] = (unsigned char)(255 - i * 16); ramp[i * 3 + 2] = (unsigned char)((i & 3) * 80); }
    panel(2);
    ortho();
    glBegin(GL_TRIANGLES); glColor3f(1, 0, 0); glVertex2f(2, 2); glColor3f(0, 1, 0); glVertex2f(30, 4); glColor3f(0, 0, 1); glVertex2f(12, 30); glEnd();
    /* Copies read whole window pixels: from this panel's corner. */
    glRasterPos2i(34, 3); glCopyPixels(px0(2), py0(2), 28, 28, GL_COLOR);
    glBindTexture(GL_TEXTURE_2D, tex[0]);
    glCopyTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, px0(2), py0(2), 32, 32, 0);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST); glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glBindTexture(GL_TEXTURE_1D, tex[1]);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage1D(GL_TEXTURE_1D, 0, GL_RGB, 16, 0, GL_RGB, GL_UNSIGNED_BYTE, ramp);
    glTexParameteri(GL_TEXTURE_1D, GL_TEXTURE_MIN_FILTER, GL_LINEAR); glTexParameteri(GL_TEXTURE_1D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glEnable(GL_TEXTURE_1D);
    glBegin(GL_QUADS); glTexCoord1f(0); glVertex2f(2, 36); glTexCoord1f(1); glVertex2f(30, 38); glTexCoord1f(1); glVertex2f(29, 60); glTexCoord1f(0); glVertex2f(3, 58); glEnd();
    glDisable(GL_TEXTURE_1D);
    glBindTexture(GL_TEXTURE_2D, tex[0]);
    glCopyTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, px0(2) + 4, py0(2) + 40, 12, 12);
    glEnable(GL_TEXTURE_2D); glColor3f(1, 1, 1);
    glBegin(GL_QUADS); glTexCoord2f(0, 0); glVertex2f(34, 34); glTexCoord2f(1, 0); glVertex2f(62, 36); glTexCoord2f(1, 1); glVertex2f(60, 62); glTexCoord2f(0, 1); glVertex2f(36, 60); glEnd();
    glDisable(GL_TEXTURE_2D);
}

static void texgen(GLuint tex)
{
    float sp[4] = {0.5f, 0, 0, 0.5f}, tp[4] = {0, 0.5f, 0.2f, 0.5f};
    panel(3);
    persp(); glEnable(GL_TEXTURE_2D); checker(tex, 8, GL_NEAREST); glEnable(GL_DEPTH_TEST);
    glTexGeni(GL_S, GL_TEXTURE_GEN_MODE, GL_OBJECT_LINEAR); glTexGeni(GL_T, GL_TEXTURE_GEN_MODE, GL_OBJECT_LINEAR);
    glTexGenfv(GL_S, GL_OBJECT_PLANE, sp); glTexGenfv(GL_T, GL_OBJECT_PLANE, tp);
    glEnable(GL_TEXTURE_GEN_S); glEnable(GL_TEXTURE_GEN_T);
    glHint(GL_PERSPECTIVE_CORRECTION_HINT, GL_NICEST);
    glTranslatef(-1.1f, 0.6f, -4.3f); glRotatef(30, 0.3f, 1, 0);
    glBegin(GL_QUADS); glColor3f(1, 1, 1); glVertex3f(-0.8f, -0.8f, 0); glVertex3f(0.8f, -0.8f, 0); glVertex3f(0.8f, 0.8f, 0); glVertex3f(-0.8f, 0.8f, 0); glEnd();
    glLoadIdentity();
    glTexGeni(GL_S, GL_TEXTURE_GEN_MODE, GL_EYE_LINEAR); glTexGeni(GL_T, GL_TEXTURE_GEN_MODE, GL_EYE_LINEAR);
    glTexGenfv(GL_S, GL_EYE_PLANE, sp); glTexGenfv(GL_T, GL_EYE_PLANE, tp);
    glTranslatef(1.0f, 0.7f, -4.6f); glRotatef(-25, 1, 0.4f, 0);
    glBegin(GL_QUADS); glVertex3f(-0.7f, -0.7f, 0); glVertex3f(0.7f, -0.7f, 0); glVertex3f(0.7f, 0.7f, 0); glVertex3f(-0.7f, 0.7f, 0); glEnd();
    glLoadIdentity();
    glTexGeni(GL_S, GL_TEXTURE_GEN_MODE, GL_SPHERE_MAP); glTexGeni(GL_T, GL_TEXTURE_GEN_MODE, GL_SPHERE_MAP);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR); glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTranslatef(0.05f, -0.9f, -4.0f); glScalef(0.9f, 0.9f, 0.9f); sphere(12);
    glDisable(GL_TEXTURE_GEN_S); glDisable(GL_TEXTURE_GEN_T); glDisable(GL_TEXTURE_2D); glDisable(GL_DEPTH_TEST);
    glHint(GL_PERSPECTIVE_CORRECTION_HINT, GL_DONT_CARE);
}

static void clip_planes(void)
{
    double p0[4] = {1, 0.3, 0, 0.2}, p1[4] = {-0.2, 1, 0.5, 0.35};
    float lp[4] = {0.3f, 0.6f, 1, 0};
    int i;
    panel(4);
    persp(); glEnable(GL_DEPTH_TEST); glEnable(GL_LIGHTING); glEnable(GL_LIGHT0); glLightfv(GL_LIGHT0, GL_POSITION, lp);
    glTranslatef(0, 0, -3.5f);
    glClipPlane(GL_CLIP_PLANE0, p0); glClipPlane(GL_CLIP_PLANE1, p1); glEnable(GL_CLIP_PLANE0); glEnable(GL_CLIP_PLANE1);
    glRotatef(20, 1, 1, 0); sphere(14);
    glDisable(GL_LIGHTING); glDisable(GL_CLIP_PLANE1);
    glBegin(GL_LINES); glColor3f(1, 1, 0); for (i = 0; i < 12; i++) { glVertex3f(-1.4f + i * 0.25f, -1.3f, 0.2f); glVertex3f(-1.2f + i * 0.2f, 1.3f, -0.1f); } glEnd();
    glDisable(GL_CLIP_PLANE0); glDisable(GL_DEPTH_TEST); glDisable(GL_LIGHT0);
}

static void polygon_offset(void)
{
    panel(5);
    persp(); glEnable(GL_DEPTH_TEST); glTranslatef(0, 0, -4); glRotatef(-60, 1, 0, 0);
    glColor3f(0.3f, 0.6f, 0.3f); glBegin(GL_QUADS); glVertex3f(-1.5f, -1.5f, 0); glVertex3f(1.5f, -1.5f, 0); glVertex3f(1.5f, 1.5f, 0); glVertex3f(-1.5f, 1.5f, 0); glEnd();
    glEnable(GL_POLYGON_OFFSET_FILL); glPolygonOffset(-1, -2);
    glColor3f(0.9f, 0.2f, 0.2f); glBegin(GL_TRIANGLES); glVertex3f(-1, -1, 0); glVertex3f(1, -0.8f, 0); glVertex3f(0, 1.2f, 0); glEnd();
    glDisable(GL_POLYGON_OFFSET_FILL);
    glLoadIdentity(); glTranslatef(0.2f, 0.3f, -3.6f);
    glColor3f(0.2f, 0.3f, 0.8f); sphere(8);
    glEnable(GL_POLYGON_OFFSET_LINE); glPolygonOffset(-1, -1); glPolygonMode(GL_FRONT_AND_BACK, GL_LINE);
    glColor3f(1, 1, 1); sphere(8);
    glPolygonMode(GL_FRONT_AND_BACK, GL_FILL); glDisable(GL_POLYGON_OFFSET_LINE); glDisable(GL_DEPTH_TEST);
}

static void stipples(void)
{
    static unsigned char pat[128];
    int i;
    for (i = 0; i < 128; i++) pat[i] = (unsigned char)((i / 4) % 2 ? 0xAA : 0xCC) ^ (unsigned char)(i * 7);
    panel(6);
    ortho(); glPolygonStipple(pat); glEnable(GL_POLYGON_STIPPLE);
    glColor3f(1, 0.6f, 0.2f); glRectf(3.3f, 3.2f, 40.1f, 35.4f);
    glDisable(GL_POLYGON_STIPPLE);
    glEnable(GL_LINE_STIPPLE);
    glLineStipple(1, 0xF0F0); glColor3f(0.4f, 1, 0.4f);
    glBegin(GL_LINES); glVertex2f(4.4f, 45.3f); glVertex2f(60.1f, 47.6f); glEnd();
    glLineStipple(3, 0x8F3C); glColor3f(0.4f, 0.6f, 1);
    glBegin(GL_LINE_STRIP); glVertex2f(44.3f, 3.6f); glVertex2f(60.2f, 20.4f); glVertex2f(48.2f, 38.7f); glVertex2f(58.4f, 58.1f); glEnd();
    glLineStipple(2, 0xAAAA); glLineWidth(3); glColor3f(1, 1, 1);
    glBegin(GL_LINES); glVertex2f(6.2f, 55.4f); glVertex2f(40.1f, 60.3f); glEnd();
    glLineWidth(1); glDisable(GL_LINE_STIPPLE);
}

static void logic_and_attrib(void)
{
    panel(7);
    ortho();
    glColor3f(0.8f, 0.3f, 0.5f); glRectf(3.3f, 3.2f, 45.1f, 45.4f);
    glPushAttrib(GL_ENABLE_BIT | GL_COLOR_BUFFER_BIT | GL_CURRENT_BIT);
    glEnable(GL_COLOR_LOGIC_OP);
    glLogicOp(GL_XOR); glColor3f(1, 1, 0); glRectf(20.2f, 20.1f, 60.3f, 60.4f);
    glLogicOp(GL_INVERT); glRectf(8.2f, 30.1f, 30.3f, 40.4f);
    glLogicOp(GL_AND); glColor3f(0.5f, 1, 0.25f); glRectf(35.2f, 4.1f, 60.3f, 18.4f);
    glPopAttrib();
    /* Logic ops off again, the colour back to before the push. */
    glRectf(48.2f, 46.1f, 60.3f, 58.4f);
}

static void accumulation(void)
{
    int i;
    panel(8);
    ortho();
    glClearAccum(0, 0, 0, 0);
    glEnable(GL_SCISSOR_TEST);
    glScissor(px0(8), py0(8), 64, 64);
    glClear(GL_ACCUM_BUFFER_BIT);
    for (i = 0; i < 4; i++) {
        glClear(GL_COLOR_BUFFER_BIT);
        glBegin(GL_TRIANGLES); glColor3f(1, 0.5f, 0.1f); glVertex2f(5.0f + i * 4, 5); glColor3f(0.2f, 1, 0.3f); glVertex2f(40.0f + i * 4, 12); glColor3f(0.3f, 0.4f, 1); glVertex2f(18.0f + i * 4, 55); glEnd();
        glAccum(GL_ACCUM, 0.25f);
    }
    glAccum(GL_RETURN, 1.0f);
    glDisable(GL_SCISSOR_TEST);
}

static void evaluators(void)
{
    static const float cp[4][4][3] = {
        {{-1.5f, -1.5f, 0}, {-0.5f, -1.5f, 0.6f}, {0.5f, -1.5f, -0.4f}, {1.5f, -1.5f, 0.2f}},
        {{-1.5f, -0.5f, 0.5f}, {-0.5f, -0.5f, 1.2f}, {0.5f, -0.5f, 0.3f}, {1.5f, -0.5f, -0.6f}},
        {{-1.5f, 0.5f, -0.3f}, {-0.5f, 0.5f, 0.4f}, {0.5f, 0.5f, 1.0f}, {1.5f, 0.5f, 0.3f}},
        {{-1.5f, 1.5f, 0.2f}, {-0.5f, 1.5f, -0.6f}, {0.5f, 1.5f, 0.2f}, {1.5f, 1.5f, 0.5f}}};
    static const float curve[4][3] = {{-1.6f, -1.2f, 0.5f}, {-0.5f, 1.6f, 0.5f}, {0.6f, -1.6f, 0.5f}, {1.6f, 1.3f, 0.5f}};
    float lp[4] = {0.4f, 0.6f, 1, 0};
    int i;
    panel(9);
    persp(); glEnable(GL_DEPTH_TEST); glTranslatef(0, 0, -4.5f); glRotatef(-35, 1, 0.2f, 0);
    glMap2f(GL_MAP2_VERTEX_3, 0, 1, 3, 4, 0, 1, 12, 4, &cp[0][0][0]);
    glEnable(GL_MAP2_VERTEX_3); glEnable(GL_AUTO_NORMAL); glEnable(GL_LIGHTING); glEnable(GL_LIGHT0); glLightfv(GL_LIGHT0, GL_POSITION, lp);
    glMapGrid2f(10, 0, 1, 10, 0, 1); glEvalMesh2(GL_FILL, 0, 10, 0, 10);
    glDisable(GL_LIGHTING); glDisable(GL_LIGHT0);
    glMap1f(GL_MAP1_VERTEX_3, 0, 1, 3, 4, &curve[0][0]); glEnable(GL_MAP1_VERTEX_3);
    glColor3f(1, 1, 0); glBegin(GL_LINE_STRIP); for (i = 0; i <= 30; i++) glEvalCoord1f(i / 30.0f); glEnd();
    glDisable(GL_MAP1_VERTEX_3); glDisable(GL_MAP2_VERTEX_3); glDisable(GL_AUTO_NORMAL); glDisable(GL_DEPTH_TEST);
}

static void light_map(GLuint *tex)
{
    static unsigned char lm[8 * 8 * 3];
    ACTIVE_TEXTURE active = (ACTIVE_TEXTURE)wglGetProcAddress("glActiveTextureARB");
    MULTI_TC2F mtc = (MULTI_TC2F)wglGetProcAddress("glMultiTexCoord2fARB");
    int i;
    for (i = 0; i < 64; i++) {
        float dx = (i % 8) - 3.5f, dy = (i / 8) - 3.5f;
        unsigned char v = (unsigned char)(255 - (dx * dx + dy * dy) * 9);
        lm[i * 3] = v; lm[i * 3 + 1] = (unsigned char)(v * 0.9f); lm[i * 3 + 2] = (unsigned char)(v * 0.6f);
    }
    panel(10);
    ortho();
    checker(tex[2], 8, GL_NEAREST);
    glBindTexture(GL_TEXTURE_2D, tex[3]);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, 8, 8, 0, GL_RGB, GL_UNSIGNED_BYTE, lm);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR); glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glColor3f(1, 1, 1);
    if (active && mtc) {
        /* One pass: the light map on unit 1, modulating. */
        active(GL_TEXTURE0_ARB); glEnable(GL_TEXTURE_2D); glBindTexture(GL_TEXTURE_2D, tex[2]);
        active(GL_TEXTURE1_ARB); glEnable(GL_TEXTURE_2D); glBindTexture(GL_TEXTURE_2D, tex[3]);
        glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
        glBegin(GL_QUADS);
        mtc(GL_TEXTURE0_ARB, 0, 0); mtc(GL_TEXTURE1_ARB, 0, 0); glVertex2f(4, 4);
        mtc(GL_TEXTURE0_ARB, 2, 0); mtc(GL_TEXTURE1_ARB, 1, 0); glVertex2f(60, 4);
        mtc(GL_TEXTURE0_ARB, 2, 2); mtc(GL_TEXTURE1_ARB, 1, 1); glVertex2f(60, 60);
        mtc(GL_TEXTURE0_ARB, 0, 2); mtc(GL_TEXTURE1_ARB, 0, 1); glVertex2f(4, 60);
        glEnd();
        glDisable(GL_TEXTURE_2D);
        active(GL_TEXTURE0_ARB); glDisable(GL_TEXTURE_2D);
    } else {
        /* Two passes: the base, then the light map multiplied in. */
        glEnable(GL_TEXTURE_2D);
        glBindTexture(GL_TEXTURE_2D, tex[2]);
        glBegin(GL_QUADS); glTexCoord2f(0, 0); glVertex2f(4, 4); glTexCoord2f(2, 0); glVertex2f(60, 4); glTexCoord2f(2, 2); glVertex2f(60, 60); glTexCoord2f(0, 2); glVertex2f(4, 60); glEnd();
        glBindTexture(GL_TEXTURE_2D, tex[3]);
        glEnable(GL_BLEND); glBlendFunc(GL_DST_COLOR, GL_ZERO);
        glBegin(GL_QUADS); glTexCoord2f(0, 0); glVertex2f(4, 4); glTexCoord2f(1, 0); glVertex2f(60, 4); glTexCoord2f(1, 1); glVertex2f(60, 60); glTexCoord2f(0, 1); glVertex2f(4, 60); glEnd();
        glDisable(GL_BLEND); glDisable(GL_TEXTURE_2D);
    }
}

static void environments(GLuint tex)
{
    static unsigned char rgba[8 * 8 * 4];
    float envc[4] = {0.2f, 0.9f, 0.4f, 1};
    int i;
    for (i = 0; i < 64; i++) { rgba[i * 4] = (unsigned char)((i % 8) * 36); rgba[i * 4 + 1] = 120; rgba[i * 4 + 2] = (unsigned char)((i / 8) * 36); rgba[i * 4 + 3] = (unsigned char)(((i % 8) + (i / 8)) * 18); }
    panel(11);
    ortho(); glEnable(GL_TEXTURE_2D); glBindTexture(GL_TEXTURE_2D, tex); glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA, 8, 8, 0, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST); glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_BLEND); glTexEnvfv(GL_TEXTURE_ENV, GL_TEXTURE_ENV_COLOR, envc);
    glColor3f(0.9f, 0.3f, 0.5f);
    quad(3, 3, 30, 30);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_DECAL);
    quad(34, 3, 61, 30);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
    quad(3, 34, 30, 61);
    glDisable(GL_TEXTURE_2D);
}

/* Selection: which of three named squares a pick region hits. */
static void selection(void)
{
    GLuint buf[64];
    GLint hits, i, k = 0;
    glSelectBuffer(64, buf);
    glRenderMode(GL_SELECT);
    glMatrixMode(GL_PROJECTION); glLoadIdentity(); glOrtho(0, 10, 0, 10, -1, 1);
    glMatrixMode(GL_MODELVIEW); glLoadIdentity();
    glInitNames();
    glPushName(0);
    for (i = 1; i <= 3; i++) {
        glLoadName(i);
        glBegin(GL_QUADS);
        glVertex3f(i * 2.0f, 2, -0.1f * i); glVertex3f(i * 2.0f + 3, 2, -0.1f * i); glVertex3f(i * 2.0f + 3, 6, -0.1f * i); glVertex3f(i * 2.0f, 6, -0.1f * i);
        glEnd();
    }
    glLoadName(9);
    glBegin(GL_TRIANGLES); glVertex2f(20, 20); glVertex2f(21, 20); glVertex2f(20, 21); glEnd();
    hits = glRenderMode(GL_RENDER);
    printf("selection: %d hits", hits);
    for (i = 0; i < hits; i++) {
        GLuint n = buf[k];
        printf(" [%u names, z %.2f-%.2f, name %u]", n, buf[k + 1] / 4294967295.0, buf[k + 2] / 4294967295.0, buf[k + 3]);
        k += 3 + n;
    }
    printf("\n");
}

/* Feedback: a triangle in 3D with colour, clipped by the view volume. */
static void feedback(void)
{
    GLfloat buf[128];
    GLint n, i;
    glFeedbackBuffer(128, GL_3D_COLOR, buf);
    glRenderMode(GL_FEEDBACK);
    glMatrixMode(GL_PROJECTION); glLoadIdentity(); glOrtho(0, 64, 0, 64, -1, 1);
    glMatrixMode(GL_MODELVIEW); glLoadIdentity();
    glViewport(0, 0, 64, 64);
    glPassThrough(7);
    glBegin(GL_TRIANGLES);
    glColor3f(1, 0, 0); glVertex2f(10.5f, 10.25f);
    glColor3f(0, 1, 0); glVertex2f(50.5f, 20.75f);
    glColor3f(0, 0, 1); glVertex2f(20.25f, 50.5f);
    glEnd();
    glBegin(GL_POINTS); glColor3f(1, 1, 1); glVertex2f(5.5f, 6.5f); glVertex2f(-5, 6); glEnd();
    n = glRenderMode(GL_RENDER);
    printf("feedback: %d values:", n);
    for (i = 0; i < n; i++)
        printf(" %.2f", buf[i]);
    printf("\n");
}

static void draw(GLuint *tex)
{
    glGenTextures(5, tex);
    draw_pixels();
    bitmaps();
    copies(tex);
    texgen(tex[2]);
    clip_planes();
    polygon_offset();
    stipples();
    logic_and_attrib();
    accumulation();
    evaluators();
    light_map(tex);
    environments(tex[4]);
    glFinish();
}

static int compare(const unsigned char *img)
{
    static const char *names[PANELS] = {"draw pixels", "bitmaps", "copies and 1D textures", "texgen", "clip planes", "polygon offset",
                                        "stipples", "logic ops and attributes", "accumulation", "evaluators", "light map", "texture environments"};
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
                if (d > 4) off++;
                if (d > worst) worst = d;
            }
        if (off <= 40) {
            printf("panel %d (%s): ok\n", n, names[n]);
        } else {
            printf("panel %d (%s): FAIL\n", n, names[n]);
            bad_panels++;
        }
        if (stats)
            printf("  %d pixels differ, %d by more than 4, largest difference %d\n", any, off, worst);
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
    BITMAPINFO bi;
    PIXELFORMATDESCRIPTOR pfd;
    unsigned char *bits;
    static unsigned char img[W * H * 3];
    HDC dc;
    HBITMAP bmp;
    HGLRC rc;
    GLuint tex[5];
    GLint v[4];
    GLfloat f[4];
    int fmt, x, y, bad;
    char path[MAX_PATH], *slash;

    stats = argc > 1 && strcmp(argv[1], "stats") == 0;
    memset(&bi, 0, sizeof bi);
    bi.bmiHeader.biSize = sizeof bi.bmiHeader;
    bi.bmiHeader.biWidth = W;
    bi.bmiHeader.biHeight = H;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    dc = CreateCompatibleDC(NULL);
    bmp = CreateDIBSection(dc, &bi, DIB_RGB_COLORS, (void **)&bits, NULL, 0);
    SelectObject(dc, bmp);
    memset(&pfd, 0, sizeof pfd);
    pfd.nSize = sizeof pfd;
    pfd.nVersion = 1;
    pfd.dwFlags = PFD_DRAW_TO_BITMAP | PFD_SUPPORT_OPENGL | PFD_SUPPORT_GDI;
    pfd.iPixelType = PFD_TYPE_RGBA;
    pfd.cColorBits = 32;
    pfd.cDepthBits = 24;
    pfd.cStencilBits = 8;
    pfd.cAccumBits = 64;
    fmt = ChoosePixelFormat(dc, &pfd);
    printf("pixel format: %s\n", fmt > 0 && SetPixelFormat(dc, fmt, &pfd) ? "set" : "FAILED");
    rc = wglCreateContext(dc);
    printf("context: %s\n", rc && wglMakeCurrent(dc, rc) ? "current" : "FAILED");
    if (!rc)
        return 1;
    glGetIntegerv(GL_ACCUM_RED_BITS, v);
    printf("accumulation bits: %s\n", v[0] >= 8 ? "enough" : "too few");

    draw(tex);
    printf("errors while drawing: 0x%x\n", glGetError());

    /* State read back. */
    glGetIntegerv(GL_ATTRIB_STACK_DEPTH, v);
    printf("attribute stack depth: %d\n", v[0]);
    glPushAttrib(GL_POLYGON_BIT); glPolygonOffset(2.5f, 3); glGetFloatv(GL_POLYGON_OFFSET_FACTOR, f); glPopAttrib();
    printf("polygon offset factor while pushed: %.1f\n", f[0]);
    glGetFloatv(GL_POLYGON_OFFSET_FACTOR, f);
    printf("after the pop: %.1f\n", f[0]);
    glLineStipple(5, 0x1234); glGetIntegerv(GL_LINE_STIPPLE_PATTERN, v); glGetIntegerv(GL_LINE_STIPPLE_REPEAT, v + 1);
    printf("line stipple: 0x%x x %d\n", v[0], v[1]);
    glBindTexture(GL_TEXTURE_2D, tex[3]);
    glGetTexLevelParameteriv(GL_TEXTURE_2D, 0, GL_TEXTURE_WIDTH, v);
    glGetTexParameteriv(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, v + 1);
    printf("light map: %d wide, min filter 0x%x\n", v[0], v[1]);
    glRasterPos2f(0, 0);
    glGetBooleanv(GL_CURRENT_RASTER_POSITION_VALID, (GLboolean *)v);
    printf("raster position valid: %d\n", ((GLboolean *)v)[0]);
    selection();
    feedback();

    /* Errors. */
    glPopAttrib();
    printf("popping an empty attribute stack: 0x%x\n", glGetError());
    glTexGeni(GL_R, GL_TEXTURE_GEN_MODE, GL_SPHERE_MAP);
    printf("a sphere map for r: 0x%x\n", glGetError());
    glDrawPixels(4, 4, GL_RGB, 0x1234, img);
    printf("a bad pixel type: 0x%x\n", glGetError());
    glLogicOp(0x1234);
    printf("a bad logic op: 0x%x\n", glGetError());
    glRenderMode(0x1234);
    printf("a bad render mode: 0x%x\n", glGetError());
    printf("no error left: 0x%x\n", glGetError());

    for (y = 0; y < H; y++)
        for (x = 0; x < W; x++) {
            unsigned char *p = bits + ((H - 1 - y) * W + x) * 4, *q = img + (y * W + x) * 3;
            q[0] = p[2]; q[1] = p[1]; q[2] = p[0];
        }
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
    strcpy(slash ? slash + 1 : path, "gl3.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);
    glDeleteTextures(5, tex);
    wglMakeCurrent(NULL, NULL);
    wglDeleteContext(rc);
    DeleteDC(dc);
    DeleteObject(bmp);
    return bad;
}
