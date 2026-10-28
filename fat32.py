import struct

def format_fat32(filename):
    with open(filename, 'r+b') as f:
        # Clear the first 1MB just to be sure
        f.seek(0)
        f.write(b'\x00' * (1024 * 1024))
        
        # Write basic BPB (Sector 0)
        f.seek(0)
        # JMP + OEM
        f.write(b'\xEB\x3C\x90' + b'MSDOS5.0')
        
        # Standard BPB
        f.write(struct.pack('<H', 512))    # Bytes per sector
        f.write(struct.pack('<B', 8))      # Sectors per cluster
        f.write(struct.pack('<H', 32))     # Reserved sectors
        f.write(struct.pack('<B', 2))      # FAT count
        f.write(struct.pack('<H', 0))      # Root entry count (0 for FAT32)
        f.write(struct.pack('<H', 0))      # Total sectors 16
        f.write(b'\xF8')                   # Media type (Hard Disk)
        f.write(struct.pack('<H', 0))      # Sectors per FAT 16
        f.write(struct.pack('<H', 63))     # Sectors per track
        f.write(struct.pack('<H', 255))    # Head count
        f.write(struct.pack('<I', 0))      # Hidden sectors
        f.write(struct.pack('<I', 20480))  # Total sectors 32
        
        # FAT32 Extended
        f.write(struct.pack('<I', 32))     # Sectors per FAT 32
        f.write(struct.pack('<H', 0))      # Ext flags
        f.write(struct.pack('<H', 0))      # Version
        f.write(struct.pack('<I', 2))      # Root cluster
        f.write(struct.pack('<H', 1))      # FS Info sector
        f.write(struct.pack('<H', 6))      # Backup boot sector
        f.write(b'\x00' * 12)              # Reserved
        f.write(b'\x80')                   # Drive number
        f.write(b'\x00')                   # Reserved
        f.write(b'\x29')                   # EXT SIGNATURE
        f.write(b'\xDE\xAD\xBE\xEF')       # Volume ID
        f.write(b'VEILOS DISK')            # Volume Label
        f.write(b'FAT32   ')               # System ID
        
        # Final Boot Signature
        f.seek(510)
        f.write(b'\x55\xAA')

        # ---| Initialize FAT Tables |---
        # FAT1 starts at reserved sector 32, FAT2 at sector 64 (32 sectors each).
        # Entry 0: media descriptor (0x0FFFFFF8)
        # Entry 1: EOC marker      (0x0FFFFFFF)
        # Entry 2: root directory  (0x0FFFFFFF = EOC, single cluster)
        # Entry 3: HELLO.TXT data  (0x0FFFFFFF = EOC, single cluster)
        fat_entries = struct.pack('<IIII',
            0x0FFFFFF8,  # cluster 0 — media descriptor
            0x0FFFFFFF,  # cluster 1 — EOC marker
            0x0FFFFFFF,  # cluster 2 — root directory (EOC)
            0x0FFFFFFF,  # cluster 3 — HELLO.TXT data (EOC)
        )
        for fat_start_sector in (32, 64):  # FAT1 and FAT2
            f.seek(fat_start_sector * 512)
            f.write(fat_entries)

        # ---| Add HELLO.TXT to Root Directory |---
        # First Data Sector = Reserved + (FATCount * FATSize)
        # = 32 + (2 * 32) = 32 + 64 = 96
        # Root Directory starts at Cluster 2 -> Sector 96
        f.seek(96 * 512)
        
        # Directory Entry (32 bytes)
        file_name = b'HELLO   ' # 8 bytes
        file_ext  = b'TXT'      # 3 bytes
        f.write(file_name + file_ext) 
        f.write(b'\x20')        # Attributes (Archive)
        f.write(b'\x00' * 8)    # Reserved/Times
        f.write(struct.pack('<H', 0)) # Cluster Hi
        f.write(b'\x00' * 4)    # Times
        f.write(struct.pack('<H', 3)) # Cluster Lo (Cluster 3)
        f.write(struct.pack('<I', 25)) # File Size (25 bytes)

        # ---| Add Data for HELLO.TXT |---
        # Cluster 3 starts at Sector 96 + (3-2)*8 = 96 + 8 = 104
        f.seek(104 * 512)
        f.write(b'Greetings from VEIL OS!')

    print("Disk Image Formatted as FAT32 with HELLO.TXT!")

format_fat32('fat32.img')
