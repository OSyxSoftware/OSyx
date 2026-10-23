//! The module graph: loads an executable and its DLLs, binds imports
//! (by name and by ordinal), follows export forwarders and API set
//! contracts, and computes the initialisation order.
//!
//! Where modules come from and where they go is the [`Host`]'s business:
//! it opens a DLL by name (an OSyx built-in module implemented by the
//! runtime, or a PE file from the VFS), reserves address space for an
//! image and commits the finished image with its protections. That keeps
//! this crate free of system calls and testable on the build host.
//!
//! Rules (Windows-compatible where it matters for real programs):
//! * DLL names compare ASCII case-insensitively; `.dll` is implied when the
//!   name has no extension;
//! * `api-*` / `ext-*` names go through the API set schema, with the
//!   importing module as the importer;
//! * import cycles are legal (a module already being loaded is bound
//!   against as is: export addresses do not depend on its imports) and are
//!   reported; initialisation follows dependency order, cycles broken at
//!   the edge that closes them;
//! * forwarder chains are bounded ([`MAX_FORWARD_DEPTH`]).

use crate::apiset::{self, Schema};
use crate::image::{self, ImageError, Protection};
use alloc::boxed::Box;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use osyx_pe::{ExportTarget, ImportEntry, PeFile};

pub const MAX_FORWARD_DEPTH: u32 = 16;
/// An import to bind: (name, or None for by-ordinal), ordinal, IAT slot RVA.
type WantedSymbol = (Option<String>, u16, u32);
pub const MAX_MODULES: usize = 256;

/// A module's exports when the runtime implements it (no PE file).
pub trait BuiltinModule {
    /// Address of export `name`, if implemented.
    fn by_name(&self, name: &str) -> Option<u64>;
    /// Address of export `ordinal`, if implemented.
    fn by_ordinal(&self, _ordinal: u16) -> Option<u64> {
        None
    }
}

pub enum Source {
    Builtin(Box<dyn BuiltinModule>),
    /// The bytes of a PE32+ DLL.
    Pe(Vec<u8>),
}

pub trait Host {
    /// Opens DLL `name` (lower-case, with extension). `Ok(None)`: not found.
    fn open(&mut self, name: &str) -> Result<Option<Source>, LinkError>;
    /// Reserves `size` bytes for an image linked at `preferred`. With
    /// `fixed` the image cannot be relocated and must go exactly there (or
    /// the host fails); otherwise `preferred` is only a hint. Returns the
    /// base actually used.
    fn reserve(&mut self, size: u64, preferred: u64, fixed: bool) -> Result<u64, LinkError>;
    /// Copies the finished image to `base` and applies the protections
    /// (RVAs relative to `base`).
    fn commit(&mut self, base: u64, image: &[u8], prot: &[Protection]) -> Result<(), LinkError>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    Image { module: String, error: ImageError },
    DllNotFound { name: String, importer: String },
    /// An API set contract with no host on this system.
    NoApiSetHost { name: String, importer: String },
    SymbolNotFound { module: String, symbol: String, importer: String },
    ForwarderTooDeep { module: String, symbol: String },
    TooManyModules,
    /// The host could not provide memory.
    NoMemory,
    Host(String),
}

impl core::fmt::Display for LinkError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LinkError::Image { module, error } => write!(f, "{module}: invalid image: {error:?}"),
            LinkError::DllNotFound { name, importer } => write!(f, "{name} (needed by {importer}) was not found"),
            LinkError::NoApiSetHost { name, importer } => write!(f, "API set {name} (needed by {importer}) has no host"),
            LinkError::SymbolNotFound { module, symbol, importer } => write!(f, "{module}!{symbol} (needed by {importer}) does not exist"),
            LinkError::ForwarderTooDeep { module, symbol } => write!(f, "forwarder chain from {module}!{symbol} is too long or circular"),
            LinkError::TooManyModules => f.write_str("too many modules"),
            LinkError::NoMemory => f.write_str("out of memory"),
            LinkError::Host(s) => f.write_str(s),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Imports are being bound (part of a cycle if seen again).
    Binding,
    Ready,
}

pub enum Kind {
    Builtin(Box<dyn BuiltinModule>),
    Pe {
        file: Vec<u8>,
        size: u32,
        entry: u32,
        is_dll: bool,
    },
}

pub struct Module {
    /// Lower-case file name (`kernel32.dll`).
    pub name: String,
    pub base: u64,
    pub kind: Kind,
    pub deps: Vec<usize>,
    pub state: State,
    /// Base relocations applied and imports bound (diagnostics).
    pub relocations: usize,
    pub imports_bound: usize,
}

impl Module {
    pub fn is_builtin(&self) -> bool {
        matches!(self.kind, Kind::Builtin(_))
    }

    /// Entry point address (0 for built-in modules or no entry point).
    pub fn entry(&self) -> u64 {
        match self.kind {
            Kind::Pe { entry, .. } if entry != 0 => self.base + entry as u64,
            _ => 0,
        }
    }

    pub fn pe(&self) -> Option<PeFile<'_>> {
        match &self.kind {
            Kind::Pe { file, .. } => PeFile::parse(file).ok(),
            Kind::Builtin(_) => None,
        }
    }
}

/// Symbol reference used in errors and lookups.
#[derive(Clone, Copy, Debug)]
pub enum Symbol<'a> {
    Name(&'a str),
    Ordinal(u16),
}

impl Symbol<'_> {
    fn describe(&self) -> String {
        match self {
            Symbol::Name(n) => n.to_string(),
            Symbol::Ordinal(o) => alloc::format!("#{o}"),
        }
    }
}

/// Normalises a DLL reference: lower-case, `.dll` appended when the name
/// has no extension.
pub fn normalize(name: &str) -> String {
    let mut n = name.to_ascii_lowercase();
    if !n.contains('.') {
        n.push_str(".dll");
    }
    n
}

pub struct Linker<'s, H: Host> {
    pub host: H,
    pub modules: Vec<Module>,
    schema: Option<Schema<'s>>,
    /// (importer, imported) edges that closed a cycle.
    pub cycles: Vec<(String, String)>,
}

impl<'s, H: Host> Linker<'s, H> {
    pub fn new(host: H, schema: Option<Schema<'s>>) -> Self {
        Linker { host, modules: Vec::new(), schema, cycles: Vec::new() }
    }

    pub fn find(&self, name: &str) -> Option<usize> {
        let n = normalize(name);
        self.modules.iter().position(|m| m.name == n)
    }

    /// Resolves an API set contract to a DLL name; other names unchanged.
    fn map_name(&self, name: &str, importer: &str) -> Result<String, LinkError> {
        if apiset::is_api_set_name(name) {
            let host = self.schema.as_ref().and_then(|s| s.resolve(name, Some(importer)).ok().flatten());
            return match host {
                Some(h) => Ok(normalize(&h)),
                None => Err(LinkError::NoApiSetHost { name: normalize(name), importer: importer.to_string() }),
            };
        }
        Ok(normalize(name))
    }

    /// Loads the main executable from its bytes. Returns its index.
    pub fn load_executable(&mut self, name: &str, file: Vec<u8>) -> Result<usize, LinkError> {
        self.load_pe(normalize(name), file, false)
    }

    /// Loads (or finds) DLL `name` as needed by `importer`.
    pub fn load_dll(&mut self, name: &str, importer: &str) -> Result<usize, LinkError> {
        let real = self.map_name(name, importer)?;
        if let Some(i) = self.modules.iter().position(|m| m.name == real) {
            if self.modules[i].state == State::Binding {
                self.cycles.push((importer.to_string(), real));
            }
            return Ok(i);
        }
        match self.host.open(&real)? {
            None => Err(LinkError::DllNotFound { name: real, importer: importer.to_string() }),
            Some(Source::Builtin(b)) => {
                if self.modules.len() >= MAX_MODULES {
                    return Err(LinkError::TooManyModules);
                }
                self.modules.push(Module { name: real, base: 0, kind: Kind::Builtin(b), deps: Vec::new(), state: State::Ready, relocations: 0, imports_bound: 0 });
                Ok(self.modules.len() - 1)
            }
            Some(Source::Pe(bytes)) => self.load_pe(real, bytes, true),
        }
    }

    fn load_pe(&mut self, name: String, file: Vec<u8>, dll: bool) -> Result<usize, LinkError> {
        if self.modules.len() >= MAX_MODULES {
            return Err(LinkError::TooManyModules);
        }
        let bad = |name: &str, error| LinkError::Image { module: name.to_string(), error };
        let pe = PeFile::parse(&file).map_err(|e| bad(&name, e.into()))?;
        image::check(&pe, dll).map_err(|e| bad(&name, e))?;
        if dll != pe.is_dll() {
            return Err(bad(&name, ImageError::Subsystem(pe.optional().subsystem)));
        }
        let size = pe.size_of_image();
        let base = self.host.reserve(size as u64, pe.image_base(), !image::relocatable(&pe))?;
        let mut img = image::layout(&pe).map_err(|e| bad(&name, e))?;
        let relocations = image::relocate(&pe, &mut img, base).map_err(|e| bad(&name, e))?;
        let prot = image::protections(&pe).map_err(|e| bad(&name, e))?;
        let entry = pe.entry_point();
        let idx = self.modules.len();
        // The module is registered before its imports are bound so that a
        // cycle back to it finds it (and its exports) instead of loading it twice.
        self.modules.push(Module {
            name: name.clone(),
            base,
            kind: Kind::Pe { file: Vec::new(), size, entry, is_dll: dll },
            deps: Vec::new(),
            state: State::Binding,
            relocations,
            imports_bound: 0,
        });
        // Collect the imports first (they borrow `file`), then move the
        // file into the module so forwarders into it can be resolved.
        let mut wanted: Vec<(String, Vec<WantedSymbol>)> = Vec::new();
        for d in pe.imports().map_err(|e| bad(&name, e.into()))? {
            let d = d.map_err(|e| bad(&name, e.into()))?;
            let mut entries = Vec::new();
            for e in d.entries() {
                match e.map_err(|e| bad(&name, e.into()))? {
                    ImportEntry::ByName { name: n, iat_rva, .. } => entries.push((Some(n.to_string()), 0, iat_rva)),
                    ImportEntry::ByOrdinal { ordinal, iat_rva } => entries.push((None, ordinal, iat_rva)),
                }
            }
            wanted.push((d.dll.to_string(), entries));
        }
        if let Kind::Pe { file: f, .. } = &mut self.modules[idx].kind {
            *f = file;
        }
        let mut bound = 0;
        for (dll_name, entries) in wanted {
            let dep = self.load_dll(&dll_name, &name)?;
            if !self.modules[idx].deps.contains(&dep) {
                self.modules[idx].deps.push(dep);
            }
            for (sym, ordinal, iat) in entries {
                let s = match &sym {
                    Some(n) => Symbol::Name(n),
                    None => Symbol::Ordinal(ordinal),
                };
                let addr = self.resolve(dep, s, &name, 0)?;
                image::write_u64(&mut img, iat, addr).ok_or_else(|| bad(&name, ImageError::TooLarge))?;
                bound += 1;
            }
        }
        self.host.commit(base, &img, &prot)?;
        let m = &mut self.modules[idx];
        m.imports_bound = bound;
        m.state = State::Ready;
        Ok(idx)
    }

    /// Address of `sym` exported by module `idx`, following forwarders.
    pub fn resolve(&mut self, idx: usize, sym: Symbol<'_>, importer: &str, depth: u32) -> Result<u64, LinkError> {
        let not_found = |m: &Module| LinkError::SymbolNotFound { module: m.name.clone(), symbol: sym.describe(), importer: importer.to_string() };
        if depth > MAX_FORWARD_DEPTH {
            return Err(LinkError::ForwarderTooDeep { module: self.modules[idx].name.clone(), symbol: sym.describe() });
        }
        let m = &self.modules[idx];
        let forward = match &m.kind {
            Kind::Builtin(b) => {
                let a = match sym {
                    Symbol::Name(n) => b.by_name(n),
                    Symbol::Ordinal(o) => b.by_ordinal(o),
                };
                return a.ok_or_else(|| not_found(m));
            }
            Kind::Pe { file, .. } => {
                let pe = PeFile::parse(file).map_err(|e| LinkError::Image { module: m.name.clone(), error: e.into() })?;
                let exports = pe.exports().ok().flatten().ok_or_else(|| not_found(m))?;
                let e = match sym {
                    Symbol::Name(n) => exports.by_name(n),
                    Symbol::Ordinal(o) => exports.by_ordinal(o),
                };
                let e = e.ok().flatten().ok_or_else(|| not_found(m))?;
                match e.target {
                    ExportTarget::Rva(rva) => return Ok(m.base + rva as u64),
                    ExportTarget::Forwarder { dll, name, ordinal, .. } => (dll.to_string(), name.map(|s| s.to_string()), ordinal),
                }
            }
        };
        let from = self.modules[idx].name.clone();
        let (dll, name, ordinal) = forward;
        let target = self.load_dll(&dll, &from)?;
        let s = match (&name, ordinal) {
            (Some(n), _) => Symbol::Name(n),
            (None, Some(o)) => Symbol::Ordinal(o),
            (None, None) => return Err(not_found(&self.modules[idx])),
        };
        self.resolve(target, s, importer, depth + 1)
    }

    /// Initialisation order: every module after the modules it imports
    /// (depth-first post-order from `root`; an edge back into a module still
    /// on the path is skipped).
    pub fn init_order(&self, root: usize) -> Vec<usize> {
        let mut order = Vec::new();
        let mut mark = alloc::vec![0u8; self.modules.len()]; // 0 new, 1 on path, 2 done
        let mut stack: Vec<(usize, usize)> = alloc::vec![(root, 0)];
        mark[root] = 1;
        while let Some((m, i)) = stack.pop() {
            if let Some(&d) = self.modules[m].deps.get(i) {
                stack.push((m, i + 1));
                if mark[d] == 0 {
                    mark[d] = 1;
                    stack.push((d, 0));
                }
            } else {
                mark[m] = 2;
                order.push(m);
            }
        }
        order
    }
}
