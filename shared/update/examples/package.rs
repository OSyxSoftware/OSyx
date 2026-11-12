//! Publishing tool for OSyx packages (runs on the build host).
//!
//! cargo run -p osyx-update --example package -- build <name> <version> <description> <tree> <secret key file> <repository dir>
//! cargo run -p osyx-update --example package -- index <secret key file> <repository dir>
//!
//! `build` reads `<tree>`, which holds the package's files where they go
//! on the system (`opt/pkg/<name>/...` and, for a launcher entry,
//! `system/share/applications/pkg-<name>.app`), and writes
//! `<name>.pkg.txt`, `<name>.pkg.sig` and `<name>.pkg.bin` into the
//! repository directory. Files with an execute bit are installed 0755, the
//! others 0644.
//!
//! `index` lists every package of the repository directory in
//! `packages.txt` and signs it (`packages.sig`).
//!
//! The key is an Ed25519 secret key in hex, as `release keygen` makes it.
//! Systems accept the OSyx release key and the public keys their
//! administrator put in `/etc/osyx/packages.keys`.

use osyx_update::package::{self, Index, Listing, Package};
use osyx_update::{hex, parse_hex, public_from_secret, sign, Entry};
use sha2::{Digest, Sha256};
use std::path::Path;

fn read_secret(path: &str) -> Result<[u8; 32], String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    parse_hex::<32>(s.trim()).ok_or_else(|| format!("{path}: not a 64-digit hex Ed25519 secret key"))
}

/// Regular files below `dir`, as (path relative to `root`, mode, data),
/// sorted by path.
fn walk(root: &Path, dir: &Path, out: &mut Vec<(String, u32, Vec<u8>)>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.filter_map(Result::ok).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if meta.is_dir() {
            walk(root, &path, out)?;
        } else if meta.is_file() {
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?;
            let rel = rel.to_str().ok_or_else(|| format!("{}: the name is not UTF-8", path.display()))?.replace('\\', "/");
            #[cfg(unix)]
            let executable = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
            #[cfg(not(unix))]
            let executable = false;
            let data = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            out.push((rel, if executable { 0o755 } else { 0o644 }, data));
        } else {
            return Err(format!("{}: only files and directories can be packaged", path.display()));
        }
    }
    Ok(())
}

fn build(name: &str, version: &str, description: &str, tree: &str, secret_path: &str, out_dir: &str) -> Result<(), String> {
    let secret = read_secret(secret_path)?;
    let mut found = Vec::new();
    walk(Path::new(tree), Path::new(tree), &mut found)?;
    let mut payload = Vec::new();
    let mut files = Vec::new();
    for (path, mode, data) in found {
        files.push(Entry { path, size: data.len() as u64, mode, sha256: Sha256::digest(&data).into() });
        payload.extend_from_slice(&data);
    }
    let p = Package {
        name: name.to_string(),
        version: version.to_string(),
        description: description.to_string(),
        payload_size: payload.len() as u64,
        payload_sha256: Sha256::digest(&payload).into(),
        files,
    };
    // What a system will read: refuse here what it would refuse.
    let text = p.to_text();
    Package::parse(&text)?;
    let [manifest, signature, bin] = package::file_names(name);
    std::fs::create_dir_all(out_dir).map_err(|e| format!("{out_dir}: {e}"))?;
    let write = |file: &str, data: &[u8]| std::fs::write(Path::new(out_dir).join(file), data).map_err(|e| format!("{out_dir}/{file}: {e}"));
    write(&manifest, text.as_bytes())?;
    write(&signature, &sign(text.as_bytes(), &secret))?;
    write(&bin, &payload)?;
    println!("{name} {version}: {} files, {} bytes, signed with {}", p.files.len(), payload.len(), hex(&public_from_secret(&secret)));
    Ok(())
}

fn index(secret_path: &str, dir: &str) -> Result<(), String> {
    let secret = read_secret(secret_path)?;
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| format!("{dir}: {e}"))?
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".pkg.txt")).map(String::from))
        .collect();
    names.sort();
    let mut list = Index::default();
    for name in names {
        let path = Path::new(dir).join(format!("{name}.pkg.txt"));
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let p = Package::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if p.name != name {
            return Err(format!("{}: the manifest is of package {}", path.display(), p.name));
        }
        list.packages.push(Listing { name: p.name, version: p.version, description: p.description });
    }
    let text = list.to_text();
    Index::parse(&text)?;
    std::fs::write(Path::new(dir).join(package::INDEX), &text).map_err(|e| format!("{dir}: {e}"))?;
    std::fs::write(Path::new(dir).join(package::INDEX_SIGNATURE), sign(text.as_bytes(), &secret)).map_err(|e| format!("{dir}: {e}"))?;
    println!("{} packages listed, signed with {}", list.packages.len(), hex(&public_from_secret(&secret)));
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let result = match args.get(1).map(String::as_str) {
        Some("build") if args.len() == 8 => build(&args[2], &args[3], &args[4], &args[5], &args[6], &args[7]),
        Some("index") if args.len() == 4 => index(&args[2], &args[3]),
        _ => Err(String::from(
            "usage: package build <name> <version> <description> <tree> <secret key file> <repository dir>\n       package index <secret key file> <repository dir>",
        )),
    };
    if let Err(e) = result {
        eprintln!("package: {e}");
        std::process::exit(1);
    }
}
