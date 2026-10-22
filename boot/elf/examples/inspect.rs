//! Validates a kernel ELF with exactly the rules the boot loader applies.
//! Exit status 0 = accepted, 1 = rejected, 2 = usage / I/O error.
//!
//! cargo run -p osyx-elf --example inspect -- <kernel.elf>

use osyx_elf::{KernelImage, PF_R, PF_W, PF_X};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: inspect <kernel.elf>");
        std::process::exit(2);
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };
    match KernelImage::parse(&data) {
        Ok(k) => {
            println!("{path}: OK, entry {:#x}, {} bytes mapped", k.entry(), k.virt_span());
            for s in k.segments() {
                let f = |bit, c| if s.flags & bit != 0 { c } else { '-' };
                println!(
                    "  {:#018x} mem {:#9x} file {:#9x} {}{}{}",
                    s.vaddr,
                    s.mem_size,
                    s.file_size,
                    f(PF_R, 'r'),
                    f(PF_W, 'w'),
                    f(PF_X, 'x')
                );
            }
        }
        Err(e) => {
            println!("{path}: REJECTED: {} ({e:?})", e.describe());
            std::process::exit(1);
        }
    }
}
