//! Unix-style permission checks on `FileStat` mode, owner and group.
//!
//! The super-user (uid 0) passes every read/write check and every search
//! check; execute permission on regular files still needs at least one
//! execute bit, as on Unix.

use osyx_abi::{FileStat, KIND_DIR, MODE_STICKY, ROOT_UID};

/// Credentials of the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cred {
    pub uid: u32,
    pub gid: u32,
}

impl Cred {
    pub const ROOT: Cred = Cred { uid: ROOT_UID, gid: 0 };

    pub fn is_root(&self) -> bool {
        self.uid == ROOT_UID
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub read: bool,
    pub write: bool,
    /// Execute a file / search a directory.
    pub exec: bool,
}

impl Access {
    pub const READ: Access = Access { read: true, write: false, exec: false };
    pub const WRITE: Access = Access { read: false, write: true, exec: false };
    pub const EXEC: Access = Access { read: false, write: false, exec: true };
    /// What changing a directory's entries needs.
    pub const WRITE_EXEC: Access = Access { read: false, write: true, exec: true };
}

/// May `cred` access a node with metadata `st` as `want`?
pub fn may(st: &FileStat, cred: &Cred, want: Access) -> bool {
    let mode = st.mode;
    if cred.is_root() {
        // Root may search any directory and execute a file with any x bit.
        return !want.exec || st.kind == KIND_DIR || mode & 0o111 != 0;
    }
    let bits = if cred.uid == st.uid {
        (mode >> 6) & 7
    } else if cred.gid == st.gid {
        (mode >> 3) & 7
    } else {
        mode & 7
    };
    (!want.read || bits & 4 != 0) && (!want.write || bits & 2 != 0) && (!want.exec || bits & 1 != 0)
}

/// May `cred` remove or rename `entry` out of directory `dir` (write and
/// search permission on `dir` are checked separately)? Sticky directories
/// only let the entry's or the directory's owner do it.
pub fn may_delete(dir: &FileStat, entry: &FileStat, cred: &Cred) -> bool {
    dir.mode & MODE_STICKY == 0 || cred.is_root() || cred.uid == dir.uid || cred.uid == entry.uid
}

/// May `cred` change the mode of a node owned by `st.uid`?
pub fn may_chmod(st: &FileStat, cred: &Cred) -> bool {
    cred.is_root() || cred.uid == st.uid
}

#[cfg(test)]
mod tests {
    use super::*;
    use osyx_abi::KIND_FILE;

    fn st(mode: u32, uid: u32, gid: u32, kind: u32) -> FileStat {
        FileStat { mode, uid, gid, kind, ..Default::default() }
    }

    #[test]
    fn owner_group_other_bits() {
        let f = st(0o640, 10, 20, KIND_FILE);
        let owner = Cred { uid: 10, gid: 99 };
        let group = Cred { uid: 11, gid: 20 };
        let other = Cred { uid: 12, gid: 21 };
        assert!(may(&f, &owner, Access::READ) && may(&f, &owner, Access::WRITE));
        assert!(may(&f, &group, Access::READ) && !may(&f, &group, Access::WRITE));
        assert!(!may(&f, &other, Access::READ));
        // The owner class decides even if group bits would allow more.
        let g = st(0o070, 10, 20, KIND_FILE);
        assert!(!may(&g, &Cred { uid: 10, gid: 20 }, Access::READ));
    }

    #[test]
    fn root_bypasses_except_exec_on_plain_files() {
        let f = st(0o000, 10, 20, KIND_FILE);
        assert!(may(&f, &Cred::ROOT, Access::READ) && may(&f, &Cred::ROOT, Access::WRITE));
        assert!(!may(&f, &Cred::ROOT, Access::EXEC));
        assert!(may(&st(0o100, 10, 20, KIND_FILE), &Cred::ROOT, Access::EXEC));
        assert!(may(&st(0o000, 10, 20, KIND_DIR), &Cred::ROOT, Access::EXEC));
    }

    #[test]
    fn sticky_directories() {
        let tmp = st(0o1777, 0, 0, KIND_DIR);
        let mine = st(0o644, 1000, 1000, KIND_FILE);
        let theirs = st(0o644, 1001, 1001, KIND_FILE);
        let me = Cred { uid: 1000, gid: 1000 };
        assert!(may_delete(&tmp, &mine, &me));
        assert!(!may_delete(&tmp, &theirs, &me));
        assert!(may_delete(&tmp, &theirs, &Cred::ROOT));
        assert!(may_delete(&st(0o777, 0, 0, KIND_DIR), &theirs, &me));
        assert!(may_chmod(&mine, &me) && !may_chmod(&theirs, &me));
    }
}
