//! Prints what the OSyx PE parser finds in a PE32+ file. Read-only; the
//! file is never executed.
//!
//! cargo run -p osyx-pe --example pe-dump -- <file.exe|file.dll>

use osyx_pe::{dir, ExportTarget, ImportEntry, PeFile, RelocationKind};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: pe-dump <file>");
        std::process::exit(2);
    };
    let data = std::fs::read(&path).unwrap_or_else(|e| {
        eprintln!("{path}: {e}");
        std::process::exit(2)
    });
    let pe = match PeFile::parse(&data) {
        Ok(pe) => pe,
        Err(e) => {
            println!("{path}: REJECTED: {e}");
            std::process::exit(1);
        }
    };
    let o = pe.optional();
    println!("{path}: PE32+ {}", if pe.is_dll() { "DLL" } else { "executable" });
    println!("  machine {:#06x}  characteristics {:#06x}", pe.machine(), pe.coff().characteristics);
    println!("  image base {:#x}  size {:#x}  headers {:#x}", o.image_base, o.size_of_image, o.size_of_headers);
    println!(
        "  entry {:#x}  subsystem {}  dll characteristics {:#06x}",
        o.address_of_entry_point, o.subsystem, o.dll_characteristics
    );
    println!("  alignment: section {:#x}  file {:#x}", o.section_alignment, o.file_alignment);
    println!("  sections:");
    for s in pe.sections() {
        println!(
            "    {:<8} va {:#07x} vsize {:#07x} raw {:#07x}+{:#07x} {}{}{}",
            s.name(),
            s.virtual_address,
            s.virtual_size,
            s.pointer_to_raw_data,
            s.size_of_raw_data,
            if s.is_readable() { 'r' } else { '-' },
            if s.is_writable() { 'w' } else { '-' },
            if s.is_executable() { 'x' } else { '-' }
        );
    }
    println!("  data directories:");
    for i in 0..16 {
        let d = pe.directory(i);
        if d.is_present() {
            println!("    [{i:2}] rva {:#07x} size {:#x}", d.virtual_address, d.size);
        }
    }
    match pe.imports() {
        Ok(it) => {
            for imp in it {
                match imp {
                    Ok(imp) => {
                        println!("  import {} (IAT {:#x})", imp.dll, imp.address_table_rva);
                        for e in imp.entries() {
                            match e {
                                Ok(ImportEntry::ByName { hint, name, iat_rva }) => {
                                    println!("    {name} (hint {hint:#x}) -> slot {iat_rva:#x}")
                                }
                                Ok(ImportEntry::ByOrdinal { ordinal, iat_rva }) => {
                                    println!("    #{ordinal} -> slot {iat_rva:#x}")
                                }
                                Err(e) => println!("    ERROR: {e}"),
                            }
                        }
                    }
                    Err(e) => println!("  imports ERROR: {e}"),
                }
            }
        }
        Err(e) => println!("  imports ERROR: {e}"),
    }
    match pe.exports() {
        Ok(Some(ex)) => {
            println!(
                "  exports of {:?}: base {}, {} functions, {} names",
                ex.dll_name,
                ex.ordinal_base,
                ex.function_count(),
                ex.name_count()
            );
            if let Err(e) = ex.validate_names() {
                println!("    name table ERROR: {e}");
            }
            for e in ex.iter() {
                match e {
                    Ok(e) => match e.target {
                        ExportTarget::Rva(r) => {
                            println!("    #{} {} -> {r:#x}", e.ordinal, e.name.unwrap_or("<noname>"))
                        }
                        ExportTarget::Forwarder { raw, .. } => {
                            println!("    #{} {} -> forwarder {raw}", e.ordinal, e.name.unwrap_or("<noname>"))
                        }
                    },
                    Err(e) => println!("    ERROR: {e}"),
                }
            }
        }
        Ok(None) => {}
        Err(e) => println!("  exports ERROR: {e}"),
    }
    match pe.relocations() {
        Ok(it) => {
            let (mut dir64, mut abs, mut other, mut err) = (0, 0, 0, None);
            for r in it {
                match r {
                    Ok(r) => match r.kind {
                        RelocationKind::Dir64 => dir64 += 1,
                        RelocationKind::Absolute => abs += 1,
                        _ => other += 1,
                    },
                    Err(e) => err = Some(e),
                }
            }
            if pe.directory(dir::BASERELOC).is_present() {
                println!("  relocations: {dir64} DIR64, {abs} padding, {other} other");
            }
            if let Some(e) = err {
                println!("  relocations ERROR: {e}");
            }
        }
        Err(e) => println!("  relocations ERROR: {e}"),
    }
    match pe.tls() {
        Ok(Some(t)) => {
            println!(
                "  TLS: data {:#x}..{:#x} index {:#x} callbacks at {:#x}",
                t.raw_data_start_rva, t.raw_data_end_rva, t.index_rva, t.callbacks_rva
            );
            for cb in t.callbacks() {
                match cb {
                    Ok(rva) => println!("    callback {rva:#x}"),
                    Err(e) => println!("    ERROR: {e}"),
                }
            }
        }
        Ok(None) => {}
        Err(e) => println!("  TLS ERROR: {e}"),
    }
    match pe.runtime_functions() {
        Ok(t) if !t.is_empty() => {
            println!("  exception data: {} functions", t.len());
            for f in t.iter().take(8) {
                let info = t.unwind_info(&f).map(|u| format!("{u:?}")).unwrap_or_else(|e| e.to_string());
                println!("    {:#x}..{:#x} unwind {:#x}: {info}", f.begin, f.end, f.unwind_info);
            }
        }
        Ok(_) => {}
        Err(e) => println!("  exception data ERROR: {e}"),
    }
}
