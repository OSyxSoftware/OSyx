//! Host tests of the loader core against the PE fixtures of osyx-pe
//! (`hello.exe`, `sample.dll`, built from source by
//! scripts/gen-pe-fixtures.ps1) and synthetic API set schemas.

use osyx_pe::PeFile;
use osyx_pe_loader::apiset::{self, ApiSetError, Schema, SchemaBuilder};
use osyx_pe_loader::image::{self, Protection};
use osyx_pe_loader::link::{normalize, BuiltinModule, Host, LinkError, Linker, PeMapping, Source, Symbol};
use std::collections::BTreeMap;

const HELLO: &[u8] = include_bytes!("../../pe-parser/tests/fixtures/hello.exe");
const SAMPLE: &[u8] = include_bytes!("../../pe-parser/tests/fixtures/sample.dll");

// ---- API set schema ---------------------------------------------------------

fn schema_bytes() -> Vec<u8> {
    let mut b = SchemaBuilder::new();
    b.add("api-ms-win-core-file-l1-2-5", "kernel32.dll", &[("kernel32.dll", "kernelbase.dll")]).unwrap();
    b.add("api-ms-win-core-sysinfo-l1-1-0", "kernel32.dll", &[]).unwrap();
    b.add("api-ms-win-core-synch-l1-2-0", "kernelbase.dll", &[]).unwrap();
    b.add("ext-ms-win-missing-l1-1-0.dll", "", &[]).unwrap();
    b.build()
}

#[test]
fn apiset_resolves_minor_revisions_and_importers() {
    let ns = schema_bytes();
    let s = Schema::parse(&ns).unwrap();
    assert_eq!(s.len(), 4);
    // Any revision of l1-2 finds the entry; case and ".dll" do not matter.
    assert_eq!(s.resolve("api-ms-win-core-file-l1-2-0.dll", None).unwrap().as_deref(), Some("kernel32.dll"));
    assert_eq!(s.resolve("API-MS-WIN-CORE-FILE-L1-2-9", Some("user32.dll")).unwrap().as_deref(), Some("kernel32.dll"));
    // kernel32 importing the contract gets kernelbase (no self-import).
    assert_eq!(s.resolve("api-ms-win-core-file-l1-2-5.dll", Some("KERNEL32.DLL")).unwrap().as_deref(), Some("kernelbase.dll"));
    // A different major contract is a different entry.
    assert_eq!(s.resolve("api-ms-win-core-file-l1-1-0", None).unwrap(), None);
    assert_eq!(s.resolve("ext-ms-win-missing-l1-1-0", None).unwrap(), None, "contract without a host");
    assert_eq!(s.resolve("api-ms-win-core-synch-l1-2-0", None).unwrap().as_deref(), Some("kernelbase.dll"));
    assert!(apiset::is_api_set_name("API-ms-x-l1-1-0.dll") && apiset::is_api_set_name("ext-ms-y-l1-1-0"));
    assert!(!apiset::is_api_set_name("kernel32.dll"));
    assert_eq!(apiset::hashed_part("api-ms-win-core-file-l1-2-5.dll").as_deref(), Some("api-ms-win-core-file-l1-2"));
}

#[test]
fn apiset_rejects_malformed_namespaces() {
    let ns = schema_bytes();
    assert_eq!(Schema::parse(&ns[..10]).unwrap_err(), ApiSetError::Truncated);
    let mut v = ns.clone();
    v[0] = 5;
    assert_eq!(Schema::parse(&v).unwrap_err(), ApiSetError::BadVersion(5));
    let mut v = ns.clone();
    v[12..16].copy_from_slice(&1_000_000u32.to_le_bytes());
    assert!(Schema::parse(&v).is_err(), "huge entry count");
    // Every truncation and every single-byte corruption: no panic, and a
    // namespace that still parses still answers queries.
    for n in 0..ns.len() {
        let _ = Schema::parse(&ns[..n]);
    }
    for i in 0..ns.len() {
        for val in [0u8, 0x7F, 0xFF] {
            let mut v = ns.clone();
            v[i] = val;
            if let Ok(s) = Schema::parse(&v) {
                let _ = s.resolve("api-ms-win-core-file-l1-2-5", Some("kernel32.dll"));
                let _ = s.resolve("ext-ms-win-missing-l1-1-0", None);
            }
        }
    }
    assert!(SchemaBuilder::new().add("kernel32.dll", "x.dll", &[]).is_err(), "not a contract name");
}

/// The host's own schema, when there is one (read only): every entry must
/// be found again through the hash table, which validates the lookup
/// algorithm against real data without assuming its size.
#[test]
fn apiset_parses_the_host_schema_if_present() {
    let path = std::path::Path::new(r"C:\Windows\System32\apisetschema.dll");
    let Ok(data) = std::fs::read(path) else {
        eprintln!("no host schema; skipped");
        return;
    };
    let pe = PeFile::parse(&data).unwrap();
    let sec = pe.sections().find(|s| s.name() == ".apiset").unwrap();
    let ns = &data[sec.pointer_to_raw_data as usize..][..sec.size_of_raw_data as usize];
    let s = Schema::parse(ns).unwrap();
    assert!(s.len() > 100);
    let mut resolved = 0;
    for i in 0..s.len() {
        let name = s.name(i).unwrap();
        // Bumping the minor version must not change the answer.
        let host = s.resolve(&name, None).unwrap();
        let bumped = format!("{}-99", &name[..name.rfind('-').unwrap()]);
        assert_eq!(s.resolve(&bumped, None).unwrap(), host, "{name}");
        resolved += host.is_some() as u32;
    }
    eprintln!("host schema: {} contracts, {resolved} with a host", s.len());
    assert!(resolved > 0);
}

// ---- linking ----------------------------------------------------------------

/// A built-in module whose exports are fake addresses.
struct Fake(BTreeMap<String, u64>, BTreeMap<u16, u64>);

impl BuiltinModule for Fake {
    fn by_name(&self, name: &str) -> Option<u64> {
        self.0.get(name).copied()
    }
    fn by_ordinal(&self, o: u16) -> Option<u64> {
        self.1.get(&o).copied()
    }
}

fn kernel32() -> Fake {
    let names = [("GetStdHandle", 0x7000_0010), ("WriteFile", 0x7000_0020), ("ExitProcess", 0x7000_0030), ("GetTickCount", 0x7000_0040)];
    Fake(names.iter().map(|(n, a)| (n.to_string(), *a)).collect(), [(42u16, 0x7000_0042u64)].into_iter().collect())
}

/// Images offered by `publish`, shared by the "processes" of a test:
/// name -> (base, laid out and relocated bytes).
type SharedImages = std::rc::Rc<std::cell::RefCell<BTreeMap<String, (u64, Vec<u8>)>>>;

#[derive(Default)]
struct MemHost {
    next: u64,
    files: BTreeMap<String, Vec<u8>>,
    images: BTreeMap<u64, (Vec<u8>, Vec<Protection>)>,
    opened: Vec<String>,
    released: Vec<u64>,
    map_files: bool,
    shared: Option<SharedImages>,
    reused: Vec<String>,
}

struct MappedFile(Vec<u8>);

impl PeMapping for MappedFile {
    fn bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Host for MemHost {
    type Image = Vec<u8>;

    fn release(&mut self, base: u64, _size: u64) {
        self.images.remove(&base);
        self.released.push(base);
    }
    fn open(&mut self, name: &str) -> Result<Option<Source>, LinkError> {
        self.opened.push(name.to_string());
        if name == "kernel32.dll" {
            return Ok(Some(Source::Builtin(Box::new(kernel32()))));
        }
        Ok(self.files.get(name).cloned().map(|bytes| {
            if self.map_files {
                Source::PeMapped(Box::new(MappedFile(bytes)))
            } else {
                Source::Pe(bytes)
            }
        }))
    }
    fn shared_base(&mut self, name: &str) -> Option<u64> {
        self.shared.as_ref()?.borrow().get(name).map(|(base, _)| *base)
    }
    fn prepare(&mut self, name: &str, base: u64, image: &mut [u8]) -> bool {
        let Some(shared) = &self.shared else { return false };
        match shared.borrow().get(name) {
            Some((b, bytes)) if *b == base && bytes.len() == image.len() => {
                image.copy_from_slice(bytes);
                self.reused.push(name.to_string());
                true
            }
            _ => false,
        }
    }
    fn publish(&mut self, name: &str, base: u64, image: &[u8]) {
        if let Some(shared) = &self.shared {
            shared.borrow_mut().entry(name.to_string()).or_insert_with(|| (base, image.to_vec()));
        }
    }
    fn reserve(&mut self, size: u64, preferred: u64, fixed: bool) -> Result<(u64, Self::Image), LinkError> {
        let base = if fixed || self.shared.as_ref().is_some_and(|s| s.borrow().values().any(|(b, _)| *b == preferred)) {
            preferred
        } else {
            // Never the linked base: every image is relocated.
            let base = 0x5_0000_0000 + self.next;
            self.next += size.next_multiple_of(0x10000);
            base
        };
        Ok((base, vec![0; size as usize]))
    }
    fn commit(&mut self, base: u64, image: Self::Image, prot: &[Protection]) -> Result<(), LinkError> {
        self.images.insert(base, (image, prot.to_vec()));
        Ok(())
    }
}

#[test]
fn hello_binds_its_kernel32_imports() {
    let mut l = Linker::new(MemHost::default(), None);
    let exe = l.load_executable("HELLO.EXE", HELLO.to_vec()).unwrap();
    let m = &l.modules[exe];
    assert_eq!(m.name, "hello.exe");
    assert_eq!(m.imports_bound, 3);
    let pe = PeFile::parse(HELLO).unwrap();
    assert_eq!(m.entry(), m.base + pe.entry_point() as u64);
    let (img, prot) = &l.host.images[&m.base];
    // Every IAT slot holds the built-in's address.
    let mut seen = Vec::new();
    for d in pe.imports().unwrap() {
        for e in d.unwrap().entries() {
            let e = e.unwrap();
            seen.push(image::read_u64(img, e.iat_rva()).unwrap());
        }
    }
    seen.sort();
    assert_eq!(seen, [0x7000_0010, 0x7000_0020, 0x7000_0030]);
    // W^X: code is executable and not writable; headers are read-only.
    assert!(prot.iter().all(|p| !(p.write && p.execute)));
    let text = pe.sections().find(|s| s.name() == ".text").unwrap();
    let p = prot.iter().find(|p| p.rva <= text.virtual_address && text.virtual_address < p.rva + p.size).unwrap();
    assert!(p.execute && !p.write);
    assert!(prot[0].rva == 0 && prot[0].read && !prot[0].write && !prot[0].execute);
    assert_eq!(l.init_order(exe), [1, 0], "kernel32 before the program");
}

#[test]
fn sample_dll_relocations_exports_and_forwarders() {
    let mut host = MemHost::default();
    host.files.insert("sample.dll".into(), SAMPLE.to_vec());
    let mut l = Linker::new(host, None);
    let dll = l.load_dll("Sample", "test.exe").unwrap();
    let base = l.modules[dll].base;
    let pe = PeFile::parse(SAMPLE).unwrap();
    assert!(l.modules[dll].relocations > 0, "loaded away from its linked base");
    // counter_pointer holds an absolute address: relocated by the delta.
    let img = &l.host.images[&base].0.clone();
    let exports = pe.exports().unwrap().unwrap();
    let cp = exports.by_name("add_numbers").unwrap().unwrap();
    let osyx_pe::ExportTarget::Rva(add_rva) = cp.target else { panic!() };
    assert_eq!(l.resolve(dll, Symbol::Name("add_numbers"), "t", 0).unwrap(), base + add_rva as u64);
    let delta = base.wrapping_sub(pe.image_base());
    let mut checked = 0;
    for r in pe.relocations().unwrap() {
        let r = r.unwrap();
        if r.kind == osyx_pe::RelocationKind::Dir64 {
            let orig = u64::from_le_bytes(pe.read_mapped::<8>(r.rva).unwrap());
            assert_eq!(image::read_u64(img, r.rva).unwrap(), orig.wrapping_add(delta));
            checked += 1;
        }
    }
    assert!(checked > 0);
    // Forwarders into kernel32 (by name and by ordinal); ordinal-only export.
    assert_eq!(l.resolve(dll, Symbol::Name("forwarded_tick"), "t", 0).unwrap(), 0x7000_0040);
    assert_eq!(l.resolve(dll, Symbol::Name("forwarded_by_ordinal"), "t", 0).unwrap(), 0x7000_0042);
    let ord = exports.by_ordinal(7).unwrap().unwrap();
    let osyx_pe::ExportTarget::Rva(r7) = ord.target else { panic!() };
    assert_eq!(l.resolve(dll, Symbol::Ordinal(7), "t", 0).unwrap(), base + r7 as u64);
    // Errors name the module and symbol.
    match l.resolve(dll, Symbol::Name("nope"), "t.exe", 0) {
        Err(LinkError::SymbolNotFound { module, symbol, importer }) => assert_eq!((module.as_str(), symbol.as_str(), importer.as_str()), ("sample.dll", "nope", "t.exe")),
        r => panic!("{r:?}"),
    }
    // Loading again finds the same module; a missing DLL is reported.
    assert_eq!(l.load_dll("SAMPLE.DLL", "x").unwrap(), dll);
    assert_eq!(l.load_dll("absent", "x.exe").unwrap_err(), LinkError::DllNotFound { name: "absent.dll".into(), importer: "x.exe".into() });
}

#[test]
fn mapped_files_support_imports_exports_and_forwarders() {
    let mut host = MemHost::default();
    host.files.insert("sample.dll".into(), SAMPLE.to_vec());
    host.map_files = true;
    let mut l = Linker::new(host, None);
    let dll = l.load_dll("sample.dll", "test.exe").unwrap();
    assert!(matches!(l.modules[dll].kind, osyx_pe_loader::link::Kind::Pe { file: osyx_pe_loader::link::PeData::Mapped(_), .. }));
    let base = l.modules[dll].base;
    let exports = l.modules[dll].pe().unwrap().exports().unwrap().unwrap();
    let osyx_pe::ExportTarget::Rva(add_rva) = exports.by_name("add_numbers").unwrap().unwrap().target else { panic!() };
    assert_eq!(l.resolve(dll, Symbol::Name("add_numbers"), "t", 0).unwrap(), base + add_rva as u64);
    assert_eq!(l.resolve(dll, Symbol::Name("forwarded_tick"), "t", 0).unwrap(), 0x7000_0040);
}

#[test]
fn forwarder_loops_are_bounded() {
    // Point `forwarded_tick` at itself: "kernel32.GetTickCount" becomes
    // "sample.forwarded_tick" (same length).
    let mut dll = SAMPLE.to_vec();
    let from = b"kernel32.GetTickCount";
    let at = dll.windows(from.len()).position(|w| w == from).unwrap();
    dll[at..at + from.len()].copy_from_slice(b"sample.forwarded_tick");
    let mut host = MemHost::default();
    host.files.insert("sample.dll".into(), dll);
    let mut l = Linker::new(host, None);
    let i = l.load_dll("sample.dll", "t").unwrap();
    assert!(matches!(l.resolve(i, Symbol::Name("forwarded_tick"), "t", 0), Err(LinkError::ForwarderTooDeep { .. })));
}

#[test]
fn api_set_imports_go_through_the_schema() {
    let ns = schema_bytes();
    let s = Schema::parse(&ns).unwrap();
    let mut l = Linker::new(MemHost::default(), Some(s));
    let k = l.load_dll("api-ms-win-core-sysinfo-l1-1-0.dll", "app.exe").unwrap();
    assert_eq!(l.modules[k].name, "kernel32.dll");
    assert_eq!(l.resolve(k, Symbol::Name("GetTickCount"), "app.exe", 0).unwrap(), 0x7000_0040);
    assert_eq!(l.load_dll("ext-ms-win-missing-l1-1-0", "app.exe").unwrap_err(), LinkError::NoApiSetHost { name: "ext-ms-win-missing-l1-1-0.dll".into(), importer: "app.exe".into() });
    assert!(l.host.opened.iter().all(|n| !n.starts_with("api-")), "contracts are never opened as files");
    assert_eq!(normalize("KERNEL32"), "kernel32.dll");
    assert_eq!(normalize("foo.drv"), "foo.drv");
}

#[test]
fn stripped_relocations_force_the_linked_base() {
    // hello.exe has no relocation directory but is dynamic-base: it may move.
    let pe = PeFile::parse(HELLO).unwrap();
    assert!(!pe.directory(osyx_pe::dir::BASERELOC).is_present() && image::relocatable(&pe));
    let mut l = Linker::new(MemHost::default(), None);
    let i = l.load_executable("hello.exe", HELLO.to_vec()).unwrap();
    assert_ne!(l.modules[i].base, pe.image_base());
    // Marked IMAGE_FILE_RELOCS_STRIPPED: it must stay where it was linked.
    let mut exe = HELLO.to_vec();
    let ch = pe.pe_offset() + 4 + 18;
    exe[ch] |= 1;
    let pe2 = PeFile::parse(&exe).unwrap();
    assert!(!image::relocatable(&pe2));
    let mut img = image::layout(&pe2).unwrap();
    assert_eq!(image::relocate(&pe2, &mut img, 0x1000_0000), Err(image::ImageError::NotRelocatable));
    let mut l = Linker::new(MemHost::default(), None);
    let i = l.load_executable("hello.exe", exe).unwrap();
    assert_eq!(l.modules[i].base, pe.image_base());
}

#[test]
fn executables_and_dlls_are_not_interchangeable() {
    let mut host = MemHost::default();
    host.files.insert("hello.dll".into(), HELLO.to_vec());
    let mut l = Linker::new(host, None);
    assert!(matches!(l.load_dll("hello", "t"), Err(LinkError::Image { .. })));
    assert!(matches!(l.load_executable("sample.dll", SAMPLE.to_vec()), Err(LinkError::Image { .. })));
}

#[test]
fn failed_dll_load_is_retried_instead_of_returning_an_unbound_image() {
    let mut dll = HELLO.to_vec();
    let pe = PeFile::parse(&dll).unwrap();
    let flags = pe.pe_offset() + 4 + 18;
    dll[flags + 1] |= 0x20; // IMAGE_FILE_DLL
    let good = dll.clone();
    let name = b"ExitProcess";
    let at = dll.windows(name.len()).position(|w| w == name).unwrap();
    dll[at..at + name.len()].copy_from_slice(b"LostProcess");
    let mut host = MemHost::default();
    host.files.insert("broken.dll".into(), dll);
    let mut l = Linker::new(host, None);
    let kernel = l.load_dll("kernel32", "test").unwrap();
    for _ in 0..2 {
        assert!(matches!(l.load_dll("broken", "test"), Err(LinkError::SymbolNotFound { .. })));
        assert!(l.find("broken").is_none());
        assert_eq!(l.find("kernel32"), Some(kernel));
        assert!(l.host.images.is_empty());
        assert!(l.cycles.is_empty());
    }
    assert_eq!(l.host.released.len(), 2);
    l.host.files.insert("broken.dll".into(), good);
    let loaded = l.load_dll("broken", "test").unwrap();
    assert_eq!(l.modules[loaded].state, osyx_pe_loader::link::State::Ready);
    assert_eq!(l.host.images.len(), 1);
}

#[test]
fn failed_root_releases_new_dependencies_and_preserves_loaded_modules() {
    let mut child = HELLO.to_vec();
    let flags = PeFile::parse(&child).unwrap().pe_offset() + 4 + 18;
    child[flags + 1] |= 0x20;
    let mut root = child.clone();
    let name = b"KERNEL32.dll";
    let at = root.windows(name.len()).position(|w| w == name).unwrap();
    root[at..at + name.len()].copy_from_slice(b"CHILD123.dll");
    let mut host = MemHost::default();
    host.files.insert("sample.dll".into(), SAMPLE.to_vec());
    host.files.insert("child123.dll".into(), child);
    host.files.insert("root.dll".into(), root);
    let mut l = Linker::new(host, None);
    let sample = l.load_dll("sample", "test").unwrap();
    let old_base = l.modules[sample].base;
    assert!(matches!(l.load_dll("root", "test"), Err(LinkError::SymbolNotFound { .. })));
    assert_eq!(l.modules.len(), 1);
    assert_eq!(l.find("sample"), Some(sample));
    assert_eq!(l.host.images.len(), 1);
    assert!(l.host.images.contains_key(&old_base));
    assert_eq!(l.host.released.len(), 2);
}

#[test]
fn layout_and_protections_match_the_sections() {
    let pe = PeFile::parse(SAMPLE).unwrap();
    image::check(&pe, true).unwrap();
    let img = image::layout(&pe).unwrap();
    assert_eq!(img.len(), pe.size_of_image() as usize);
    for s in pe.sections() {
        let n = s.file_size() as usize;
        assert_eq!(&img[s.virtual_address as usize..][..n], pe.slice_at_rva(s.virtual_address, n).unwrap(), "{}", s.name());
    }
    let prot = image::protections(&pe).unwrap();
    let total: u32 = prot.iter().map(|p| p.size).sum();
    assert_eq!(total, pe.size_of_image().div_ceil(image::PAGE) * image::PAGE);
}

/// A second "process" places a DLL where the first one published it and
/// takes the laid-out, relocated image from the host instead of building
/// it; the committed image is identical. A host that cannot use that base
/// still lays the image out itself.
#[test]
fn shared_images_skip_layout_at_the_published_base() {
    let shared = std::rc::Rc::new(std::cell::RefCell::new(BTreeMap::new()));
    let process = |next: u64| {
        let mut host = MemHost { next, shared: Some(shared.clone()), ..MemHost::default() };
        host.files.insert("sample.dll".into(), SAMPLE.to_vec());
        let mut l = Linker::new(host, None);
        l.load_dll("sample.dll", "test.exe").unwrap();
        l
    };
    let first = process(0);
    let a = first.modules.iter().position(|m| m.name == "sample.dll").unwrap();
    assert!(first.host.reused.is_empty());
    assert!(first.modules[a].relocations > 0);
    let base = first.modules[a].base;
    assert_eq!(shared.borrow()["sample.dll"].0, base, "published at its base before binding");

    let second = process(0x100_0000);
    let b = second.modules.iter().position(|m| m.name == "sample.dll").unwrap();
    assert_eq!(second.host.reused, ["sample.dll"]);
    assert_eq!(second.modules[b].base, base, "placed at the shared base");
    assert_eq!(second.modules[b].relocations, 0, "no layout or relocation");
    assert_eq!(second.modules[b].imports_bound, first.modules[a].imports_bound);
    assert_eq!(second.host.images[&base], first.host.images[&base], "same bound image and protections");

    // A published image that does not fit the reservation: built locally.
    let mut host = MemHost { next: 0x200_0000, ..MemHost::default() };
    host.files.insert("sample.dll".into(), SAMPLE.to_vec());
    host.shared = Some(std::rc::Rc::new(std::cell::RefCell::new(BTreeMap::from([(
        "sample.dll".to_string(),
        (0x9_0000_0000u64, vec![0u8; 16]),
    )]))));
    let mut l = Linker::new(host, None);
    let c = l.load_dll("sample.dll", "test.exe").unwrap();
    assert!(l.host.reused.is_empty(), "size mismatch is never reused");
    assert!(l.modules[c].relocations > 0);
}
