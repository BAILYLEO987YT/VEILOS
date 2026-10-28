
import struct

with open("storage/desktop/gui_test.exe", "rb") as f:
    data = f.read()

print(data[0x800:0xA00].replace(b"\x00", b" ").decode("ascii", "ignore"))

