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
pub fn save(dir: &str, name: &str, source: Source) -> Result<(), String> {
    let (child, mut to_child, mut from_child) =
        Command::new(PROGRAM).arg(FLAG).arg(dir).arg(name).spawn_piped().map_err(|e| format!("{PROGRAM}: {e}"))?;
    let sent = match source {
        Source::Bytes(data) => to_child.write_all(data),
        Source::File(path) => File::open(path).and_then(|mut f| {
            let mut buf = vec![0u8; 64 << 10];
            loop {
                match f.read(&mut buf)? {
                    0 => return Ok(()),
                    n => to_child.write_all(&buf[..n])?,
                }
            }
        }),
    };
    // End of data for the child.
    drop(to_child);
    let mut said = String::new();
    let _ = from_child.read_to_string(&mut said);
    let status = child.wait().map_err(|e| format!("{e}"))?;
    if status.success() && sent.is_ok() {
        return Ok(());
    }
    // The child knows why better than a closed pipe does. A child that
    // had everything is told nothing about a source that broke off: what
    // it saved is not the whole.
    if status.success() {
        let _ = fs::remove_file(format!("{dir}/{name}"));
    }
    let said = said.trim();
    match (said.is_empty(), sent) {
        (false, _) => Err(String::from(said)),
        (true, Err(e)) => Err(format!("{e}")),
        (true, Ok(())) => Err(format!("{dir}/{name} was not saved")),
    }
}

/// The child: standard input becomes the new file `dir/name`, written as
/// the desktop user. Returns the exit code; errors go to standard output,
/// which the parent reads.
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
    if let Err(e) = fs::create_dir_all(dir) {
        println!("{dir}: {e}");
        return 1;
    }
    let path = format!("{dir}/{name}");
    let mut file = match File::options().write(true).create_new(true).mode(0o644).open(&path) {
        Ok(f) => f,
        Err(e) => {
            println!("{path}: {e}");
            return 1;
        }
    };
    let mut input = io::stdin();
    let mut buf = vec![0u8; 64 << 10];
    let copied = loop {
        match input.read(&mut buf) {
            Ok(0) => break file.sync_all(),
            Ok(n) => {
                if let Err(e) = file.write_all(&buf[..n]) {
                    break Err(e);
                }
            }
            Err(e) => break Err(e),
        }
    };
    drop(file);
    match copied {
        Ok(()) => 0,
        Err(e) => {
            let _ = fs::remove_file(&path);
            println!("{path}: {e}");
            1
        }
    }
}
