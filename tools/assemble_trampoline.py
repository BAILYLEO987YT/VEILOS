"""
AP trampoline assembler — regenerates AP_TRAMPOLINE_BYTES in src/smp.rs.

Usage: python tools/assemble_trampoline.py
Outputs the Rust static array to stdout. Paste it into the static in smp.rs.
"""
import struct

BLOB_SIZE = 0x200
blob = bytearray(b'\x00' * BLOB_SIZE)

def put(off, data):
    blob[off:off+len(data)] = bytes(data)

def u16(v): return struct.pack('<H', v)
def u32(v): return struct.pack('<I', v)

# 16-bit code at 0x000
code16 = bytearray()
code16 += bytes([0xFA])                                         # cli
code16 += bytes([0x31, 0xC0])                                   # xor ax, ax
code16 += bytes([0x8E, 0xD8])                                   # mov ds, ax
code16 += bytes([0x8E, 0xC0])                                   # mov es, ax
code16 += bytes([0x8E, 0xD0])                                   # mov ss, ax
code16 += bytes([0xBC, 0xF0, 0x0F])                             # mov sp, 0x0FF0
code16 += bytes([0x8C, 0xC8])                                   # mov ax, cs
# NOTE: do NOT set DS=CS here. Keep DS=0 so [esi+disp]=phys_base+disp (correct).
# Setting DS=CS would make [esi+disp] = DS.base + esi + disp = 2*phys_base+disp (wrong).
code16 += bytes([0x66, 0x0F, 0xB7, 0xF0])                      # movzx esi, ax
code16 += bytes([0x66, 0xC1, 0xE6, 0x04])                      # shl esi, 4
# lea eax,[esi+0x80]   (67=addr32 prefix, 66=op32 prefix)
code16 += bytes([0x67, 0x66, 0x8D, 0x86, 0x80, 0x00, 0x00, 0x00])
# mov [esi+0xA2],eax   (GDTR base field = GDTR_off+2)
code16 += bytes([0x67, 0x66, 0x89, 0x86, 0xA2, 0x00, 0x00, 0x00])
# lea eax,[esi+0x100]
code16 += bytes([0x67, 0x66, 0x8D, 0x86, 0x00, 0x01, 0x00, 0x00])
# mov [esi+0xB0],eax   (PM far-jmp offset)
code16 += bytes([0x67, 0x66, 0x89, 0x86, 0xB0, 0x00, 0x00, 0x00])
# lgdt [esi+0xA0]
code16 += bytes([0x67, 0x0F, 0x01, 0x96, 0xA0, 0x00, 0x00, 0x00])
code16 += bytes([0x66, 0x0F, 0x20, 0xC0])                      # mov eax, cr0
code16 += bytes([0x66, 0x83, 0xC8, 0x01])                      # or  eax, 1
code16 += bytes([0x66, 0x0F, 0x22, 0xC0])                      # mov cr0, eax
# already correct via modrm_disp32_esi    # jmp fword [esi+0xB0]
assert len(code16) <= 0x80
put(0x000, code16)

# GDT at 0x080
put(0x080, bytes(8))                                            # null
put(0x088, bytes.fromhex('FFFF000000CF9A00'))                   # code32
put(0x090, bytes.fromhex('FFFF000000CF9200'))                   # data32
put(0x098, bytes.fromhex('FFFF000000AF9A00'))                   # code64

# GDTR at 0x0A0: limit=0x1F, base=0 (patched at runtime)
put(0x0A0, u16(0x1F) + u32(0))

# PM far-jump at 0x0B0: offset=0 (patched), selector=0x08
put(0x0B0, u32(0) + u16(0x08))

# Parameter block at 0x0C0: all zeros (written by write_ap_params)
put(0x0C0, bytes(40))

# LM far-jump at 0x0E0: offset=0 (patched), selector=0x18
put(0x0E0, u32(0) + u16(0x18))

# 32-bit code at 0x100
code32 = bytearray()
code32 += bytes([0x66, 0xB8, 0x10, 0x00])                      # mov ax, 0x10
code32 += bytes([0x8E, 0xD8])                                   # mov ds, ax
code32 += bytes([0x8E, 0xC0])                                   # mov es, ax
code32 += bytes([0x8E, 0xE0])                                   # mov fs, ax
code32 += bytes([0x8E, 0xE8])                                   # mov gs, ax
code32 += bytes([0x8E, 0xD0])                                   # mov ss, ax
code32 += bytes([0xBC, 0xF0, 0x0F, 0x00, 0x00])                 # mov esp, 0x0FF0
code32 += bytes([0x0F, 0x20, 0xE0])                             # mov eax, cr4
code32 += bytes([0x83, 0xC8, 0x20])                             # or  eax, 0x20 (PAE)
code32 += bytes([0x0F, 0x22, 0xE0])                             # mov cr4, eax
code32 += bytes([0x8B, 0x86, 0xC0, 0x00, 0x00, 0x00])           # mov eax,[esi+0xC0]
code32 += bytes([0x0F, 0x22, 0xD8])                             # mov cr3, eax
code32 += bytes([0xB9, 0x80, 0x00, 0x00, 0xC0])                 # mov ecx, 0xC0000080
code32 += bytes([0x0F, 0x32])                                   # rdmsr
code32 += bytes([0x0D, 0x00, 0x01, 0x00, 0x00])                 # or  eax, 0x100 (LME)
code32 += bytes([0x0F, 0x30])                                   # wrmsr
code32 += bytes([0x0F, 0x20, 0xC0])                             # mov eax, cr0
code32 += bytes([0x0D, 0x00, 0x00, 0x00, 0x80])                 # or  eax, 0x80000000 (PG)
code32 += bytes([0x0F, 0x22, 0xC0])                             # mov cr0, eax
code32 += bytes([0x8D, 0x86, 0x80, 0x01, 0x00, 0x00])           # lea eax,[esi+0x180]
code32 += bytes([0x89, 0x86, 0xE0, 0x00, 0x00, 0x00])           # mov [esi+0xE0],eax
code32 += bytes([0xFF, 0xBE, 0xE0, 0x00, 0x00, 0x00])           # jmp fword [esi+0xE0]
assert len(code32) <= 0x80
put(0x100, code32)

# 64-bit code at 0x180
code64 = bytearray()
code64 += bytes([0x48, 0x89, 0xF3])                             # mov rbx, rsi
code64 += bytes([0x48, 0x8B, 0x83, 0xC8, 0x00, 0x00, 0x00])    # mov rax,[rbx+0xC8]
code64 += bytes([0x48, 0x8B, 0xA3, 0xD0, 0x00, 0x00, 0x00])    # mov rsp,[rbx+0xD0]
code64 += bytes([0x8B, 0xBB, 0xD8, 0x00, 0x00, 0x00])           # mov edi,[rbx+0xD8]
code64 += bytes([0x66, 0xB9, 0x10, 0x00])                       # mov cx, 0x10
code64 += bytes([0x8E, 0xD9])                                   # mov ds, cx
code64 += bytes([0x8E, 0xC1])                                   # mov es, cx
code64 += bytes([0x8E, 0xE1])                                   # mov fs, cx
code64 += bytes([0x8E, 0xE9])                                   # mov gs, cx
code64 += bytes([0x8E, 0xD1])                                   # mov ss, cx
code64 += bytes([0xFF, 0xE0])                                   # jmp rax
assert len(code64) <= 0x80
put(0x180, code64)

# Print as Rust array
lines = []
for i in range(0, BLOB_SIZE, 16):
    row = ', '.join(f'0x{b:02X}' for b in blob[i:i+16])
    lines.append(f'    /* {i:#05x} */ {row},')
print('#[allow(clippy::all)]')
print('static AP_TRAMPOLINE_BYTES: [u8; 0x200] = [')
for l in lines:
    print(l)
print('];')
