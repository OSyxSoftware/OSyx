//! Boot-time storage setup: find disks and partitions, mount the file
//! system tree.
//!
//! Layout:
//!
//! | Mount point | File system | Source |
//! |---|---|---|
//! | `/` | ext4 | GPT partition of type "Linux root (x86-64)", or `root=<dev>` |
//! | `/boot` | FAT32 | the EFI system partition |
//! | `/dev` | devfs | the device registry |
//! | `/tmp` | tmpfs | RAM, mode 1777 |
//! | `/run` | tmpfs | RAM, mode 0755 |
//!
//! Without a usable root partition a small tmpfs becomes `/` so the rest
//! of the tree still works (and the log says so); the boot modules from
//! `\EFI\OSYX\BIN` become its `/bin`. Other FAT32/ext4 volumes are mounted
//! below `/mnt/<device>`.

use crate::block::{self, BlockDev};
use crate::vfs::{self, devfs::DevFs, tmpfs::TmpfsVolume, Filesystem, Lookup};
use crate::{kprintln, live, modules, BootData};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use osyx_abi::{MOUNT_READ_ONLY, OPEN_CREATE, OPEN_TRUNCATE, OPEN_WRITE};

const TMP_LIMIT: u64 = 64 << 20;
const RUN_LIMIT: u64 = 8 << 20;
// The live root unpacks into this tmpfs; the installed fallback (boot
// modules only) uses a few MiB of it. tmpfs charges only what is stored.
const FALLBACK_ROOT_LIMIT: u64 = 256 << 20;

/// What ended up mounted, for the boot summary and tests.
pub struct Summary {
    pub root: String,
    pub mounts: usize,
}

fn probe_fs(disk: &Arc<dyn BlockDev>) -> Option<&'static str> {
    let mut s = [0u8; block::SECTOR_SIZE * 4];
    block::read(&**disk, 0, &mut s).ok()?;
    // ext2/3/4: superblock at byte 1024, magic 0xEF53 at offset 56.
    if s[1024 + 56] == 0x53 && s[1024 + 57] == 0xEF {
        return Some("ext4");
    }
    // FAT32: boot signature and "FAT32   " file system type string.
    if s[510] == 0x55 && s[511] == 0xAA && &s[82..90] == b"FAT32   " {
        return Some("fat32");
    }
    None
}

fn try_mount(fstype: &str, disk: Arc<dyn BlockDev>, at: &str, read_only: bool) -> bool {
    let name = String::from(disk.name());
    match vfs::mount_disk(fstype, disk, read_only) {
        Ok(fs) => match vfs::mount_fs(at, &alloc::format!("/dev/disks/{name}"), if read_only { MOUNT_READ_ONLY } else { 0 }, fs) {
            Ok(_) => true,
            Err(e) => {
                kprintln!("storage: mounting {name} at {at} failed: {e:?}");
                false
            }
        },
        Err(e) => {
            kprintln!("storage: {name}: {fstype} not mountable: {e:?}");
            false
        }
    }
}

/// Creates the standard mount points on a fresh fallback root and installs
/// the boot modules as programs in `/bin`.
fn populate_fallback_root() {
    let cx = Lookup::kernel();
    for d in ["boot", "dev", "tmp", "run", "mnt", "bin"] {
        if let Err(e) = vfs::mkdir(&cx, &alloc::format!("/{d}"), 0o755) {
            kprintln!("storage: cannot create /{d} on the fallback root: {e:?}");
        }
    }
    for (name, data) in modules::all() {
        let path = alloc::format!("/bin/{name}");
        let flags = OPEN_WRITE | OPEN_CREATE | OPEN_TRUNCATE;
        let r = vfs::open(&cx, &path, flags, 0o755).and_then(|mut f| {
            let mut done = 0;
            while done < data.len() {
                match f.write(&data[done..])? {
                    0 => return Err(osyx_abi::Errno::NoSpace),
                    n => done += n,
                }
            }
            Ok(())
        });
        match r {
            Ok(()) => kprintln!("storage: boot module {name} installed as {path}"),
            Err(e) => kprintln!("storage: cannot install {path}: {e:?}"),
        }
    }
}

pub fn mount_all(boot: &BootData) -> Summary {
    let disks = block::devices();
    let esp = disks.iter().find(|d| d.partition().is_some_and(|p| p.type_guid == osyx_part::TYPE_ESP)).cloned();
    let wanted_root = boot.option_value("root");
    // A live boot keeps every disk untouched: the whole system comes from
    // the compressed archive in \EFI\OSYX\BIN.
    let root_dev = if boot.has_option("live") {
        kprintln!("storage: live boot; disk roots ignored");
        None
    } else {
        match wanted_root {
            Some(name) => disks.iter().find(|d| d.name() == name).cloned(),
            None => disks.iter().find(|d| d.partition().is_some_and(|p| p.type_guid == osyx_part::TYPE_ROOT_X86_64)).cloned(),
        }
    };
    let root_ro = boot.has_option("ro");
    let mut used: Vec<String> = Vec::new();
    let mut root = String::from("tmpfs (fallback)");
    let mut have_root = false;
    if let Some(dev) = root_dev {
        match probe_fs(&dev) {
            Some(fstype) => {
                let name = String::from(dev.name());
                if try_mount(fstype, dev, "/", root_ro) {
                    root = alloc::format!("{fstype} on {name}");
                    used.push(name);
                    have_root = true;
                }
            }
            None => kprintln!("storage: root device {} holds no known file system", dev.name()),
        }
    } else if let Some(name) = wanted_root {
        kprintln!("storage: root={name}: no such block device");
    }
    if !have_root {
        kprintln!("storage: no root file system; using a {} MiB tmpfs as /", FALLBACK_ROOT_LIMIT >> 20);
        let fs: Arc<dyn Filesystem> = Arc::new(TmpfsVolume::new(FALLBACK_ROOT_LIMIT, 0o755));
        vfs::mount_fs("/", "tmpfs", 0, fs).expect("mounting a fresh tmpfs at / cannot fail");
        if live::present() {
            match live::unpack() {
                Ok(n) => {
                    kprintln!("storage: live root unpacked ({} entries)", n);
                    root = alloc::format!("tmpfs (live root, {n} entries)");
                }
                Err(e) => {
                    kprintln!("storage: live root unusable: {e}");
                    populate_fallback_root();
                }
            }
        } else {
            populate_fallback_root();
        }
    }
    let mount_virtual = |at: &str, fs: Arc<dyn Filesystem>, src: &str| {
        if let Err(e) = vfs::mount_fs(at, src, 0, fs) {
            kprintln!("storage: cannot mount {src} at {at}: {e:?}");
        }
    };
    mount_virtual("/dev", Arc::new(DevFs::new()), "devfs");
    mount_virtual("/tmp", Arc::new(TmpfsVolume::new(TMP_LIMIT, 0o1777)), "tmpfs");
    mount_virtual("/run", Arc::new(TmpfsVolume::new(RUN_LIMIT, 0o755)), "tmpfs");
    if let Some(dev) = esp {
        let name = String::from(dev.name());
        if probe_fs(&dev) == Some("fat32") && try_mount("fat32", dev, "/boot", false) {
            used.push(name);
        } else {
            kprintln!("storage: EFI system partition {name} is not a FAT32 volume");
        }
    }
    // Remaining recognisable volumes go to /mnt/<device>.
    for d in disks {
        let name = String::from(d.name());
        let covered = used.iter().any(|u| *u == name || u.starts_with(&name) || name.starts_with(u.as_str()));
        let is_partitioned_disk = d.partition().is_none() && block::devices().iter().any(|p| p.partition().is_some() && p.name().starts_with(&name));
        if covered || is_partitioned_disk {
            continue;
        }
        if let Some(fstype) = probe_fs(&d) {
            let at = alloc::format!("/mnt/{name}");
            if vfs::mkdir(&Lookup::kernel(), &at, 0o755).is_ok() || vfs::stat(&Lookup::kernel(), &at).is_ok() {
                let ro = d.read_only();
                if try_mount(fstype, d, &at, ro) {
                    used.push(name);
                }
            }
        }
    }
    Summary { root, mounts: vfs::mounts().len() }
}
