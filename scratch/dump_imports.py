
import struct

with open("storage/desktop/gui_test.exe", "rb") as f:
    data = f.read()

# Just scan for strings in .rdata
print(data[0x600:0x800].replace(b"\x00", b" ").decode("ascii", "ignore"))

