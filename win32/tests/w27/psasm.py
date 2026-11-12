# psasm.py IN OUT: assemble ps_1_1 .. ps_1_4 pixel shaders (Direct3D 9
# shader assembly) into C token arrays, since the SDK's fxc no longer emits
# ps_1_x. IN holds shaders as
#
#   shader NAME
#   ps_1_1
#   def c0, 0.5, 0.5, 0.5, 1
#   tex t0
#   mul_x2 r0.rgb, t0_bx2, v0
#   + mov r0.a, c0
#   end
#
# Instructions, destinations with write masks and result modifiers (_sat,
# _x2, _x4, _x8, _d2, _d4, _d8), sources with negation, swizzles and source
# modifiers (_bias, _bx2, _x2, _dz, _db, _dw, _da, 1-r for complement),
# '+' for co-issue. OUT gets `static const DWORD NAME[] = {...};` for each.
import re, struct, sys

OPS = {
    'nop': 0, 'mov': 1, 'add': 2, 'sub': 3, 'mad': 4, 'mul': 5, 'rcp': 6, 'rsq': 7, 'dp3': 8, 'dp4': 9,
    'min': 10, 'max': 11, 'slt': 12, 'sge': 13, 'lrp': 18, 'texcoord': 64, 'texcrd': 64, 'texkill': 65,
    'tex': 66, 'texld': 66, 'texbem': 67, 'texbeml': 68, 'texreg2ar': 69, 'texreg2gb': 70, 'texm3x2pad': 71,
    'texm3x2tex': 72, 'texm3x3pad': 73, 'texm3x3tex': 74, 'texm3x3spec': 76, 'texm3x3vspec': 77,
    'cnd': 80, 'texreg2rgb': 82, 'texdp3tex': 83, 'texm3x2depth': 84, 'texdp3': 85, 'texm3x3': 86,
    'texdepth': 87, 'cmp': 88, 'bem': 89, 'phase': 0xFFFD,
}
REG = {'r': 0, 'v': 1, 'c': 2, 't': 3}
SHIFT = {'x2': 1, 'x4': 2, 'x8': 3, 'd2': 0xF, 'd4': 0xE, 'd8': 0xD}
SRCMOD = {'bias': 2, 'bx2': 4, 'x2': 7, 'dz': 9, 'db': 9, 'dw': 10, 'da': 10}
COMP = 'xyzw'
ALIAS = {'r': 'x', 'g': 'y', 'b': 'z', 'a': 'w'}


def reg(s):
    m = re.fullmatch(r'([rvct])(\d+)', s)
    if not m:
        raise SystemExit('bad register ' + s)
    t, n = REG[m.group(1)], int(m.group(2))
    return 0x80000000 | (t & 7) << 28 | (t >> 3) << 11 | n


def mask(s):
    if not s:
        return 0xF
    s = ''.join(ALIAS.get(c, c) for c in s)
    return sum(1 << COMP.index(c) for c in s)


def swizzle(s):
    if not s:
        return 0xE4
    s = ''.join(ALIAS.get(c, c) for c in s)
    s = (s + s[-1] * 4)[:4]
    return sum(COMP.index(c) << (2 * i) for i, c in enumerate(s))


def dest(text, mods):
    r, _, m = text.partition('.')
    tok = reg(r) | mask(m) << 16
    for x in mods:
        if x == 'sat':
            tok |= 1 << 20
        elif x in SHIFT:
            tok |= SHIFT[x] << 24
        else:
            raise SystemExit('bad result modifier ' + x)
    return tok


def source(text):
    text = text.strip()
    neg = text.startswith('-')
    if neg:
        text = text[1:]
    comp = text.startswith('1-')
    if comp:
        text = text[2:]
    r, _, sw = text.partition('.')
    r, *mods = r.split('_')
    mod = 0
    for x in mods:
        mod = SRCMOD[x]
    if comp:
        mod = 6
    if neg:
        mod = {0: 1, 2: 3, 4: 5, 7: 8}.get(mod, mod)
    return reg(r) | swizzle(sw) << 16 | mod << 24


def assemble(lines):
    toks = []
    for line in lines:
        line = line.split(';')[0].split('//')[0].strip()
        if not line:
            continue
        m = re.fullmatch(r'ps_1_(\d)', line)
        if m:
            toks.append(0xFFFF0100 | int(m.group(1)))
            continue
        co = line.startswith('+')
        if co:
            line = line[1:].strip()
        op, _, rest = line.partition(' ')
        op, *mods = op.split('_')
        args = [a.strip() for a in rest.split(',')] if rest.strip() else []
        if op == 'def':
            toks.append(0x51)
            toks.append(dest(args[0], []))
            toks += [struct.unpack('<I', struct.pack('<f', float(v)))[0] for v in args[1:5]]
            continue
        toks.append(OPS[op] | (1 << 30 if co else 0))
        if op == 'phase':
            continue
        if args:
            toks.append(dest(args[0], mods))
            toks += [source(a) for a in args[1:]]
    toks.append(0x0000FFFF)
    return toks


def main():
    src, out = sys.argv[1], sys.argv[2]
    shaders, name, body = [], None, []
    for line in open(src):
        s = line.strip()
        if s.startswith('shader '):
            name, body = s.split()[1], []
        elif s == 'end':
            shaders.append((name, assemble(body)))
            name = None
        elif name:
            body.append(line)
    with open(out, 'w', newline='\n') as f:
        f.write('/* Made by psasm.py from %s: do not edit. */\n' % src)
        for n, t in shaders:
            f.write('static const DWORD %s[] = {\n' % n)
            for i in range(0, len(t), 6):
                f.write('    ' + ', '.join('0x%08X' % x for x in t[i:i + 6]) + ',\n')
            f.write('};\n')


main()
