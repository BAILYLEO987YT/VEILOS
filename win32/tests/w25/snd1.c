/* snd1: sound through winmm and kernel32, checked against Windows.
 *
 * Plays, with gaps of silence between them:
 *   1. waveOut: 440 Hz, 44.1 kHz 16-bit stereo, 1 s in 4 buffers (callback function)
 *   2. PlaySound from memory: 880 Hz, 22.05 kHz 8-bit mono, 0.5 s (SND_MEMORY|SND_SYNC)
 *   3. PlaySound from a file: 660 Hz, 48 kHz 32-bit float stereo, 0.5 s
 *   4. Beep(1000, 300)
 *   5. waveOut: 330 Hz, 16 kHz 24-bit mono, 0.4 s, looped twice (one header)
 * and prints what each call answered. Run with "quiet" to skip the API error
 * checks' output (the sounds still play).
 */
#include <windows.h>
#include <mmsystem.h>
#include <stdio.h>
#include <string.h>
#include <math.h>

static volatile LONG done_count, open_seen, close_seen;

static void CALLBACK cb(HWAVEOUT h, UINT msg, DWORD_PTR inst, DWORD_PTR p1, DWORD_PTR p2) {
    (void)h; (void)inst; (void)p1; (void)p2;
    if (msg == WOM_DONE) InterlockedIncrement(&done_count);
    if (msg == WOM_OPEN) InterlockedIncrement(&open_seen);
    if (msg == WOM_CLOSE) InterlockedIncrement(&close_seen);
}

static double tone(double hz, int i, int rate) {
    return sin(2.0 * 3.14159265358979 * hz * i / rate);
}

static void put16(unsigned char *p, unsigned v) { p[0] = (unsigned char)v; p[1] = (unsigned char)(v >> 8); }
static void put32(unsigned char *p, unsigned v) { put16(p, v & 0xFFFF); put16(p + 2, v >> 16); }

/* A RIFF WAVE image around `data`. */
static unsigned char *wav(int tag, int ch, int rate, int bits, const void *data, unsigned len, unsigned *total) {
    unsigned char *w = (unsigned char *)malloc(44 + len);
    memcpy(w, "RIFF", 4); put32(w + 4, 36 + len); memcpy(w + 8, "WAVEfmt ", 8);
    put32(w + 16, 16); put16(w + 20, tag); put16(w + 22, ch); put32(w + 24, rate);
    put32(w + 28, rate * ch * bits / 8); put16(w + 32, ch * bits / 8); put16(w + 34, bits);
    memcpy(w + 36, "data", 4); put32(w + 40, len); memcpy(w + 44, data, len);
    *total = 44 + len;
    return w;
}

int main(int argc, char **argv) {
    int quiet = argc > 1 && !strcmp(argv[1], "quiet");
    WAVEFORMATEX f = {0};
    HWAVEOUT h = 0;
    MMRESULT r;
    int i;

    printf("devices %d\n", waveOutGetNumDevs() > 0);
    {
        WAVEOUTCAPSW caps;
        r = waveOutGetDevCapsW(WAVE_MAPPER, &caps, sizeof caps);
        printf("caps %u channels %u volume %u\n", r, caps.wChannels, (caps.dwSupport & WAVECAPS_VOLUME) != 0);
    }

    /* Formats. */
    f.wFormatTag = WAVE_FORMAT_PCM; f.nChannels = 2; f.nSamplesPerSec = 44100; f.wBitsPerSample = 12;
    f.nBlockAlign = 3; f.nAvgBytesPerSec = 44100 * 3;
    printf("open 12-bit %u\n", waveOutOpen(&h, WAVE_MAPPER, &f, 0, 0, CALLBACK_NULL));
    f.wFormatTag = 2; /* ADPCM */ f.wBitsPerSample = 4; f.nBlockAlign = 2048; f.cbSize = 0;
    printf("open adpcm %u\n", waveOutOpen(&h, WAVE_MAPPER, &f, 0, 0, CALLBACK_NULL));
    f.wFormatTag = WAVE_FORMAT_PCM; f.nChannels = 2; f.nSamplesPerSec = 44100; f.wBitsPerSample = 16;
    f.nBlockAlign = 4; f.nAvgBytesPerSec = 44100 * 4;
    printf("query 16-bit %u\n", waveOutOpen(NULL, WAVE_MAPPER, &f, 0, 0, WAVE_FORMAT_QUERY));
    printf("open bad device %u\n", waveOutOpen(&h, 100, &f, 0, 0, CALLBACK_NULL));
    printf("close bad handle %u\n", waveOutClose((HWAVEOUT)0x1234));

    /* 1. 440 Hz, 1 s in 4 buffers. */
    r = waveOutOpen(&h, WAVE_MAPPER, &f, (DWORD_PTR)cb, 0, CALLBACK_FUNCTION);
    printf("open %u open-callback %ld\n", r, open_seen);
    {
        static short buf[4][11025 * 2];
        WAVEHDR hd[4];
        MMTIME t;
        DWORD start;
        for (i = 0; i < 44100; i++) {
            short s = (short)(tone(440, i, 44100) * 12000);
            buf[i / 11025][(i % 11025) * 2] = s;
            buf[i / 11025][(i % 11025) * 2 + 1] = s;
        }
        memset(hd, 0, sizeof hd);
        hd[0].lpData = (LPSTR)buf[0]; hd[0].dwBufferLength = sizeof buf[0];
        printf("write unprepared %u\n", waveOutWrite(h, &hd[0], sizeof hd[0]));
        for (i = 0; i < 4; i++) {
            hd[i].lpData = (LPSTR)buf[i]; hd[i].dwBufferLength = sizeof buf[i];
            r = waveOutPrepareHeader(h, &hd[i], sizeof hd[i]);
            if (!quiet || r) printf("prepare %d: %u flags %lx\n", i, r, hd[i].dwFlags);
        }
        for (i = 0; i < 4; i++) {
            r = waveOutWrite(h, &hd[i], sizeof hd[i]);
            if (!quiet || r) printf("write %d: %u inqueue %d\n", i, r, (hd[i].dwFlags & WHDR_INQUEUE) != 0);
        }
        printf("close while playing %u\n", waveOutClose(h));
        printf("unprepare while playing %u\n", waveOutUnprepareHeader(h, &hd[3], sizeof hd[3]));
        start = GetTickCount();
        while (done_count < 4 && GetTickCount() - start < 5000) Sleep(10);
        printf("done %ld, took about 1 s: %d\n", done_count, GetTickCount() - start > 600 && GetTickCount() - start < 2500);
        for (i = 0; i < 4; i++) {
            if (!quiet) printf("header %d flags %lx\n", i, hd[i].dwFlags);
            waveOutUnprepareHeader(h, &hd[i], sizeof hd[i]);
        }
        t.wType = TIME_BYTES;
        waveOutGetPosition(h, &t, sizeof t);
        printf("position type %u at end: %d\n", t.wType, t.u.cb >= 44100 * 4 - 4096 && t.u.cb <= 44100 * 4);
        t.wType = TIME_SMPTE;
        waveOutGetPosition(h, &t, sizeof t);
        printf("position smpte answered as %u\n", t.wType);
        r = waveOutReset(h);
        t.wType = TIME_SAMPLES;
        waveOutGetPosition(h, &t, sizeof t);
        printf("reset %u, position after %lu\n", r, t.u.sample);
        r = waveOutClose(h);
        printf("close %u close-callback %ld\n", r, close_seen);
    }
    Sleep(300);

    /* 2. PlaySound from memory: 880 Hz, 8-bit mono 22050, 0.5 s. */
    {
        static unsigned char pcm[11025];
        unsigned n;
        unsigned char *w;
        DWORD t0;
        for (i = 0; i < 11025; i++) pcm[i] = (unsigned char)(128 + tone(880, i, 22050) * 45);
        w = wav(1, 1, 22050, 8, pcm, sizeof pcm, &n);
        t0 = GetTickCount();
        printf("PlaySound memory %d\n", PlaySoundW((LPCWSTR)w, NULL, SND_MEMORY | SND_SYNC));
        printf("sync took about 0.5 s: %d\n", GetTickCount() - t0 > 300 && GetTickCount() - t0 < 1500);
    }
    Sleep(300);

    /* 3. PlaySound from a file: 660 Hz, float stereo 48 kHz, 0.5 s. */
    {
        static float pcm[24000 * 2];
        unsigned n;
        unsigned char *w;
        FILE *out;
        for (i = 0; i < 24000; i++) pcm[2 * i] = pcm[2 * i + 1] = (float)(tone(660, i, 48000) * 0.35);
        w = wav(3, 2, 48000, 32, pcm, sizeof pcm, &n);
        out = fopen("snd1_tmp.wav", "wb");
        fwrite(w, 1, n, out);
        fclose(out);
        printf("PlaySound file %d\n", PlaySoundA("snd1_tmp.wav", NULL, SND_FILENAME | SND_SYNC));
        DeleteFileA("snd1_tmp.wav");
        printf("PlaySound missing nodefault %d\n", PlaySoundA("snd1_missing.wav", NULL, SND_FILENAME | SND_SYNC | SND_NODEFAULT));
        {
            DWORD t1 = GetTickCount();
            int ok = PlaySoundA("SystemAsterisk", NULL, SND_ALIAS | SND_SYNC | SND_LOOP | SND_NODEFAULT);
            printf("PlaySound loop sync %d returned at once: %d\n", ok, GetTickCount() - t1 < 150);
            Sleep(1500);
        }
        printf("PlaySound stop %d\n", PlaySoundA(NULL, NULL, 0));
    }
    Sleep(300);

    /* 4. Beep. */
    {
        DWORD t0 = GetTickCount();
        SetLastError(0);
        printf("Beep %d\n", Beep(1000, 300));
        printf("Beep took about 0.3 s: %d\n", GetTickCount() - t0 > 200 && GetTickCount() - t0 < 1000);
        SetLastError(0);
        i = Beep(10, 50);
        printf("Beep 10 Hz %d error %lu\n", i, GetLastError());
    }
    Sleep(300);

    /* 5. 330 Hz, 24-bit mono 16 kHz, 0.4 s, played twice by a loop. */
    {
        static unsigned char pcm[6400 * 3];
        WAVEHDR hd;
        DWORD start;
        WAVEFORMATEX g = {0};
        g.wFormatTag = WAVE_FORMAT_PCM; g.nChannels = 1; g.nSamplesPerSec = 16000; g.wBitsPerSample = 24;
        g.nBlockAlign = 3; g.nAvgBytesPerSec = 48000;
        for (i = 0; i < 6400; i++) {
            int s = (int)(tone(330, i, 16000) * 3000000);
            pcm[3 * i] = (unsigned char)s; pcm[3 * i + 1] = (unsigned char)(s >> 8); pcm[3 * i + 2] = (unsigned char)(s >> 16);
        }
        done_count = 0;
        r = waveOutOpen(&h, WAVE_MAPPER, &g, (DWORD_PTR)cb, 0, CALLBACK_FUNCTION);
        printf("open 24-bit %u\n", r);
        memset(&hd, 0, sizeof hd);
        hd.lpData = (LPSTR)pcm; hd.dwBufferLength = sizeof pcm;
        hd.dwFlags = WHDR_BEGINLOOP | WHDR_ENDLOOP; hd.dwLoops = 2;
        waveOutPrepareHeader(h, &hd, sizeof hd);
        waveOutWrite(h, &hd, sizeof hd);
        start = GetTickCount();
        while (done_count < 1 && GetTickCount() - start < 5000) Sleep(10);
        printf("loop done %ld, took about 0.8 s: %d\n", done_count, GetTickCount() - start > 550 && GetTickCount() - start < 2000);
        waveOutUnprepareHeader(h, &hd, sizeof hd);
        waveOutClose(h);
    }
    Sleep(300);

    /* 6. MessageBeep: the error sound, asynchronous. */
    {
        DWORD t0 = GetTickCount();
        printf("MessageBeep %d returned at once: %d\n", MessageBeep(MB_ICONHAND), GetTickCount() - t0 < 150);
        Sleep(1500);
    }
    printf("timeGetTime ticks: %d\n", timeGetTime() != 0);
    printf("done\n");
    return 0;
}
