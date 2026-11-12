//! Files that PRISM saves for the desktop user (screenshots, recordings).
//!
//! PRISM is root, and the user's home is the user's: a folder there can be
//! replaced by a symbolic link to any place on the disk. Root therefore
//! writes nothing below it. A child process gives up root first and then
//! makes the folder and the file, with the data piped to it: a planted link
//! leads only where the user may write anyway, and what is saved belongs to
//! the user, who can rename and delete it.

use os_std::fs::{self, File};
use os_std::io;
use os_std::prelude::*;
use os_std::process::Command;

/// The first argument that makes the program the saving child.
pub const FLAG: &str = "--save-as-user";
const PROGRAM: &str = "/system/bin/prism";
/// The desktop user and its group, as in `shell_common.rs`.
const USER_UID: u32 = 1000;
const ADMIN_GID: u32 = 27;

/// What to save.
pub enum Source<'a> {
    Bytes(&'a [u8]),
    /// A file of root's, read here and piped to the child.
    File(&'a str),
}

/// Saves `source` as the new file `dir/name` with the desktop user's
/// rights. A file of that name is never written over.
///
/// The child is told the length first and keeps the file only when it got
/// exactly that much, so nothing here has to tidy up after it: root never
/// touches the user's folder, not even to remove a half-written file.
pub fn save(dir: &str, name: &str, source: Source) -> Result<(), String> {
    let mut file = match &source {
        Source::Bytes(_) => None,
        Source::File(path) => Some(File::open(path).map_err(|e| format!("{path}: {e}"))?),
    };
    let length = match (&source, &file) {
        (Source::Bytes(data), _) => data.len() as u64,
        (_, Some(f)) => f.metadata().map_err(|e| format!("{e}"))?.len(),
        (Source::File(_), None) => 0,
    };
    let (child, mut to_child, mut from_child) =
        Command::new(PROGRAM).arg(FLAG).arg(dir).arg(name).spawn_piped().map_err(|e| format!("{PROGRAM}: {e}"))?;
    let sent = to_child.write_all(&length.to_le_bytes()).and_then(|()| match (&source, &mut file) {
        (Source::Bytes(data), _) => to_child.write_all(data),
        (_, Some(f)) => {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                match f.read(&mut buf)? {
                    0 => return Ok(()),
                    n => to_child.write_all(&buf[..n])?,
                }
            }
        }
        (Source::File(_), None) => Ok(()),
    });
    // End of data for the child.
    drop(to_child);
    let mut said = String::new();
    let _ = from_child.read_to_string(&mut said);
    let status = child.wait().map_err(|e| format!("{e}"))?;
    if status.success() {
        return Ok(());
    }
    // The child knows why better than a closed pipe does.
    let said = said.trim();
    match (said.is_empty(), sent) {
        (false, _) => Err(String::from(said)),
        (true, Err(e)) => Err(format!("{e}")),
        (true, Ok(())) => Err(format!("{dir}/{name} was not saved")),
    }
}

/// Makes room for the user's own `dir` where a folder of that name is
/// someone else's (an earlier version made the capture folders as root):
/// that folder gets another name beside it, which the owner of the home
/// folder may give it, and stays readable there.
fn set_aside(dir: &str, uid: u32) -> bool {
    let foreign = fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir() && m.uid() != uid);
    if !foreign {
        return false;
    }
    for n in 1..100 {
        let aside = if n == 1 { format!("{dir} (old)") } else { format!("{dir} (old {n})") };
        if fs::symlink_metadata(&aside).is_err() {
            return fs::rename(dir, &aside).is_ok();
        }
    }
    false
}

/// The child: standard input (a length, then that many bytes) becomes the
/// new file `dir/name`, written as the desktop user. Returns the exit
/// code; errors go to standard output, which the parent reads.
pub fn child(dir: &str, name: &str) -> i32 {
    if os_std::os::getuid().0 == 0 {
        if let Err(e) = os_std::os::setuid(USER_UID, ADMIN_GID) {
            // Never as root.
            println!("cannot become the desktop user: {e}");
            return 1;
        }
    }
    if name.is_empty() || name.contains('/') {
        println!("{name}: not a file name");
        return 1;
    }
    let path = format!("{dir}/{name}");
    let open = || {
        fs::create_dir_all(dir)?;
        File::options().write(true).create_new(true).mode(0o644).open(&path)
    };
    let mut file = match open() {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::PermissionDenied && set_aside(dir, os_std::os::getuid().0) => match open() {
            Ok(f) => f,
            Err(e) => {
                println!("{path}: {e}");
                return 1;
            }
        },
        Err(e) => {
            println!("{path}: {e}");
            return 1;
        }
    };
    let mut input = io::stdin();
    let mut length = [0u8; 8];
    let mut copied = input.read_exact(&mut length);
    let mut left = u64::from_le_bytes(length);
    let mut buf = vec![0u8; 64 << 10];
    while copied.is_ok() && left > 0 {
        let want = buf.len().min(left as usize);
        copied = match input.read(&mut buf[..want]) {
            Ok(0) => Err(Error::new(ErrorKind::UnexpectedEof, "the data ended early")),
            Ok(n) => {
                left -= n as u64;
                file.write_all(&buf[..n])
            }
            Err(e) => Err(e),
        };
    }
    let copied = copied.and_then(|()| file.sync_all());
    drop(file);
    match copied {
        Ok(()) => 0,
        Err(e) => {
            // As the user: this removal can reach nothing the user could
            // not remove anyway.
            let _ = fs::remove_file(&path);
            println!("{path}: {e}");
            1
        }
    }
}
