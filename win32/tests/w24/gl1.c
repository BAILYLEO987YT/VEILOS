/* G5 test: OpenGL 1.1 fixed function, compared pixel by pixel with
 * Windows' own software OpenGL (its generic implementation).
 *
 * Draws one 128x128 image into a DIB section (PFD_DRAW_TO_BITMAP, which only
 * the generic implementation supports, so Windows never hands it to a GPU
 * driver), in four 64x64 panels:
 *
 *   0  a smooth-shaded triangle and a flat-shaded quad, orthographic
 *   1  a cube with depth test and back-face culling, cut by a quad
 *   2  textures: a checkerboard in perspective (nearest, GL_NICEST), a
 *      small texture magnified (linear), decal and replace
 *   3  blending and alpha test, drawn with vertex arrays
 *
 *   gl1                compare with gl1.ref next to the program; prints the
 *                      same lines on Windows and on VEILOS if they match
 *   gl1 record FILE    write what this machine draws as FILE (a PPM)
 *   gl1 stats          as gl1, with each panel's differences counted
 *   gl1 wait           a window with a spinning textured cube, until closed
 *   gl1 bench [N]      draw N (default 20) 512x512 frames of textured,
 *                      depth-tested triangles and print the time each took
 *
 * Tolerance: in each panel at most 1% of pixels (40) may differ by more
 * than 4 in any channel, and none by more than 64.
 *
 * The scenes keep clear of exact ties (a pixel centre exactly on an edge,
 * a texel boundary within float error of a pixel centre): Windows decides
 * those by its float rounding, not by a fixed rule.
 */
#include <windows.h>
#include <GL/gl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 128
#define H 128

static unsigned char ref[W * H * 3];
static int stats;

static void checker(GLuint tex, int size, unsigned char a[3], unsigned char b[3], GLint filter)
{
    unsigned char px[64 * 64 * 3];
    int x, y;
    for (y = 0; y < size; y++)
        for (x = 0; x < size; x++)
            memcpy(px + (y * size + x) * 3, ((x ^ y) & 1) ? a : b, 3);
    glBindTexture(GL_TEXTURE_2D, tex);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_REPEAT);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_REPEAT);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGB, size, size, 0, GL_RGB, GL_UNSIGNED_BYTE, px);
}

/* Start a panel: its viewport, cleared inside a scissor box. */
static void panel(int n, float r, float g, float b)
{
    int x = (n % 2) * 64, y = (n / 2) * 64;
    glViewport(x, y, 64, 64);
    glScissor(x, y, 64, 64);
    glEnable(GL_SCISSOR_TEST);
    glClearColor(r, g, b, 1.0f);
    glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
    glDisable(GL_SCISSOR_TEST);
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
}

static void cube(void)
{
    static const float v[8][3] = {
        {-1, -1, -1}, {1, -1, -1}, {1, 1, -1}, {-1, 1, -1},
        {-1, -1, 1}, {1, -1, 1}, {1, 1, 1}, {-1, 1, 1},
    };
    /* Counter-clockwise seen from outside. */
    static const int f[6][4] = {
        {4, 5, 6, 7}, {1, 0, 3, 2}, {5, 1, 2, 6}, {0, 4, 7, 3}, {7, 6, 2, 3}, {0, 1, 5, 4},
    };
    static const float c[6][3] = {
        {1, 0, 0}, {0, 1, 0}, {0, 0, 1}, {1, 1, 0}, {1, 0, 1}, {0, 1, 1},
    };
    int i, k;
    glBegin(GL_QUADS);
    for (i = 0; i < 6; i++) {
        glColor3fv(c[i]);
        for (k = 0; k < 4; k++) {
            glTexCoord2f((k == 1 || k == 2) ? 1.0f : 0.0f, k >= 2 ? 1.0f : 0.0f);
            glVertex3fv(v[f[i][k]]);
        }
    }
    glEnd();
}

static void draw(GLuint *tex)
{
    unsigned char red[3] = {230, 40, 40}, white[3] = {250, 250, 250};
    unsigned char blue[3] = {30, 60, 200}, gold[3] = {240, 200, 40};

    glGenTextures(2, tex);
    checker(tex[0], 8, red, white, GL_NEAREST);
    checker(tex[1], 2, blue, gold, GL_LINEAR);
    glBindTexture(GL_TEXTURE_2D, 0);

    /* 0: smooth and flat shading. */
    panel(0, 0.1f, 0.1f, 0.3f);
    glOrtho(0, 1, 0, 1, -1, 1);
    glShadeModel(GL_SMOOTH);
    glBegin(GL_TRIANGLES);
    glColor3f(1, 0, 0); glVertex2f(0.08f, 0.1f);
    glColor3f(0, 1, 0); glVertex2f(0.92f, 0.25f);
    glColor3f(0, 0, 1); glVertex2f(0.4f, 0.93f);
    glEnd();
    glShadeModel(GL_FLAT);
    glBegin(GL_QUADS);
    glColor3f(1, 1, 1); glVertex2f(0.6f, 0.6f);
    glColor3f(1, 0, 1); glVertex2f(0.95f, 0.62f);
    glColor3f(1, 1, 0); glVertex2f(0.9f, 0.95f);
    glColor3f(0.2f, 0.9f, 0.6f); glVertex2f(0.62f, 0.9f);
    glEnd();
    glShadeModel(GL_SMOOTH);

    /* 1: a cube, depth and culling, cut by a quad. */
    panel(1, 0.15f, 0.15f, 0.15f);
    glMatrixMode(GL_PROJECTION);
    glFrustum(-0.5, 0.5, -0.5, 0.5, 1.0, 10.0);
    glMatrixMode(GL_MODELVIEW);
    glTranslatef(0, 0, -4.2f);
    glRotatef(35, 1, 0, 0);
    glRotatef(-40, 0, 1, 0);
    glEnable(GL_DEPTH_TEST);
    glEnable(GL_CULL_FACE);
    cube();
    glDisable(GL_CULL_FACE);
    glBegin(GL_QUADS);
    glColor3f(0.9f, 0.9f, 0.9f); glVertex3f(-1.6f, -0.3f, -1.6f);
    glColor3f(0.5f, 0.5f, 0.5f); glVertex3f(1.6f, 0.4f, -1.6f);
    glColor3f(0.9f, 0.9f, 0.9f); glVertex3f(1.6f, 0.4f, 1.6f);
    glColor3f(0.3f, 0.3f, 0.3f); glVertex3f(-1.6f, -0.3f, 1.6f);
    glEnd();
    glDisable(GL_DEPTH_TEST);

    /* 2: textures, perspective-correct. (Below GL_NICEST, Windows' software
     * OpenGL approximates; VEILOS is always exact.) */
    panel(2, 0.0f, 0.2f, 0.1f);
    glHint(GL_PERSPECTIVE_CORRECTION_HINT, GL_NICEST);
    glMatrixMode(GL_PROJECTION);
    glFrustum(-0.5, 0.5, -0.5, 0.5, 1.0, 20.0);
    glMatrixMode(GL_MODELVIEW);
    glTranslatef(0, -0.63f, -3);
    glRotatef(-70, 1, 0, 0);
    glEnable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, tex[0]);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_MODULATE);
    glBegin(GL_QUADS);
    glColor3f(1, 1, 1); glTexCoord2f(0, 0); glVertex3f(-1.5f, -1.5f, 0);
    glColor3f(1, 1, 1); glTexCoord2f(3, 0); glVertex3f(1.5f, -1.5f, 0);
    glColor3f(1, 1, 0.3f); glTexCoord2f(3, 3); glVertex3f(1.5f, 4.5f, 0);
    glColor3f(1, 1, 0.3f); glTexCoord2f(0, 3); glVertex3f(-1.5f, 4.5f, 0);
    glEnd();
    glLoadIdentity();
    glMatrixMode(GL_PROJECTION);
    glLoadIdentity();
    glOrtho(0, 64, 0, 64, -1, 1);
    glMatrixMode(GL_MODELVIEW);
    glBindTexture(GL_TEXTURE_2D, tex[1]);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_REPLACE);
    glBegin(GL_QUADS);
    glTexCoord2f(0, 0); glVertex2f(4, 40);
    glTexCoord2f(1, 0); glVertex2f(24, 40);
    glTexCoord2f(1, 1); glVertex2f(24, 60);
    glTexCoord2f(0, 1); glVertex2f(4, 60);
    glEnd();
    glBindTexture(GL_TEXTURE_2D, tex[0]);
    glTexEnvi(GL_TEXTURE_ENV, GL_TEXTURE_ENV_MODE, GL_DECAL);
    glColor3f(0.2f, 0.4f, 1.0f);
    glBegin(GL_TRIANGLES);
    glTexCoord2f(0, 0); glVertex2f(36, 42);
    glTexCoord2f(1, 0); glVertex2f(60, 46);
    glTexCoord2f(0.5f, 1); glVertex2f(44, 62);
    glEnd();
    glDisable(GL_TEXTURE_2D);
    glBindTexture(GL_TEXTURE_2D, 0);

    /* 3: blending and the alpha test, from vertex arrays. */
    {
        static const float pos[] = {
            6, 6, 40, 10, 34, 44, 4, 38,      /* quad, opaque orange */
            20, 20, 58, 22, 56, 58, 18, 54,   /* quad, half-transparent blue */
            30, 4, 62, 8, 46, 30,             /* triangle, alpha ramp, tested */
        };
        static const unsigned char col[] = {
            250, 140, 20, 255, 250, 140, 20, 255, 250, 140, 20, 255, 250, 140, 20, 255,
            40, 90, 255, 128, 40, 90, 255, 128, 90, 255, 90, 128, 90, 255, 90, 128,
            255, 255, 255, 0, 255, 255, 255, 255, 255, 255, 255, 160,
        };
        static const unsigned char quads[] = {0, 1, 2, 3, 4, 5, 6, 7};
        panel(3, 0.3f, 0.1f, 0.1f);
        glMatrixMode(GL_PROJECTION);
        glOrtho(0, 64, 0, 64, -1, 1);
        glMatrixMode(GL_MODELVIEW);
        /* Off the half-pixel grid: no pixel centre exactly on an edge. */
        glTranslatef(0.13f, 0.07f, 0);
        glEnableClientState(GL_VERTEX_ARRAY);
        glEnableClientState(GL_COLOR_ARRAY);
        glVertexPointer(2, GL_FLOAT, 0, pos);
        glColorPointer(4, GL_UNSIGNED_BYTE, 0, col);
        glDrawElements(GL_QUADS, 4, GL_UNSIGNED_BYTE, quads);
        glEnable(GL_BLEND);
        glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
        glDrawElements(GL_QUADS, 4, GL_UNSIGNED_BYTE, quads + 4);
        glDisable(GL_BLEND);
        glEnable(GL_ALPHA_TEST);
        glAlphaFunc(GL_GREATER, 0.5f);
        glDrawArrays(GL_TRIANGLES, 8, 3);
        glDisable(GL_ALPHA_TEST);
        glDisableClientState(GL_COLOR_ARRAY);
        glDisableClientState(GL_VERTEX_ARRAY);
    }
    glFinish();
}

/* Compare `img` (RGB, top row first) with the reference, panel by panel. */
static int compare(const unsigned char *img)
{
    static const char *names[4] = {"shading", "depth and culling", "textures", "blending and arrays"};
    int n, bad_panels = 0;
    for (n = 0; n < 4; n++) {
        int x0 = (n % 2) * 64, y0 = H - 64 - (n / 2) * 64, x, y, off = 0, worst = 0, any = 0;
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
        if (off <= 40 && worst <= 64) {
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
        && fgets(hdr, sizeof hdr, f) && strcmp(hdr, "128 128\n") == 0
        && fgets(hdr, sizeof hdr, f) && strcmp(hdr, "255\n") == 0
        && fread(ref, 1, sizeof ref, f) == sizeof ref;
    fclose(f);
    return ok;
}

static LRESULT CALLBACK wndproc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY) {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

static int spin(void)
{
    WNDCLASSA wc = {0};
    PIXELFORMATDESCRIPTOR pfd = {sizeof pfd, 1};
    HWND hwnd;
    HDC dc;
    HGLRC rc;
    GLuint tex[2];
    MSG msg;
    float angle = 0;
    unsigned char red[3] = {230, 40, 40}, white[3] = {250, 250, 250};

    wc.style = CS_OWNDC;
    wc.lpfnWndProc = wndproc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursor(NULL, IDC_ARROW);
    wc.lpszClassName = "gl1";
    RegisterClassA(&wc);
    hwnd = CreateWindowA("gl1", "gl1: OpenGL", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 100, 100, 420, 420, NULL, NULL, wc.hInstance, NULL);
    dc = GetDC(hwnd);
    pfd.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
    pfd.cColorBits = 32;
    pfd.cDepthBits = 24;
    SetPixelFormat(dc, ChoosePixelFormat(dc, &pfd), &pfd);
    rc = wglCreateContext(dc);
    wglMakeCurrent(dc, rc);
    glGenTextures(2, tex);
    checker(tex[0], 8, red, white, GL_NEAREST);
    for (;;) {
        RECT r;
        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE)) {
            if (msg.message == WM_QUIT) {
                wglMakeCurrent(NULL, NULL);
                wglDeleteContext(rc);
                return 0;
            }
            DispatchMessageA(&msg);
        }
        GetClientRect(hwnd, &r);
        glViewport(0, 0, r.right, r.bottom);
        glClearColor(0.1f, 0.1f, 0.2f, 1);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        glFrustum(-0.5, 0.5, -0.5, 0.5, 1.0, 10.0);
        glMatrixMode(GL_MODELVIEW);
        glLoadIdentity();
        glTranslatef(0, 0, -4);
        glRotatef(angle, 1, 0.6f, 0.2f);
        glEnable(GL_DEPTH_TEST);
        glEnable(GL_CULL_FACE);
        glEnable(GL_TEXTURE_2D);
        cube();
        SwapBuffers(dc);
        angle += 2;
        Sleep(16);
    }
}

/* Frames of a textured floor and cubes, filling a 512x512 bitmap. */
static int bench(int frames)
{
    BITMAPINFO bi;
    PIXELFORMATDESCRIPTOR pfd = {sizeof pfd, 1};
    unsigned char red[3] = {230, 40, 40}, white[3] = {250, 250, 250};
    void *bits;
    HDC dc = CreateCompatibleDC(NULL);
    HGLRC rc;
    GLuint tex[1];
    DWORD t0, t;
    int f, i, j;

    memset(&bi, 0, sizeof bi);
    bi.bmiHeader.biSize = sizeof bi.bmiHeader;
    bi.bmiHeader.biWidth = 512;
    bi.bmiHeader.biHeight = 512;
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    SelectObject(dc, CreateDIBSection(dc, &bi, DIB_RGB_COLORS, &bits, NULL, 0));
    pfd.dwFlags = PFD_DRAW_TO_BITMAP | PFD_SUPPORT_OPENGL;
    pfd.cColorBits = 32;
    pfd.cDepthBits = 24;
    SetPixelFormat(dc, ChoosePixelFormat(dc, &pfd), &pfd);
    rc = wglCreateContext(dc);
    wglMakeCurrent(dc, rc);
    glGenTextures(1, tex);
    checker(tex[0], 8, red, white, GL_LINEAR);
    glHint(GL_PERSPECTIVE_CORRECTION_HINT, GL_NICEST);
    glEnable(GL_DEPTH_TEST);
    glEnable(GL_CULL_FACE);
    glEnable(GL_TEXTURE_2D);
    t0 = GetTickCount();
    for (f = 0; f < frames; f++) {
        glClearColor(0.1f, 0.1f, 0.2f, 1);
        glClear(GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT);
        glMatrixMode(GL_PROJECTION);
        glLoadIdentity();
        glFrustum(-0.5, 0.5, -0.5, 0.5, 1.0, 40.0);
        glMatrixMode(GL_MODELVIEW);
        glLoadIdentity();
        glTranslatef(0, -1, -6);
        glRotatef(20, 1, 0, 0);
        glRotatef((float)f * 3, 0, 1, 0);
        glColor3f(1, 1, 1);
        glBegin(GL_QUADS);
        glTexCoord2f(0, 0); glVertex3f(-8, -1, 8);
        glTexCoord2f(8, 0); glVertex3f(8, -1, 8);
        glTexCoord2f(8, 8); glVertex3f(8, -1, -8);
        glTexCoord2f(0, 8); glVertex3f(-8, -1, -8);
        glEnd();
        for (i = -2; i <= 2; i++)
            for (j = -2; j <= 2; j++) {
                glPushMatrix();
                glTranslatef(i * 2.5f, 0, j * 2.5f);
                glRotatef((float)(f * 5 + i * 30 + j * 50), 0.3f, 1, 0.2f);
                glScalef(0.7f, 0.7f, 0.7f);
                cube();
                glPopMatrix();
            }
        glFinish();
    }
    t = GetTickCount() - t0;
    printf("%d frames of 512x512 in %lu ms: %lu ms a frame\n", frames, t, t / frames);
    wglMakeCurrent(NULL, NULL);
    wglDeleteContext(rc);
    return 0;
}

int main(int argc, char **argv)
{
    BITMAPINFO bi;
    PIXELFORMATDESCRIPTOR pfd;
    unsigned char *bits, img[W * H * 3], rp[W * H * 3];
    HDC dc;
    HBITMAP bmp;
    HGLRC rc;
    GLuint tex[2];
    int fmt, x, y, same, bad;
    char path[MAX_PATH], *slash;

    if (argc > 1 && strcmp(argv[1], "wait") == 0)
        return spin();
    if (argc > 1 && strcmp(argv[1], "bench") == 0)
        return bench(argc > 2 ? atoi(argv[2]) : 20);
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
    fmt = ChoosePixelFormat(dc, &pfd);
    printf("pixel format: %s\n", fmt > 0 && SetPixelFormat(dc, fmt, &pfd) ? "set" : "FAILED");
    memset(&pfd, 0, sizeof pfd);
    DescribePixelFormat(dc, fmt, sizeof pfd, &pfd);
    printf("draws to a bitmap: %s, depth bits at least 16: %s\n",
           pfd.dwFlags & PFD_DRAW_TO_BITMAP ? "yes" : "no", pfd.cDepthBits >= 16 ? "yes" : "no");
    rc = wglCreateContext(dc);
    printf("context: %s, made current: %s\n", rc ? "created" : "FAILED", rc && wglMakeCurrent(dc, rc) ? "yes" : "no");
    if (!rc)
        return 1;
    printf("current: %s\n", wglGetCurrentContext() == rc && wglGetCurrentDC() == dc ? "yes" : "no");

    draw(tex);
    printf("errors while drawing: 0x%x\n", glGetError());

    /* The DIB (bottom-up BGRX) as RGB rows, top first; and glReadPixels. */
    for (y = 0; y < H; y++)
        for (x = 0; x < W; x++) {
            unsigned char *p = bits + ((H - 1 - y) * W + x) * 4, *q = img + (y * W + x) * 3;
            q[0] = p[2]; q[1] = p[1]; q[2] = p[0];
        }
    glPixelStorei(GL_PACK_ALIGNMENT, 1);
    glReadPixels(0, 0, W, H, GL_RGB, GL_UNSIGNED_BYTE, rp);
    same = 1;
    for (y = 0; y < H; y++)
        if (memcmp(rp + (H - 1 - y) * W * 3, img + y * W * 3, W * 3)) same = 0;
    printf("glReadPixels matches the bitmap: %s\n", same ? "yes" : "no");

    /* Errors as GL defines them. */
    glDepthFunc(0x1234);
    printf("bad enum: 0x%x\n", glGetError());
    glMatrixMode(GL_MODELVIEW);
    glLoadIdentity();
    glPopMatrix();
    printf("popping the last matrix: 0x%x\n", glGetError());
    glBegin(GL_TRIANGLES);
    glClear(GL_COLOR_BUFFER_BIT);
    glEnd();
    printf("glClear inside glBegin: 0x%x\n", glGetError());
    printf("no error left: 0x%x\n", glGetError());

    if (argc > 2 && strcmp(argv[1], "record") == 0) {
        FILE *f = fopen(argv[2], "wb");
        if (!f) {
            printf("can't write %s\n", argv[2]);
            return 1;
        }
        fprintf(f, "P6\n128 128\n255\n");
        fwrite(img, 1, sizeof img, f);
        fclose(f);
        printf("recorded\n");
        return 0;
    }

    GetModuleFileNameA(NULL, path, sizeof path);
    slash = strrchr(path, '\\');
    if (!slash) slash = strrchr(path, '/');
    strcpy(slash ? slash + 1 : path, "gl1.ref");
    if (!load_ref(path)) {
        printf("no reference image\n");
        return 1;
    }
    bad = compare(img);

    glDeleteTextures(2, tex);
    wglMakeCurrent(NULL, NULL);
    printf("deleted: %s, current after: %s\n", wglDeleteContext(rc) ? "yes" : "no", wglGetCurrentContext() ? "a context" : "none");
    DeleteDC(dc);
    DeleteObject(bmp);
    return bad;
}
