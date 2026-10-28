
import string
with open("storage/desktop/gui_test.exe", "rb") as f:
    data = f.read()

printable = set(string.printable.encode("ascii")) - set(b"\n\r\t")
current = bytearray()
for b in data:
    if b in printable:
        current.append(b)
    else:
        if len(current) >= 5:
            print(current.decode("ascii"))
        current = bytearray()

