
import urllib.request
import os

if not os.path.exists("capstone"):
    urllib.request.urlretrieve("https://github.com/capstone-engine/capstone/releases/download/5.0.1/capstone-5.0.1-win64.zip", "capstone.zip")
    import zipfile
    with zipfile.ZipFile("capstone.zip", "r") as z:
        z.extractall("capstone")

import sys
sys.path.append("capstone/python")
from capstone import *

with open("storage/desktop/gui_test.exe", "rb") as f:
    data = f.read()

md = Cs(CS_ARCH_X86, CS_MODE_64)
for i in md.disasm(data[0x400:0x500], 0x140001000):
    print("0x%x:\t%s\t%s" %(i.address, i.mnemonic, i.op_str))

