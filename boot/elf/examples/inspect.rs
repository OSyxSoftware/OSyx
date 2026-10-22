//! Validates a kernel ELF with exactly the rules the boot loader applies.
//! Exit status 0 = accepted, 1 = rejected, 2 = usage / I/O error.
//!
//! cargo run -p osyx-elf --example inspect -- [--user] <file.elf>

use osyx_elf::{ElfImage, PF_R, PF_W, PF_X};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let user = args.iter().any(|a| a == "--user");
    let Some(path) = args.iter().find(|a| !a.starts_with("--")).cloned() else {
        eprintln!("usage: inspect [--user] <file.elf>");
        std::process::exit(2);
    };
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{path}: {e}");
            std::process::exit(2);
        }
    };
    let parsed = if user { ElfImage::parse_user(&data) } else { ElfImage::parse(&data) };
    match parsed {
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
