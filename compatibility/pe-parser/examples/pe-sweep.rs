//! Robustness sweep: parses every `.dll`, `.exe` and `.sys` file in a
//! directory (not recursive) and walks every table the parser offers.
//! Read-only: files are read into memory, never executed or copied.
//! Build in debug mode so integer overflow panics are caught too.
//!
//! cargo run -p osyx-pe --example pe-sweep -- C:\Windows\System32

use osyx_pe::{ImportEntry, PeError, PeFile};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::panic;

thread_local! {
    static CURRENT: RefCell<String> = const { RefCell::new(String::new()) };
}

#[derive(Default)]
struct Stats {
    files: usize,
    parsed: usize,
    rejected: BTreeMap<String, usize>,
    panics: Vec<String>,
    imports: usize,
    import_dlls: usize,
    exports: usize,
    forwarders: usize,
    relocations: usize,
    tls_callbacks: usize,
    runtime_functions: usize,
    unwind_infos: usize,
    table_errors: BTreeMap<String, usize>,
}

fn kind(e: &PeError) -> String {
    let s = format!("{e:?}");
    s.split([' ', '(', '{']).next().unwrap_or("?").to_string()
        + &match e {
            PeError::BadImport { reason }
            | PeError::BadExport { reason }
            | PeError::BadRelocation { reason }
            | PeError::BadTls { reason }
            | PeError::BadExceptionData { reason }
            | PeError::BadSection { reason, .. } => format!(": {reason}"),
            _ => String::new(),
        }
}

fn walk(pe: &PeFile, st: &mut Stats) {
    let err = |e: PeError, st: &mut Stats| {
        let k = kind(&e);
        if std::env::var_os("PE_SWEEP_VERBOSE").is_some() && st.table_errors.get(&k).is_none_or(|n| *n < 3) {
            println!("  {}: {e}", CURRENT.with(|c| c.borrow().clone()));
        }
        *st.table_errors.entry(k).or_default() += 1
    };
    match pe.imports() {
        Ok(it) => {
            for imp in it {
                match imp {
                    Ok(imp) => {
                        st.import_dlls += 1;
                        for e in imp.entries() {
                            match e {
                                Ok(ImportEntry::ByName { .. } | ImportEntry::ByOrdinal { .. }) => st.imports += 1,
                                Err(e) => err(e, st),
                            }
                        }
                    }
                    Err(e) => err(e, st),
                }
            }
        }
        Err(e) => err(e, st),
    }
    match pe.exports() {
        Ok(Some(ex)) => {
            if let Err(e) = ex.validate_names() {
                err(e, st);
            }
            for f in ex.functions() {
                match f {
                    Ok((ordinal, target)) => {
                        st.exports += 1;
                        if matches!(target, osyx_pe::ExportTarget::Forwarder { .. }) {
                            st.forwarders += 1;
                        }
                        if ex.by_ordinal(ordinal).ok().flatten().map(|e| e.target) != Some(target) {
                            err(PeError::BadExport { reason: "by_ordinal lookup mismatch" }, st);
                        }
                    }
                    Err(e) => err(e, st),
                }
            }
            for n in ex.names() {
                match n {
                    Ok((name, ordinal)) => match ex.by_name(name) {
                        Ok(Some(found)) if found.ordinal == ordinal => {}
                        Ok(_) => err(PeError::BadExport { reason: "by_name lookup mismatch" }, st),
                        Err(e) => err(e, st),
                    },
                    Err(e) => err(e, st),
                }
            }
        }
        Ok(None) => {}
        Err(e) => err(e, st),
    }
    match pe.relocations() {
        Ok(it) => {
            for r in it {
                match r {
                    Ok(_) => st.relocations += 1,
                    Err(e) => err(e, st),
                }
            }
        }
        Err(e) => err(e, st),
    }
    match pe.tls() {
        Ok(Some(t)) => {
            let _ = t.template();
            for cb in t.callbacks() {
                match cb {
                    Ok(_) => st.tls_callbacks += 1,
                    Err(e) => err(e, st),
                }
            }
        }
        Ok(None) => {}
        Err(e) => err(e, st),
    }
    match pe.runtime_functions() {
        Ok(t) => {
            for f in t.iter() {
                st.runtime_functions += 1;
                match t.unwind_info(&f) {
                    Ok(_) => st.unwind_infos += 1,
                    Err(e) => err(e, st),
                }
                // Zero-length entries contain no address, so lookup skips them.
                if f.begin < f.end && t.lookup(f.begin).ok().flatten() != Some(f) {
                    err(PeError::BadExceptionData { reason: "lookup mismatch" }, st);
                }
            }
        }
        Err(e) => err(e, st),
    }
}

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: pe-sweep <directory>");
        std::process::exit(2);
    };
    panic::set_hook(Box::new(|_| {}));
    let mut st = Stats::default();
    let start = std::time::Instant::now();
    let mut entries: Vec<_> = std::fs::read_dir(&dir).expect("read_dir").flatten().map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        let ext = path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase());
        if !matches!(ext.as_deref(), Some("dll" | "exe" | "sys")) || !path.is_file() {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else { continue };
        CURRENT
            .with(|c| *c.borrow_mut() = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        st.files += 1;
        let mut local = Stats::default();
        let r = panic::catch_unwind(panic::AssertUnwindSafe(|| match PeFile::parse(&data) {
            Ok(pe) => {
                local.parsed += 1;
                walk(&pe, &mut local);
                None
            }
            Err(e) => Some(kind(&e)),
        }));
        match r {
            Ok(Some(reason)) => *st.rejected.entry(reason).or_default() += 1,
            Ok(None) => {
                st.parsed += local.parsed;
                st.imports += local.imports;
                st.import_dlls += local.import_dlls;
                st.exports += local.exports;
                st.forwarders += local.forwarders;
                st.relocations += local.relocations;
                st.tls_callbacks += local.tls_callbacks;
                st.runtime_functions += local.runtime_functions;
                st.unwind_infos += local.unwind_infos;
                for (k, v) in local.table_errors {
                    *st.table_errors.entry(k).or_default() += v;
                }
            }
            Err(_) => st.panics.push(path.display().to_string()),
        }
    }
    println!("{} files in {} ms", st.files, start.elapsed().as_millis());
    println!("parsed: {}", st.parsed);
    for (k, v) in &st.rejected {
        println!("rejected: {v:5}  {k}");
    }
    println!("panics: {}", st.panics.len());
    for p in st.panics.iter().take(20) {
        println!("  {p}");
    }
    println!(
        "walked: {} import DLLs / {} imports, {} exports ({} forwarders), {} relocations, {} TLS callbacks, {} runtime functions ({} unwind infos)",
        st.import_dlls, st.imports, st.exports, st.forwarders, st.relocations, st.tls_callbacks, st.runtime_functions, st.unwind_infos
    );
    for (k, v) in &st.table_errors {
        println!("table error: {v:5}  {k}");
    }
}
