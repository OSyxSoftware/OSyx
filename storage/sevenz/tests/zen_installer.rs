//! The official Zen Browser installer (fetched by scripts/fetch-zen.ps1 into
//! build/zen, not part of the repository): every file extracted here must
//! equal what Windows tar (libarchive) extracted into build/zen/<version>.
//! Skipped when the installer is not present.

use osyx_7z::{extract, find, list, Sink, Source};
use std::path::PathBuf;

struct FileSource(std::fs::File, u64);

impl Source for FileSource {
    fn size(&self) -> u64 {
        self.1
    }
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> osyx_7z::Result<usize> {
        use std::io::{Read, Seek, SeekFrom};
        self.0.seek(SeekFrom::Start(offset)).map_err(|_| osyx_7z::Error::Io)?;
        self.0.read(buf).map_err(|_| osyx_7z::Error::Io)
    }
}

struct Compare {
    reference: PathBuf,
    current: Option<(String, Vec<u8>)>,
    files: usize,
    dirs: usize,
}

impl Sink for Compare {
    fn dir(&mut self, path: &str) -> osyx_7z::Result<()> {
        assert!(self.reference.join(path).is_dir(), "{path} is not a directory in the reference");
        self.dirs += 1;
        Ok(())
    }
    fn file_start(&mut self, path: &str, size: u64) -> osyx_7z::Result<()> {
        self.current = Some((path.to_string(), Vec::with_capacity(size as usize)));
        Ok(())
    }
    fn file_data(&mut self, data: &[u8]) -> osyx_7z::Result<()> {
        self.current.as_mut().unwrap().1.extend_from_slice(data);
        Ok(())
    }
    fn file_end(&mut self) -> osyx_7z::Result<()> {
        let (path, data) = self.current.take().unwrap();
        let want = std::fs::read(self.reference.join(&path)).unwrap_or_else(|e| panic!("{path}: {e}"));
        assert!(data == want, "{path} differs from the reference ({} vs {} bytes)", data.len(), want.len());
        self.files += 1;
        Ok(())
    }
}

#[test]
fn official_zen_installer_matches_the_reference_extraction() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/zen");
    let installer = root.join("zen-1.23.4b-x64.exe");
    let reference = root.join("1.23.4b");
    if !installer.exists() || !reference.join("core/zen.exe").exists() {
        eprintln!("SKIPPED: run scripts/fetch-zen.ps1 first");
        return;
    }
    let open = || {
        let f = std::fs::File::open(&installer).unwrap();
        let len = f.metadata().unwrap().len();
        FileSource(f, len)
    };
    let mut src = open();
    let base = find(&mut src, 4 << 20).unwrap();
    assert_eq!(base, 131_665);
    let entries = list(open(), base).unwrap();
    assert_eq!(entries.len(), 80);
    assert!(entries.iter().any(|e| e.path == "core/zen.exe" && !e.dir));
    let mut sink = Compare { reference, current: None, files: 0, dirs: 0 };
    let total = extract(open(), base, &mut sink).unwrap();
    assert_eq!(total, 450_144_499);
    assert_eq!(sink.files + sink.dirs, 80);
}

#[test]
fn rejects_non_archives() {
    let mut v = vec![0u8; 4096];
    assert!(find(&mut v, 4096).is_err());
}
