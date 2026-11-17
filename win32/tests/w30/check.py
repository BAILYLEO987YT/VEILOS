"""check.py REF_DIR GOT_DIR [SCREEN.png] -- compare what OpenTTD wrote on VEILOS
(GOT_DIR: exit.sav, bg.png, mm.png) with the Windows reference runs (REF_DIR,
made by prepare.sh); and, given a screendump of VEILOS with the game's
window on it, that window with the screenshot the game took of itself at
that moment (GOT_DIR/late.png: its Ctrl+S screenshot): what GDI put on the screen.

The saves are compared decompressed. Each game has a random 128-bit id
(written as 32 hex digits), so one run of up to 32 differing bytes, hex digits
in both, is allowed; nothing else may differ. The screenshots are compared
pixel by pixel and must be equal; on the screen, only the mouse cursor's
square (at most 32 x 32 pixels) may differ."""
import lzma, os, sys, zlib
from PIL import Image

ref, got = sys.argv[1:3]
bad = 0

def load(p):
    b = open(p, 'rb').read()
    if b[:4] == b'OTTX':
        return b[:8], lzma.decompress(b[8:])
    if b[:4] == b'OTTZ':
        return b[:8], zlib.decompress(b[8:])
    return b[:8], b[8:]

(ha, a), (hb, b) = load(os.path.join(ref, 'exit.sav')), load(os.path.join(got, 'exit.sav'))
diff = [i for i in range(min(len(a), len(b))) if a[i] != b[i]]
hexd = set(b'0123456789ABCDEFabcdef')
ok = ha == hb and len(a) == len(b) and (not diff or (diff[-1] - diff[0] < 32 and all(a[i] in hexd and b[i] in hexd for i in diff)))
print('save: %d bytes, %d differ (the game id)%s' % (len(b), len(diff), '' if ok else ' -- FAIL'))
bad += not ok

for name in ('bg.png', 'mm.png'):
    x = Image.open(os.path.join(ref, name)).convert('RGB')
    y = Image.open(os.path.join(got, name)).convert('RGB')
    same = x.size == y.size and x.tobytes() == y.tobytes()
    print('%s: %dx%d, %s' % (name, y.size[0], y.size[1], 'equal' if same else 'DIFFERENT'))
    bad += not same
if len(sys.argv) > 3:
    scr = Image.open(sys.argv[3]).convert('RGB')
    late = Image.open(os.path.join(got, 'late.png')).convert('RGB')
    w, h = late.size
    # Where the window's client area is: a row of the game's image found
    # on the screen.
    row = [late.getpixel((x, h // 2)) for x in range(0, w, 7)]
    at = None
    for oy in range(scr.size[1] - h + 1):
        for ox in range(scr.size[0] - w + 1):
            if all(scr.getpixel((ox + 7 * i, oy + h // 2)) == p for i, p in enumerate(row)):
                at = (ox, oy)
                break
        if at:
            break
    if at is None:
        print('screen: the window not found -- FAIL')
        bad += 1
    else:
        diff = [(x, y) for y in range(h) for x in range(w) if scr.getpixel((at[0] + x, at[1] + y)) != late.getpixel((x, y))]
        span = (max(x for x, _ in diff) - min(x for x, _ in diff), max(y for _, y in diff) - min(y for _, y in diff)) if diff else (0, 0)
        ok = span[0] < 32 and span[1] < 32
        print('screen: the window at %s, %d pixels differ%s' % (at, len(diff), ' (the cursor)' if diff and ok else '' if ok else ' -- FAIL'))
        bad += not ok
print('failed: %d' % bad)
sys.exit(1 if bad else 0)
