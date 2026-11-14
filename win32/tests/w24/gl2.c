/* G5b test: the rest of OpenGL 1.1's fixed function, compared pixel by
 * pixel with Windows' own software OpenGL (its generic implementation).
 *
 * Draws one 192x128 image into a DIB section (PFD_DRAW_TO_BITMAP, so
 * Windows never hands it to a GPU driver), in six 64x64 panels:
 *
 *   0  lighting: two spheres from one display list, a directional and a
 *      point light, specular, colour material
 *   1  fog per pixel (GL_NICEST): linear on a floor, exp on cubes
 *   2  lines and points: thin lines at many slopes, wide lines, a line
 *      loop, polygons drawn as outlines, points of sizes 1 to 4
 *   3  the stencil buffer: shapes counted into it, then drawn through it
 *   4  mipmaps: a trilinear floor (GL_LINEAR_MIPMAP_LINEAR) built and
 *      replayed from display lists (GL_COMPILE_AND_EXECUTE, glCallLists)
 *   5  two-sided lighting and a spot light on two grids, flat shaded
 *
 *   gl2                compare with gl2.ref next to the program; prints the
 *                      same lines on Windows and on VEILOS if they match
 *   gl2 record FILE    write what this machine draws as FILE (a PPM)
 *   gl2 stats          as gl2, with each panel's differences counted
 *
 * Tolerance: in each panel at most 1% of pixels (40) may differ by more
 * than 4 in any channel. (Until G5c also none by more than 64: dropped,
 * since a pixel centre within float error of an edge can fall either way,
 * as Windows' own rounding decides, and so differ completely.)
 *
 * The scenes keep clear of exact ties (a pixel centre exactly on an edge or
 * a line, a point exactly on a pixel boundary): Windows decides those by
 * its float rounding, not by a fixed rule. Fog asks for GL_NICEST: below
 * it Windows approximates, and VEILOS always works fog out per pixel.
 */
#include <windows.h>
#include <GL/gl.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 192
#define H 128

static unsigned char ref[W * H * 3];
static int stats;

/* Start a panel: its viewport, cleared inside a scissor box. */
static void panel(int n, float r, float g, float b)
{
    int x = (n % 3) * 64, y = (1 - n / 3) * 64;
    glViewport(x, y, 64, 64);
    glScissor(x, y, 64, 64);
    glEnable(GL_SCISSOR_TEST);
    glClearColor(r, g, b, 1.0f);
    glClearStencil(0);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    glDisable(GL_SCISSOR_TEST);
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
}

static void frustum(void)
{
    glMatrixMode(GL_PROJECTION);
    glFrustum(-0.5, 0.5, -0.5, 0.5, 1.0, 30.0);
    glMatrixMode(GL_MODELVIEW);
}

static void ortho(void)
{
    glMatrixMode(GL_PROJECTION);
    glOrtho(0, 64, 0, 64, -1, 1);
    glMatrixMode(GL_MODELVIEW);
    /* Off the half-pixel grid: no pixel centre exactly on an edge. */
    glTranslatef(0.13f, 0.07f, 0);
}

static void sphere(int n)
{
    int i, j;
    for (i = 0; i < n; i++) {
        float a0 = 3.14159265f * i / n - 1.5707963f, a1 = 3.14159265f * (i + 1) / n - 1.5707963f;
        glBegin(GL_QUAD_STRIP);
        for (j = 0; j <= 2 * n; j++) {
            float b = 3.14159265f * j / n;
            glNormal3f(cosf(a1) * cosf(b), sinf(a1), cosf(a1) * sinf(b));
            glVertex3f(cosf(a1) * cosf(b), sinf(a1), cosf(a1) * sinf(b));
            glNormal3f(cosf(a0) * cosf(b), sinf(a0), cosf(a0) * sinf(b));
            glVertex3f(cosf(a0) * cosf(b), sinf(a0), cosf(a0) * sinf(b));
        }
        glEnd();
    }
}

static void cube(void)
{
    static const float v[8][3] = {
        {-1, -1, -1}, {1, -1, -1}, {1, 1, -1}, {-1, 1, -1},
        {-1, -1, 1}, {1, -1, 1}, {1, 1, 1}, {-1, 1, 1},
    };
    static const int f[6][4] = {
        {4, 5, 6, 7}, {1, 0, 3, 2}, {5, 1, 2, 6}, {0, 4, 7, 3}, {7, 6, 2, 3}, {0, 1, 5, 4},
    };
    static const float c[6][3] = {
        {1, 0.2f, 0.2f}, {0.2f, 1, 0.2f}, {0.2f, 0.2f, 1}, {1, 1, 0.2f}, {1, 0.2f, 1}, {0.2f, 1, 1},
    };
    int i, k;
    glBegin(GL_QUADS);
    for (i = 0; i < 6; i++) {
        glColor3fv(c[i]);
        for (k = 0; k < 4; k++)
            glVertex3fv(v[f[i][k]]);
    }
    glEnd();
}

static void lighting(GLuint list)
{
    float dir[4] = {0.5f, 0.8f, 1.0f, 0}, pos[4] = {-2.3f, -1.1f, 1.7f, 1};
    float white[4] = {1, 1, 1, 1}, warm[4] = {1, 0.45f, 0.2f, 1}, amb[4] = {0.12f, 0.1f, 0.15f, 1};
    float blue[4] = {0.3f, 0.55f, 0.95f, 1}, spec[4] = {0.9f, 0.9f, 0.9f, 1};

    panel(0, 0.05f, 0.05f, 0.1f);
    frustum();
    glLightfv(GL_LIGHT0, GL_POSITION, dir);
    glLightfv(GL_LIGHT0, GL_DIFFUSE, white);
    glLightfv(GL_LIGHT0, GL_SPECULAR, white);
    glLightfv(GL_LIGHT1, GL_POSITION, pos);
    glLightfv(GL_LIGHT1, GL_DIFFUSE, warm);
    glLightf(GL_LIGHT1, GL_LINEAR_ATTENUATION, 0.25f);
    glLightModelfv(GL_LIGHT_MODEL_AMBIENT, amb);
    glEnable(GL_LIGHTING);
    glEnable(GL_LIGHT0);
    glEnable(GL_LIGHT1);
    glEnable(GL_DEPTH_TEST);
    glEnable(GL_NORMALIZE);
    glMaterialfv(GL_FRONT, GL_DIFFUSE, blue);
    glMaterialfv(GL_FRONT, GL_SPECULAR, spec);
    glMaterialf(GL_FRONT, GL_SHININESS, 28);

    /* The sphere is a display list, compiled without drawing. */
    glNewList(list, GL_COMPILE);
    sphere(14);
    glEndList();

    glTranslatef(-0.55f, 0.3f, -4.1f);
    glPushMatrix();
    glScalef(0.9f, 0.9f, 0.9f);
    glCallList(list);
    glPopMatrix();
    glTranslatef(1.25f, -0.75f, -0.6f);
    glEnable(GL_COLOR_MATERIAL);
    glColorMaterial(GL_FRONT, GL_AMBIENT_AND_DIFFUSE);
    glColor3f(0.95f, 0.7f, 0.2f);
    glCallList(list);
    glDisable(GL_COLOR_MATERIAL);
    glDisable(GL_LIGHTING);
    glDisable(GL_NORMALIZE);
    glDisable(GL_DEPTH_TEST);
}

static void fog(void)
{
    float fc[4] = {0.55f, 0.6f, 0.85f, 1};
    int i;
    panel(1, 0.55f, 0.6f, 0.85f);
    frustum();
    glEnable(GL_FOG);
    glHint(GL_FOG_HINT, GL_NICEST);
    glFogfv(GL_FOG_COLOR, fc);
    glFogi(GL_FOG_MODE, GL_LINEAR);
    glFogf(GL_FOG_START, 2.0f);
    glFogf(GL_FOG_END, 14.0f);
    glEnable(GL_DEPTH_TEST);
    glPushMatrix();
    glTranslatef(0, -0.63f, -2.3f);
    glRotatef(-78, 1, 0, 0);
    glBegin(GL_TRIANGLE_STRIP);
    for (i = 0; i <= 8; i++) {
        glColor3f(i & 1 ? 0.9f : 0.3f, 0.7f, i & 1 ? 0.2f : 0.5f);
        glVertex3f(-2.1f, i * 2.0f - 1, 0);
        glColor3f(0.4f, i & 1 ? 0.3f : 0.9f, 0.3f);
        glVertex3f(2.1f, i * 2.0f - 1, 0);
    }
    glEnd();
    glPopMatrix();
    glFogi(GL_FOG_MODE, GL_EXP);
    glFogf(GL_FOG_DENSITY, 0.17f);
    for (i = 0; i < 3; i++) {
        glPushMatrix();
        glTranslatef(-1.3f + i * 1.4f, 0.35f, -3.0f - i * 2.7f);
        glRotatef(25.0f + i * 20, 0.4f, 1, 0.1f);
        glScalef(0.45f, 0.45f, 0.45f);
        cube();
        glPopMatrix();
    }
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_FOG);
    glHint(GL_FOG_HINT, GL_DONT_CARE);
}

static void lines(void)
{
    int i;
    panel(2, 0.1f, 0.1f, 0.12f);
    ortho();
    glBegin(GL_LINES);
    for (i = 0; i < 20; i++) {
        float a = i * 6.2831853f / 20 + 0.05f;
        glColor3f(1, 0.3f, 0.1f); glVertex2f(20.27f, 43.61f);
        glColor3f(0.2f, 0.9f, 1); glVertex2f(20.27f + 17.3f * cosf(a), 43.61f + 17.3f * sinf(a));
    }
    glEnd();
    glLineWidth(3);
    glBegin(GL_LINES);
    glColor3f(1, 1, 0.2f); glVertex2f(41.3f, 60.2f); glVertex2f(61.2f, 44.7f);
    glColor3f(0.3f, 1, 0.3f); glVertex2f(44.4f, 30.3f); glVertex2f(48.9f, 55.8f);
    glEnd();
    glLineWidth(1);
    glColor3f(1, 1, 1);
    glBegin(GL_LINE_LOOP);
    glVertex2f(3.3f, 3.4f); glVertex2f(24.9f, 6.3f); glVertex2f(15.1f, 22.6f);
    glEnd();
    glPolygonMode(GL_FRONT_AND_BACK, GL_LINE);
    glColor3f(1, 0.5f, 1);
    glBegin(GL_QUADS);
    glVertex2f(28.2f, 3.3f); glVertex2f(42.6f, 5.1f); glVertex2f(40.2f, 20.7f); glVertex2f(27.3f, 18.4f);
    glEnd();
    glPolygonMode(GL_FRONT_AND_BACK, GL_FILL);
    for (i = 1; i <= 4; i++) {
        int k;
        glPointSize((float)i);
        glBegin(GL_POINTS);
        for (k = 0; k < 4; k++) {
            glColor3f(1, 0.25f * i, 0.25f * k);
            glVertex2f(47.2f + k * 4.6f, 3.3f + i * 5.7f + k * 0.37f);
        }
        glEnd();
    }
    glPointSize(1);
}

static void stencil(void)
{
    panel(3, 0.2f, 0.2f, 0.2f);
    ortho();
    glEnable(GL_STENCIL_TEST);
    glColorMask(0, 0, 0, 0);
    glStencilFunc(GL_ALWAYS, 1, 0xFF);
    glStencilOp(GL_KEEP, GL_KEEP, GL_REPLACE);
    glBegin(GL_TRIANGLES);
    glVertex2f(4.3f, 4.1f); glVertex2f(60.6f, 10.3f); glVertex2f(30.1f, 59.7f);
    glEnd();
    glStencilOp(GL_KEEP, GL_KEEP, GL_INCR);
    glRectf(18.2f, 16.3f, 46.1f, 40.2f);
    glStencilFunc(GL_NOTEQUAL, 0, 0xFF);
    glStencilOp(GL_KEEP, GL_KEEP, GL_INVERT);
    glStencilMask(0x0F);
    glRectf(36.2f, 30.3f, 58.1f, 56.2f);
    glStencilMask(0xFF);
    glColorMask(1, 1, 1, 1);
    glStencilOp(GL_KEEP, GL_KEEP, GL_KEEP);
    glStencilFunc(GL_EQUAL, 1, 0xFF);
    glColor3f(1, 0.55f, 0.1f); glRectf(0, 0, 64, 64);
    glStencilFunc(GL_EQUAL, 2, 0xFF);
    glColor3f(0.1f, 0.5f, 1); glRectf(0, 0, 64, 64);
    glStencilFunc(GL_LESS, 2, 0x0F);
    glColor3f(0.9f, 0.9f, 0.3f); glRectf(0, 0, 64, 64);
    glDisable(GL_STENCIL_TEST);
}

static void mipmaps(GLuint tex, GLuint lists)
{
    static const unsigned char lv[7][3] = {{240,60,60},{60,220,60},{60,60,240},{240,230,60},{220,60,220},{60,220,220},{235,235,235}};
    static unsigned char px[64 * 64 * 3];
    static const GLubyte calls[2] = {0, 1};
    int l, s, x, y;

    panel(4, 0.0f, 0.15f, 0.1f);
    /* List 0 of the pair: the texture's levels; list 1: the floor. The first
     * is compiled and run at once; the second only compiled. */
    glNewList(lists, GL_COMPILE_AND_EXECUTE);
    glBindTexture(GL_TEXTURE_2D, tex);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    for (l = 0, s = 64; s >= 1; l++, s /= 2) {
        for (y = 0; y < s; y++)
            for (x = 0; x < s; x++) {
                unsigned char *p = px + (y * s + x) * 3;
                int dark = ((x ^ y) & 1);
                p[0] = lv[l][0] >> dark; p[1] = lv[l][1] >> dark; p[2] = lv[l][2] >> dark;
            }
        glTexImage2D(GL_TEXTURE_2D, l, GL_RGB, s, s, 0, GL_RGB, GL_UNSIGNED_BYTE, px);
    }
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR_MIPMAP_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glEndList();
    glNewList(lists + 1, GL_COMPILE);
    glBegin(GL_QUADS);
    glColor3f(1, 1, 1);
    glTexCoord2f(0, 0); glVertex3f(-2, -1, 0);
    glTexCoord2f(3, 0); glVertex3f(2, -1, 0);
    glTexCoord2f(3, 12); glVertex3f(2, 15, 0);
    glTexCoord2f(0, 12); glVertex3f(-2, 15, 0);
    glEnd();
    glEndList();
    /* Overwritten by the list replay below: proves it happens. */
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST);

    frustum();
    glTranslatef(0, -0.63f, -2.2f);
    glRotatef(-80, 1, 0, 0);
    glEnable(GL_TEXTURE_2D);
    glHint(GL_PERSPECTIVE_CORRECTION_HINT, GL_NICEST);
    glListBase(lists);
    glCallLists(2, GL_UNSIGNED_BYTE, calls);
    glListBase(0);
    glDisable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, 0);
}

static void sheet(void)
{
    float front[4] = {0.95f, 0.35f, 0.3f, 1}, back[4] = {0.3f, 0.45f, 1, 1}, white[4] = {1, 1, 1, 1};
    float pos[4] = {0.1f, 0.9f, -1.6f, 1}, dir[3] = {0, -0.45f, -1}, none[4] = {0, 0, 0, 1};
    float amb[4] = {0.25f, 0.25f, 0.25f, 1};
    int i, j, k;
    panel(5, 0.08f, 0.08f, 0.08f);
    frustum();
    glLightfv(GL_LIGHT0, GL_POSITION, pos);
    glLightfv(GL_LIGHT0, GL_SPOT_DIRECTION, dir);
    glLightf(GL_LIGHT0, GL_SPOT_CUTOFF, 40);
    glLightf(GL_LIGHT0, GL_SPOT_EXPONENT, 3);
    glLightfv(GL_LIGHT0, GL_DIFFUSE, white);
    glLightfv(GL_LIGHT0, GL_SPECULAR, white);
    glLightModelfv(GL_LIGHT_MODEL_AMBIENT, amb);
    glLightModeli(GL_LIGHT_MODEL_TWO_SIDE, 1);
    glLightModeli(GL_LIGHT_MODEL_LOCAL_VIEWER, 1);
    glMaterialfv(GL_FRONT, GL_AMBIENT_AND_DIFFUSE, front);
    glMaterialfv(GL_BACK, GL_AMBIENT_AND_DIFFUSE, back);
    glMaterialfv(GL_FRONT_AND_BACK, GL_SPECULAR, white);
    glMaterialf(GL_FRONT_AND_BACK, GL_SHININESS, 20);
    {
        float d1[4] = {-0.3f, 0.2f, 1, 0}, dim[4] = {0.7f, 0.7f, 0.7f, 1};
        glLightfv(GL_LIGHT1, GL_POSITION, d1);
        glLightfv(GL_LIGHT1, GL_DIFFUSE, dim);
        glLightf(GL_LIGHT1, GL_LINEAR_ATTENUATION, 0);
    }
    glEnable(GL_LIGHTING);
    glEnable(GL_LIGHT0);
    glEnable(GL_LIGHT1);
    glEnable(GL_DEPTH_TEST);
    glShadeModel(GL_FLAT);
    /* Two flat grids with wavy normals (no folds, so no face is ever seen
     * edge-on): one is seen from the front, the other from behind. */
    for (k = 0; k < 2; k++) {
        glPushMatrix();
        glTranslatef(k ? 0.76f : -0.74f, 0.063f, -3.6f);
        glRotatef(k ? 145.0f : -35.0f, 0, 1, 0);
        glRotatef(-20, 1, 0, 0);
        glScalef(0.7f, 0.9f, 1);
        for (i = 0; i < 8; i++) {
            glBegin(GL_QUAD_STRIP);
            for (j = 0; j <= 8; j++) {
                float x = -1 + j * 0.25f, y0 = -1 + i * 0.25f, y1 = y0 + 0.25f;
                glNormal3f(0.5f * sinf(3 * x + 2 * y1), 0.5f * cosf(2 * x - 3 * y1), 1);
                glVertex3f(x, y1, 0);
                glNormal3f(0.5f * sinf(3 * x + 2 * y0), 0.5f * cosf(2 * x - 3 * y0), 1);
                glVertex3f(x, y0, 0);
            }
            glEnd();
        }
        glPopMatrix();
    }
    glShadeModel(GL_SMOOTH);
    glMaterialfv(GL_FRONT_AND_BACK, GL_SPECULAR, none);
    glLightModeli(GL_LIGHT_MODEL_TWO_SIDE, 0);
    glDisable(GL_LIGHT1);
    glDisable(GL_LIGHTING);
    glDisable(GL_DEPTH_TEST);
}

static void draw(GLuint *tex, GLuint lists)
{
    glGenTextures(1, tex);
    lighting(lists + 2);
    fog();
    lines();
    stencil();
    mipmaps(tex[0], lists);
    sheet();
    glFinish();
}

/* Compare `img` (RGB, top row first) with the reference, panel by panel. */
static int compare(const unsigned char *img)
{
    static const char *names[6] = {"lighting", "fog", "lines and points", "stencil", "mipmaps and display lists", "two-sided lighting"};
    int n, bad_panels = 0;
    for (n = 0; n < 6; n++) {
        int x0 = (n % 3) * 64, y0 = (n / 3) * 64, x, y, off = 0, worst = 0, any = 0;
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
        && fgets(hdr, sizeof hdr, f) && strcmp(hdr, "192 128\n") == 0
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
    GLuint tex[1], lists;
    GLint v;
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
    fmt = ChoosePixelFormat(dc, &pfd);
    printf("pixel format: %s\n", fmt > 0 && SetPixelFormat(dc, fmt, &pfd) ? "set" : "FAILED");
    rc = wglCreateContext(dc);
    printf("context: %s\n", rc && wglMakeCurrent(dc, rc) ? "current" : "FAILED");
    if (!rc)
        return 1;
    glGetIntegerv(GL_STENCIL_BITS, &v);
    printf("stencil bits: %s\n", v >= 8 ? "at least 8" : "too few");

    lists = glGenLists(3);
    printf("lists: %s, %s\n", lists ? "generated" : "none", glIsList(lists) ? "known" : "unknown");
    draw(tex, lists);
    printf("errors while drawing: 0x%x\n", glGetError());

    /* GL's errors for the new calls. */
    glNewList(0, GL_COMPILE);
    printf("list 0: 0x%x\n", glGetError());
    glNewList(lists, GL_COMPILE);
    glNewList(lists + 1, GL_COMPILE);
    printf("a list inside a list: 0x%x\n", glGetError());
    glEndList();
    glEndList();
    printf("glEndList without a list: 0x%x\n", glGetError());
    glLightf(GL_LIGHT0, GL_SPOT_CUTOFF, 120);
    printf("a spot cutoff of 120: 0x%x\n", glGetError());
    glStencilOp(GL_KEEP, 0x1234, GL_KEEP);
    printf("a bad stencil operation: 0x%x\n", glGetError());
    glLineWidth(0);
    printf("a line width of 0: 0x%x\n", glGetError());
    glFogi(GL_FOG_MODE, GL_DECAL);
    printf("a bad fog mode: 0x%x\n", glGetError());
    glDeleteLists(lists, 3);
    printf("deleted lists: %s\n", glIsList(lists) || glIsList(lists + 2) ? "still there" : "gone");

    /* The DIB (bottom-up BGRX) as RGB rows, top first. */
    for (y = 0; y < H; y++)
        for (x = 0; x < W; x++) {
            unsigned char *p = bits + ((H - 1 - y) * W + x) * 4, *q = img + (y * W + x) * 3;
            q[0] = p[2]; q[1] = p[1]; q[2] = p[0];
        }

    if (argc > 2 && strcmp(argv[1], "record") == 0) {
        FILE *f = fopen(argv[2], "wb");
        if (!f) {
            printf("can't write %s\n", argv[2]);
            return 1;
        }
        fprintf(f, "P6\n192 128\n255\n");
        fwrite(img, 1, sizeof img, f);
        fclose(f);
        printf("recorded\n");
        return 0;
    }

    GetModuleFileNameA(NULL, path, sizeof path);
    slash = strrchr(path, '\\');
    if (!slash) slash = strrchr(path, '/');
    strcpy(slash ? slash + 1 : path, "gl2.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);

    glDeleteTextures(1, tex);
    wglMakeCurrent(NULL, NULL);
    wglDeleteContext(rc);
    DeleteDC(dc);
    DeleteObject(bmp);
    return bad;
}
