//! Audit vendor PE imports against export/API-set tables printed by the
//! actual OSyx winrun binary (`winrun --exports`). No vendor code executes.
//! cargo run -p osyx-pe-loader --example dependency-audit -- DIR QEMU_LOG OUT
use osyx_pe::{ExportTarget, Import, ImportEntry, PeFile};
use osyx_pe_loader::apiset::hashed_part;
use osyx_pe_loader::link::normalize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Clone)]
enum Target {
    Local,
    Forward(String, String),
}
struct Need {
    dll: String,
    symbol: String,
    delay: bool,
}
struct Module {
    name: String,
    path: String,
    machine: u16,
    file_size: usize,
    image_size: u32,
    version_size: usize,
    needs: Vec<Need>,
    exports: BTreeMap<String, Target>,
}
#[derive(Default)]
struct Runtime {
    exports: BTreeMap<String, BTreeSet<String>>,
    apisets: BTreeMap<String, String>,
}
impl Runtime {
    fn parse(log: &str) -> Self {
        let mut result = Self::default();
        for line in log.lines() {
            for marker in ["WIN_EXPORT ", "WIN_ORDINAL ", "WIN_APISET "] {
                let Some(at) = line.find(marker) else { continue };
                let fields: Vec<_> = line[at + marker.len()..].split_whitespace().collect();
                if fields.len() != 2 {
                    continue;
                }
                if marker == "WIN_APISET " {
                    if let Some(key) = hashed_part(fields[0]) {
                        result.apisets.insert(key, normalize(fields[1]));
                    }
                } else {
                    result.exports.entry(normalize(fields[0])).or_default().insert(fields[1].to_string());
                }
            }
        }
        result
    }
    fn host(&self, dll: &str) -> Result<String, &'static str> {
        let name = normalize(dll);
        if name.starts_with("api-") || name.starts_with("ext-") {
            return hashed_part(&name)
                .and_then(|key| self.apisets.get(&key).cloned())
                .ok_or("missing_api_set_contract");
        }
        Ok(name)
    }
}
fn quote(text: &str) -> String {
    let mut s = String::from("\"");
    for c in text.chars() {
        match c {
            '\\' => s.push_str("\\\\"),
            '"' => s.push_str("\\\""),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if c < ' ' => s.push_str(&format!("\\u{:04x}", c as u32)),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}
fn files(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let path = entry.path();
        if kind.is_dir() {
            files(&path, out)?;
        } else if kind.is_file()
            && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dll") || e.eq_ignore_ascii_case("exe"))
        {
            out.push(path);
        }
    }
    Ok(())
}
fn add_import(needs: &mut Vec<Need>, d: Import<'_>, delay: bool) -> Result<(), Box<dyn std::error::Error>> {
    for symbol in d.entries() {
        let symbol = match symbol? {
            ImportEntry::ByName { name, .. } => name.to_string(),
            ImportEntry::ByOrdinal { ordinal, .. } => format!("#{ordinal}"),
        };
        needs.push(Need { dll: normalize(d.dll), symbol, delay });
    }
    Ok(())
}
fn target(t: ExportTarget<'_>) -> Target {
    match t {
        ExportTarget::Rva(_) => Target::Local,
        ExportTarget::Forwarder { dll, name, ordinal, .. } => Target::Forward(
            normalize(dll),
            name.map(str::to_string).unwrap_or_else(|| format!("#{}", ordinal.unwrap_or(0))),
        ),
    }
}
fn module(path: &Path, root: &Path) -> Result<Module, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(path)?;
    let pe = PeFile::parse(&bytes)?;
    let mut needs = Vec::new();
    let mut exports = BTreeMap::new();
    for d in pe.imports()? {
        add_import(&mut needs, d?, false)?;
    }
    for d in pe.delay_imports()? {
        add_import(&mut needs, d?.import, true)?;
    }
    if let Some(ex) = pe.exports()? {
        for entry in ex.functions() {
            let (ordinal, t) = entry?;
            exports.insert(format!("#{ordinal}"), target(t));
        }
        for entry in ex.names() {
            let (name, ordinal) = entry?;
            if let Some(t) = exports.get(&format!("#{ordinal}")).cloned() {
                exports.insert(name.to_string(), t);
            }
        }
    }
    Ok(Module {
        name: path.file_name().unwrap().to_string_lossy().to_lowercase(),
        path: path.strip_prefix(root)?.to_string_lossy().replace('\\', "/"),
        machine: pe.machine(),
        file_size: bytes.len(),
        image_size: pe.size_of_image(),
        version_size: pe.resource(16, None, None)?.map_or(0, <[u8]>::len),
        needs,
        exports,
    })
}
fn resolve(
    runtime: &Runtime,
    modules: &BTreeMap<String, Module>,
    dll: &str,
    symbol: &str,
    depth: u32,
) -> Result<(), &'static str> {
    if depth > 16 {
        return Err("forwarder_cycle_or_depth");
    }
    let host = runtime.host(dll)?;
    if let Some(exports) = runtime.exports.get(&host) {
        return if exports.contains(symbol) { Ok(()) } else { Err("missing_runtime_export") };
    }
    let m = modules.get(&host).ok_or("missing_dll")?;
    match m.exports.get(symbol).ok_or("missing_vendor_export")? {
        Target::Local => Ok(()),
        Target::Forward(d, s) => resolve(runtime, modules, d, s, depth + 1),
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("usage: dependency-audit DIRECTORY WINRUN_LOG OUTPUT.json".into());
    }
    let root = Path::new(&args[0]);
    let runtime = Runtime::parse(&std::fs::read_to_string(&args[1])?);
    if runtime.exports.is_empty() {
        return Err("no actual WIN_EXPORT manifest in supplied log; refusing to guess availability".into());
    }
    let mut paths = Vec::new();
    files(root, &mut paths)?;
    paths.sort();
    let mut modules = BTreeMap::new();
    let mut errors = Vec::new();
    let mut excluded = Vec::new();
    for path in paths {
        match module(&path, root) {
            Ok(m) => {
                // Keep nested copies separately; normal filename loading
                // searches the application's root directory, as winrun does.
                modules.insert(m.path.to_ascii_lowercase(), m);
            }
            Err(e)
                if matches!(
                    e.downcast_ref::<osyx_pe::PeError>(),
                    Some(osyx_pe::PeError::NotPe32Plus { magic: 0x10b })
                ) =>
            {
                excluded.push(format!(
                    "{}: PE32 helper, unsupported by x64-only runtime",
                    path.strip_prefix(root)?.display()
                ));
            }
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    let mut startup = BTreeSet::new();
    let mut queue = VecDeque::from([String::from("zen.exe")]);
    while let Some(name) = queue.pop_front() {
        if !startup.insert(name.clone()) {
            continue;
        }
        if let Some(m) = modules.get(&name) {
            for n in m.needs.iter().filter(|n| !n.delay) {
                if let Ok(host) = runtime.host(&n.dll) {
                    if !runtime.exports.contains_key(&host) && modules.contains_key(&host) {
                        queue.push_back(host);
                    }
                }
            }
        }
    }
    let mut gaps = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    let mut imports = 0usize;
    let mut delays = 0usize;
    let mut module_rows = Vec::new();
    for m in modules.values() {
        module_rows.push(format!("{{\"name\":{},\"path\":{},\"machine\":{},\"file_bytes\":{},\"image_bytes\":{},\"version_resource_bytes\":{},\"startup_vendor_dependency\":{}}}",quote(&m.name),quote(&m.path),m.machine,m.file_size,m.image_size,m.version_size,startup.contains(&m.path.to_ascii_lowercase())));
        for n in &m.needs {
            if n.delay {
                delays += 1;
            } else {
                imports += 1;
            }
            if let Err(reason) = resolve(&runtime, &modules, &n.dll, &n.symbol, 0) {
                *counts.entry(reason).or_default() += 1;
                gaps.push(format!(
                    "{{\"importer\":{},\"dll\":{},\"symbol\":{},\"delay\":{},\"startup_importer\":{},\"reason\":{}}}",
                    quote(&m.path),
                    quote(&n.dll),
                    quote(&n.symbol),
                    n.delay,
                    startup.contains(&m.path.to_ascii_lowercase()),
                    quote(reason)
                ));
            }
        }
    }
    let counts = counts.iter().map(|(k, v)| format!("{}:{v}", quote(k))).collect::<Vec<_>>().join(",");
    let excluded_json = excluded.iter().map(|s| quote(s)).collect::<Vec<_>>().join(",");
    let json=format!("{{\n\"scope\":\"package static and delay imports; dynamic LoadLibrary/GetProcAddress dependencies are additional\",\n\"symbol_presence_only\":true,\n\"browser_success\":false,\n\"module_count\":{},\n\"static_imports\":{imports},\n\"delay_imports\":{delays},\n\"gap_count\":{},\n\"gap_counts\":{{{counts}}},\n\"unsupported_pe32_helpers\":[{excluded_json}],\n\"parse_errors\":[{}],\n\"modules\":[{}],\n\"gaps\":[{}]\n}}\n",modules.len(),gaps.len(),errors.iter().map(|s|quote(s)).collect::<Vec<_>>().join(","),module_rows.join(",\n"),gaps.join(",\n"));
    std::fs::write(&args[2], json)?;
    println!("{} PE modules; {imports} static imports; {delays} delay imports; {} unresolved import occurrences; {} parse errors",modules.len(),gaps.len(),errors.len());
    if !errors.is_empty() {
        return Err("incomplete audit: inspect parse_errors".into());
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("dependency-audit: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capabilities_are_explicit_and_minor_versions_match() {
        let r = Runtime::parse(
            "[winrun:2] WIN_EXPORT version.dll VerQueryValueW\nWIN_APISET api-ms-win-core-version-l1-1-1 version.dll\n",
        );
        assert_eq!(r.host("api-ms-win-core-version-l1-1-0.dll").unwrap(), "version.dll");
        assert!(resolve(&r, &BTreeMap::new(), "version.dll", "VerQueryValueW", 0).is_ok());
        assert_eq!(resolve(&r, &BTreeMap::new(), "version.dll", "NoSuchApi", 0), Err("missing_runtime_export"));
        assert_eq!(r.host("api-ms-win-crt-runtime-l1-1-0.dll"), Err("missing_api_set_contract"));
        assert_eq!(resolve(&r, &BTreeMap::new(), "ucrtbase.dll", "malloc", 0), Err("missing_dll"));
    }
    #[test]
    fn json_escaping_is_valid_for_paths_and_control_characters() {
        assert_eq!(quote("C:\\a\n\u{1}\""), "\"C:\\\\a\\n\\u0001\\\"\"");
    }
}
