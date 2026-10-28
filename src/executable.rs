use crate::println;
use alloc::vec::Vec;

// -----| PE Headers |-----

#[repr(C, packed)]
struct DosHeader {
    magic: u16,           // "MZ" (0x5A4D)
    _reserved: [u8; 58],
    lfanew: u32,          // Offset to PE Header
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct ImageDataDirectory {
    pub virtual_address: u32,
    pub size: u32,
}

#[repr(C, packed)]
pub struct ImageImportDescriptor {
    pub original_first_thunk: u32,
    pub time_date_stamp: u32,
    pub forwarder_chain: u32,
    pub name: u32,
    pub first_thunk: u32,
}

#[repr(C, packed)]
pub struct ImageExportDirectory {
    pub characteristics: u32,
    pub time_date_stamp: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub name: u32,
    pub base: u32,
    pub number_of_functions: u32,
    pub number_of_names: u32,
    pub address_of_functions: u32,
    pub address_of_names: u32,
    pub address_of_name_ordinals: u32,
}

#[repr(C, packed)]
struct PeHeader {
    signature: u32,       // "PE\0\0" (0x00004550)
    machine: u16,
    num_sections: u16,
    time_date_stamp: u32,
    _reserved: [u32; 2],
    optional_header_size: u16,
    characteristics: u16,
}

#[repr(C, packed)]
struct OptionalHeader64 {
    magic: u16,           // 0x20B for PE32+ (x64)
    major_linker_version: u8,
    minor_linker_version: u8,
    size_of_code: u32,
    size_of_initialized_data: u32,
    size_of_uninitialized_data: u32,
    entry_point_address: u32,
    base_of_code: u32,
    image_base: u64,
    section_alignment: u32,
    file_alignment: u32,
    major_os_version: u16,
    minor_os_version: u16,
    major_image_version: u16,
    minor_image_version: u16,
    major_subsystem_version: u16,
    minor_subsystem_version: u16,
    _reserved1: u32,
    size_of_image: u32,
    size_of_headers: u32,
    check_sum: u32,
    subsystem: u16,
    dll_characteristics: u16,
    size_of_stack_reserve: u64,
    size_of_stack_commit: u64,
    size_of_heap_reserve: u64,
    size_of_heap_commit: u64,
    loader_flags: u32,
    num_rva_and_sizes: u32,
}

#[repr(C, packed)]
struct OptionalHeader32 {
    magic: u16,           // 0x10B for PE32
    major_linker_version: u8,
    minor_linker_version: u8,
    size_of_code: u32,
    size_of_initialized_data: u32,
    size_of_uninitialized_data: u32,
    entry_point_address: u32,
    base_of_code: u32,
    base_of_data: u32,
    image_base: u32,
    section_alignment: u32,
    file_alignment: u32,
    major_os_version: u16,
    minor_os_version: u16,
    major_image_version: u16,
    minor_image_version: u16,
    major_subsystem_version: u16,
    minor_subsystem_version: u16,
    _reserved1: u32,
    size_of_image: u32,
    size_of_headers: u32,
    check_sum: u32,
    subsystem: u16,
    dll_characteristics: u16,
    size_of_stack_reserve: u32,
    size_of_stack_commit: u32,
    size_of_heap_reserve: u32,
    size_of_heap_commit: u32,
    loader_flags: u32,
    num_rva_and_sizes: u32,
}

#[repr(C, packed)]
#[derive(Debug)]
pub struct SectionHeader {
    pub name: [u8; 8],
    pub virtual_size: u32,
    pub virtual_address: u32,
    pub size_of_raw_data: u32,
    pub pointer_to_raw_data: u32,
    pub pointer_to_relocations: u32,
    pub pointer_to_linenumbers: u32,
    pub num_relocations: u16,
    pub num_linenumbers: u16,
    pub characteristics: u32,
}

// -----| PE Parser |-----

pub struct ImportedDll {
    pub name: alloc::string::String,
    pub original_first_thunk: u32,
    pub first_thunk: u32,
}

pub struct ExportedFunction {
    pub name: Option<alloc::string::String>,
    pub rva: u32,
    pub ordinal: u16,
}

pub struct PeFile {
    pub entry_point: u64,
    pub image_base: u64,
    pub sections: Vec<SectionHeader>,
    pub imports: Vec<ImportedDll>,
    pub exports: Vec<ExportedFunction>,
    /// [start, end) RVA range of the export directory itself.
    ///
    /// An export whose RVA falls inside this range is a *forwarder*: the RVA
    /// points at an ASCII string like "NTDLL.RtlAllocateHeap", not at code.
    /// Linking an IAT slot straight to it makes the app execute the string.
    pub export_dir_range: Option<(u32, u32)>,
    /// Base RVA of the relocation directory and its size, if present.
    pub reloc_dir: Option<(u32, u32)>,
}

impl PeFile {
    pub fn rva_to_offset(sections: &[SectionHeader], rva: u32) -> Option<usize> {
        for sec in sections {
            let max_v_size = (sec.virtual_size + 4095) & !4095;
            if rva >= sec.virtual_address && rva < sec.virtual_address + max_v_size {
                let offset = rva - sec.virtual_address;
                if offset < sec.size_of_raw_data {
                    return Some((sec.pointer_to_raw_data + offset) as usize);
                } else {
                    // The RVA lands in the zero-filled tail of the section
                    // (virtual_size > size_of_raw_data).  There is no file
                    // data for it, but it is a legal address — callers treat
                    // None as "not found", which silently dropped imports and
                    // exports living in that region.
                    return None;
                }
            }
        }
        None
    }

    pub fn parse(data: &[u8]) -> Result<Self, &'static str> {
        crate::serial_println!("[PE_PARSE] Initiating parse of binary ({} bytes)", data.len());
        if data.len() < core::mem::size_of::<DosHeader>() {
            return Err("File too small for DOS header");
        }

        let dos = unsafe { &*(data.as_ptr() as *const DosHeader) };
        if dos.magic != 0x5A4D {
            return Err("Invalid DOS signature");
        }

        let pe_offset = dos.lfanew as usize;
        if data.len() < pe_offset + core::mem::size_of::<PeHeader>() {
            return Err("File too small for PE header");
        }

        let pe = unsafe { &*(data.as_ptr().add(pe_offset) as *const PeHeader) };
        if pe.signature != 0x00004550 {
            return Err("Invalid PE signature");
        }

        let opt_offset = pe_offset + core::mem::size_of::<PeHeader>();
        if data.len() < opt_offset + 2 {
            return Err("File too small for Optional header magic");
        }

        let magic = unsafe { core::ptr::read_unaligned(data.as_ptr().add(opt_offset) as *const u16) };
        
        let (entry_point, image_base, num_rvas) = if magic == 0x20B {
            if data.len() < opt_offset + core::mem::size_of::<OptionalHeader64>() {
                return Err("File too small for Optional header 64");
            }
            let opt = unsafe { &*(data.as_ptr().add(opt_offset) as *const OptionalHeader64) };
            (opt.entry_point_address as u64, opt.image_base, opt.num_rva_and_sizes)
        } else if magic == 0x10B {
            if data.len() < opt_offset + core::mem::size_of::<OptionalHeader32>() {
                return Err("File too small for Optional header 32");
            }
            let opt = unsafe { &*(data.as_ptr().add(opt_offset) as *const OptionalHeader32) };
            (opt.entry_point_address as u64, opt.image_base as u64, opt.num_rva_and_sizes)
        } else {
            return Err("Unsupported PE magic - not PE32 or PE32+");
        };

        // Parse Data Directories array immediately following OptionalHeader
        let opt_size = if magic == 0x20B { core::mem::size_of::<OptionalHeader64>() } else { core::mem::size_of::<OptionalHeader32>() };
        let data_dirs_offset = opt_offset + opt_size;
        
        // Import Directory is Index 1
        let mut import_dir_rva = 0;
        let mut export_dir_rva = 0;
        let mut export_dir_size: u32 = 0;
        let mut reloc_dir: Option<(u32, u32)> = None;

        if num_rvas > 0 {
            let offset = data_dirs_offset; // index 0: Exports
            if data.len() >= offset + core::mem::size_of::<ImageDataDirectory>() {
                let dir = unsafe { core::ptr::read_unaligned(data.as_ptr().add(offset) as *const ImageDataDirectory) };
                export_dir_rva  = dir.virtual_address;
                export_dir_size = dir.size;
            }
        }

        // Index 5: Base Relocation Table.
        if num_rvas > 5 {
            let offset = data_dirs_offset + 5 * core::mem::size_of::<ImageDataDirectory>();
            if data.len() >= offset + core::mem::size_of::<ImageDataDirectory>() {
                let dir = unsafe { core::ptr::read_unaligned(data.as_ptr().add(offset) as *const ImageDataDirectory) };
                if dir.virtual_address != 0 && dir.size != 0 {
                    reloc_dir = Some((dir.virtual_address, dir.size));
                }
            }
        }
        
        if num_rvas > 1 {
            let offset = data_dirs_offset + core::mem::size_of::<ImageDataDirectory>(); // index 1: Imports
            if data.len() >= offset + core::mem::size_of::<ImageDataDirectory>() {
                let dir = unsafe { core::ptr::read_unaligned(data.as_ptr().add(offset) as *const ImageDataDirectory) };
                import_dir_rva = dir.virtual_address;
            }
        }

        // `optional_header_size` comes straight off disk — a malformed or
        // hostile PE can point the section table past the end of the buffer.
        let opt_hdr_size = pe.optional_header_size as usize;
        if opt_hdr_size < opt_size || opt_offset + opt_hdr_size > data.len() {
            return Err("Optional header size out of range");
        }
        let section_offset = opt_offset + opt_hdr_size;
        let mut sections = Vec::new();
        for i in 0..pe.num_sections as usize {
            let offset = section_offset + i * core::mem::size_of::<SectionHeader>();
            if data.len() < offset + core::mem::size_of::<SectionHeader>() {
                break;
            }
            let section = unsafe { &*(data.as_ptr().add(offset) as *const SectionHeader) };
            sections.push(unsafe { core::ptr::read(section) });
        }

        // Parse Imports
        let mut imports = Vec::new();
        if import_dir_rva != 0 {
            if let Some(mut offset) = Self::rva_to_offset(&sections, import_dir_rva) {
                crate::serial_println!("[PE_PARSE] Identified Import Table at File Offset {:#X}", offset);
                loop {
                    if data.len() < offset + core::mem::size_of::<ImageImportDescriptor>() { break; }
                    let desc = unsafe { core::ptr::read_unaligned(data.as_ptr().add(offset) as *const ImageImportDescriptor) };
                    
                    // A null descriptor terminates the array
                    if desc.name == 0 && desc.first_thunk == 0 { break; }
                    
                    let mut name = alloc::string::String::new();
                    if let Some(name_off) = Self::rva_to_offset(&sections, desc.name) {
                        let mut i = name_off;
                        while i < data.len() && data[i] != 0 {
                            name.push(data[i] as char);
                            i += 1;
                        }
                    }
                    
                    imports.push(ImportedDll {
                        name,
                        original_first_thunk: desc.original_first_thunk,
                        first_thunk: desc.first_thunk,
                    });
                    
                    offset += core::mem::size_of::<ImageImportDescriptor>();
                }
            }
        }

        // Parse Exports
        //
        // Two passes: named exports (via the name-pointer table) and then
        // ordinal-only exports (entries in the address table with no name).
        // The old code only did the first pass, so every ordinal-only export
        // — common in system DLLs — was invisible to the IAT linker.
        let mut exports = Vec::new();
        if export_dir_rva != 0 {
            if let Some(offset) = Self::rva_to_offset(&sections, export_dir_rva) {
                if offset + core::mem::size_of::<ImageExportDirectory>() <= data.len() {
                let export_dir = unsafe {
                    core::ptr::read_unaligned(data.as_ptr().add(offset) as *const ImageExportDirectory)
                };
                let ord_base      = export_dir.base as u16;
                let num_funcs     = export_dir.number_of_functions as usize;
                let num_names     = export_dir.number_of_names as usize;
                let func_off_opt  = Self::rva_to_offset(&sections, export_dir.address_of_functions);

                // Pass 1 — named exports.
                let mut named_ordinals: Vec<u16> = Vec::new();
                if let (Some(func_off), Some(name_off), Some(ord_off)) = (
                    func_off_opt,
                    Self::rva_to_offset(&sections, export_dir.address_of_names),
                    Self::rva_to_offset(&sections, export_dir.address_of_name_ordinals)
                ) {
                    for i in 0..num_names {
                        if name_off + i*4 + 4 > data.len() { break; }
                        if ord_off  + i*2 + 2 > data.len() { break; }
                        let name_rva    = unsafe { core::ptr::read_unaligned(data.as_ptr().add(name_off + i*4) as *const u32) };
                        let ordinal_val = unsafe { core::ptr::read_unaligned(data.as_ptr().add(ord_off + i*2) as *const u16) };
                        if func_off + (ordinal_val as usize)*4 + 4 > data.len() { continue; }
                        let func_rva    = unsafe { core::ptr::read_unaligned(data.as_ptr().add(func_off + (ordinal_val as usize)*4) as *const u32) };
                        if func_rva == 0 { continue; }

                        let mut name = alloc::string::String::new();
                        if let Some(n_off) = Self::rva_to_offset(&sections, name_rva) {
                            let mut k = n_off;
                            while k < data.len() && data[k] != 0 {
                                name.push(data[k] as char);
                                k += 1;
                            }
                        }

                        named_ordinals.push(ordinal_val);
                        exports.push(ExportedFunction {
                            name: Some(name),
                            rva: func_rva,
                            ordinal: ordinal_val.wrapping_add(ord_base),
                        });
                    }
                }

                // Pass 2 — ordinal-only exports (no entry in the name table).
                if let Some(func_off) = func_off_opt {
                    for idx in 0..num_funcs {
                        if named_ordinals.contains(&(idx as u16)) { continue; }
                        if func_off + idx*4 + 4 > data.len() { break; }
                        let func_rva = unsafe { core::ptr::read_unaligned(data.as_ptr().add(func_off + idx*4) as *const u32) };
                        if func_rva == 0 { continue; }   // hole in the table
                        exports.push(ExportedFunction {
                            name: None,
                            rva: func_rva,
                            ordinal: (idx as u16).wrapping_add(ord_base),
                        });
                    }
                }
                }
            }
        }

        let export_dir_range = if export_dir_rva != 0 && export_dir_size != 0 {
            Some((export_dir_rva, export_dir_rva + export_dir_size))
        } else {
            None
        };

        Ok(PeFile {
            entry_point,
            image_base,
            sections,
            imports,
            exports,
            export_dir_range,
            reloc_dir,
        })
    }

    pub fn print_info(&self) {
        crate::serial_println!("PE File Info:");
        crate::serial_println!("  - Entry Point (RVA): {:#X}", self.entry_point);
        crate::serial_println!("  - Image Base: {:#018X}", self.image_base);
        crate::serial_println!("  - Sections:");
        for section in &self.sections {
            let name = core::str::from_utf8(&section.name).unwrap_or("Unknown").trim_matches('\0');
            let v_addr = section.virtual_address;
            let p_data = section.pointer_to_raw_data;
            crate::serial_println!("    - {:<8} (V: {:#010X}, P: {:#010X})", name, v_addr, p_data);
        }
        crate::serial_println!("  - Imports:");
        for imp in &self.imports {
            crate::serial_println!("    - {} (OFT: {:#X}, FT: {:#X})", imp.name, imp.original_first_thunk, imp.first_thunk);
        }
        crate::serial_println!("  - Exports ({}):", self.exports.len());
        for exp in self.exports.iter().take(10) {
            let name = exp.name.as_deref().unwrap_or("<ordinal>");
            crate::serial_println!("    - {} @ {:#X} (ord {})", name, exp.rva, exp.ordinal);
        }
        if self.exports.len() > 10 {
            crate::serial_println!("    - ... (trunc)");
        }
    }
}

// -----| ELF Headers |-----

#[repr(C, packed)]
struct ElfHeader64 {
    ident: [u8; 16],      // 0x7F 'E' 'L' 'F' ...
    file_type: u16,       // 1: Relocatable, 2: Executable, 3: Shared Object
    machine: u16,         // 0x3E: x86-64
    version: u32,
    entry_point: u64,
    ph_offset: u64,       // Program Header Table offset
    sh_offset: u64,       // Section Header Table offset
    flags: u32,
    header_size: u16,
    ph_entry_size: u16,
    ph_num: u16,          // Number of programs (segments)
    sh_entry_size: u16,
    sh_num: u16,
    sh_str_index: u16,
}

#[repr(C, packed)]
pub struct ElfProgramHeader64 {
    pub segment_type: u32,    // 1: LOAD, 2: DYNAMIC, etc.
    pub flags: u32,           // 1: X, 2: W, 4: R
    pub offset: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
    pub file_size: u64,
    pub mem_size: u64,
    pub alignment: u64,
}

// -----| ELF Parser |-----

pub struct ElfFile {
    pub entry_point: u64,
    pub segments: Vec<ElfProgramHeader64>,
}

impl ElfFile {
    pub fn parse(data: &[u8]) -> Result<Self, &'static str> {
        if data.len() < core::mem::size_of::<ElfHeader64>() {
            return Err("File too small for ELF header");
        }

        let header = unsafe { &*(data.as_ptr() as *const ElfHeader64) };
        if header.ident[0..4] != [0x7F, b'E', b'L', b'F'] {
            return Err("Invalid ELF signature");
        }

        if header.ident[4] != 2 {
            return Err("Not a 64-bit ELF");
        }

        let mut segments = Vec::new();
        for i in 0..header.ph_num as usize {
            let offset = header.ph_offset as usize + i * header.ph_entry_size as usize;
            if data.len() < offset + core::mem::size_of::<ElfProgramHeader64>() {
                break;
            }
            let program = unsafe { &*(data.as_ptr().add(offset) as *const ElfProgramHeader64) };
            segments.push(unsafe { core::ptr::read(program) });
        }

        Ok(ElfFile {
            entry_point: header.entry_point,
            segments,
        })
    }

    pub fn print_info(&self) {
        println!("ELF File Info:");
        println!("  - Entry Point: {:#018X}", self.entry_point);
        println!("  - Program Segments:");
        for (i, segment) in self.segments.iter().enumerate() {
            if segment.segment_type == 1 { // PT_LOAD
                let v_addr = segment.virtual_address;
                let p_addr = segment.physical_address;
                let f_size = segment.file_size;
                println!("    - {:2} LOAD (V: {:#010X}, P: {:#010X}, S: {:#X})", i, v_addr, p_addr, f_size);
            }
        }
    }
}
