
import struct
import sys

def parse_pe(path):
    with open(path, "rb") as f:
        data = f.read()

    # DOS header e_lfanew
    pe_offset = struct.unpack_from("<I", data, 0x3C)[0]
    
    # PE Header
    if data[pe_offset:pe_offset+4] != b"PE\x00\x00":
        return
        
    # Optional header
    opt_header_offset = pe_offset + 24
    
    # Data directories
    data_dir_offset = opt_header_offset + 112
    import_va, import_size = struct.unpack_from("<II", data, data_dir_offset + 8)
    
    # Find section containing import_va
    sections_offset = opt_header_offset + 240
    num_sections = struct.unpack_from("<H", data, pe_offset + 6)[0]
    
    for i in range(num_sections):
        sec_offset = sections_offset + (i * 40)
        vaddr, vsize, raddr, rsize = struct.unpack_from("<IIII", data, sec_offset + 12)
        if import_va >= vaddr and import_va < vaddr + vsize:
            file_offset = import_va - vaddr + raddr
            
            # Read import directory
            curr = file_offset
            while True:
                ilt_rva, time, chain, name_rva, iat_rva = struct.unpack_from("<IIIII", data, curr)
                if ilt_rva == 0 and name_rva == 0:
                    break
                    
                name_offset = name_rva - vaddr + raddr
                name = b""
                n = name_offset
                while data[n] != 0:
                    name += bytes([data[n]])
                    n += 1
                
                print(f"DLL: {name.decode()}")
                
                # Read thunks
                thunk_rva = ilt_rva if ilt_rva != 0 else iat_rva
                thunk_offset = thunk_rva - vaddr + raddr
                
                while True:
                    thunk = struct.unpack_from("<Q", data, thunk_offset)[0]
                    if thunk == 0:
                        break
                        
                    if thunk & (1 << 63):
                        print(f"  Ordinal: {thunk & 0xFFFF}")
                    else:
                        func_name_rva = (thunk & 0x7FFFFFFF) + 2
                        func_name_offset = func_name_rva - vaddr + raddr
                        func_name = b""
                        n = func_name_offset
                        while data[n] != 0:
                            func_name += bytes([data[n]])
                            n += 1
                        print(f"  {func_name.decode()}")
                        
                    thunk_offset += 8
                curr += 20
            break

parse_pe("storage/desktop/gui_test.exe")

