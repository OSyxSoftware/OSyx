//! ktmw32: transactions. A transaction is active until it is committed or
//! rolled back, with the errors of Windows for the other transitions
//! (commit after rollback ERROR_TRANSACTION_ALREADY_ABORTED, any call after
//! a commit ERROR_TRANSACTION_ALREADY_COMMITTED, rollback after rollback
//! succeeds). The one resource manager is the registry: keys created
//! through RegCreateKeyTransactedW are removed again by a rollback, and
//! kept by a commit. The file system is the other: DeleteFileTransactedW
//! moves the file aside (a commit removes it, a rollback puts it back) and
//! MoveFileTransactedW renames, keeping a replaced destination aside the
//! same way. Changes are visible to other readers at once; there is no
//! isolation between a transaction and the rest of the system.
#![allow(non_snake_case)]
use crate::{
    error::*,
    kernel32::fail,
    objects::{self, Object},
    security,
};
use alloc::{string::String, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};
use os_std::sync::Mutex;

const ERROR_TRANSACTION_ALREADY_ABORTED: u32 = 6704;
const ERROR_TRANSACTION_ALREADY_COMMITTED: u32 = 6705;
const TRANSACTION_DO_NOT_PROMOTE: u32 = 1;

const ACTIVE: u32 = 0;
const COMMITTED: u32 = 1;
const ABORTED: u32 = 2;

/// What a rollback undoes: a registry key this transaction created (hive
/// index and lower-case path).
pub enum Undo {
    RegistryKey(usize, alloc::string::String),
    /// A file renamed from .0 to .1: a rollback renames it back.
    FileRenamed(String, String),
    /// A file moved aside from .1 to .0: a commit deletes it, a rollback
    /// moves it back.
    FileHeld(String, String),
}

pub struct TransactionObj {
    id: [u8; 16],
    state: AtomicU32,
    undo: Mutex<Vec<Undo>>,
}

impl TransactionObj {
    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::SeqCst) == ACTIVE
    }

    pub fn record(&self, op: Undo) {
        self.undo.lock().unwrap().push(op);
    }
}

/// The active transaction behind a handle: ERROR_INVALID_HANDLE for anything
/// else, and the error of its final state when it has ended.
pub fn active(h: u64) -> Result<Arc<TransactionObj>, u32> {
    let Some(Object::Transaction(t)) = objects::get(h) else { return Err(ERROR_INVALID_HANDLE) };
    match t.state.load(Ordering::SeqCst) {
        ACTIVE => Ok(t),
        COMMITTED => Err(ERROR_TRANSACTION_ALREADY_COMMITTED),
        _ => Err(ERROR_TRANSACTION_ALREADY_ABORTED),
    }
}

/// CreateTransaction(security, uow, options, isolation level, isolation
/// flags, timeout, description): the handle, or INVALID_HANDLE_VALUE. The
/// options may only be TRANSACTION_DO_NOT_PROMOTE; the isolation arguments
/// are accepted and unused (see the module comment).
extern "win64" fn CreateTransaction(attributes: u64, uow: *const u8, options: u32, _isolation: u32, _flags: u32, _timeout: u32, _description: *const u16) -> u64 {
    if let Err(e) = security::validate_attributes(attributes) {
        return fail(e, u64::MAX);
    }
    if options & !TRANSACTION_DO_NOT_PROMOTE != 0 {
        return fail(ERROR_INVALID_PARAMETER, u64::MAX);
    }
    let mut id = [0u8; 16];
    if !uow.is_null() {
        // SAFETY: the caller's GUID.
        unsafe { core::ptr::copy_nonoverlapping(uow, id.as_mut_ptr(), 16) };
    } else if !crate::ucrt::entropy(&mut id) {
        return fail(ERROR_GEN_FAILURE, u64::MAX);
    }
    objects::insert(Object::Transaction(Arc::new(TransactionObj { id, state: AtomicU32::new(ACTIVE), undo: Mutex::new(Vec::new()) })))
}

extern "win64" fn CommitTransaction(h: u64) -> i32 {
    match active(h) {
        Ok(t) => {
            t.state.store(COMMITTED, Ordering::SeqCst);
            for op in core::mem::take(&mut *t.undo.lock().unwrap()) {
                if let Undo::FileHeld(held, _) = op {
                    crate::kernel32::delete_file(Some(held));
                }
            }
            1
        }
        Err(e) => fail(e, 0),
    }
}

/// RollbackTransaction: undoes what the transaction did, newest first. A
/// transaction that was already rolled back succeeds again.
extern "win64" fn RollbackTransaction(h: u64) -> i32 {
    let t = match active(h) {
        Ok(t) => t,
        Err(ERROR_TRANSACTION_ALREADY_ABORTED) => return 1,
        Err(e) => return fail(e, 0),
    };
    t.state.store(ABORTED, Ordering::SeqCst);
    let ops: Vec<Undo> = core::mem::take(&mut *t.undo.lock().unwrap());
    for op in ops.into_iter().rev() {
        match op {
            Undo::RegistryKey(hive, path) => crate::registry::undo_create(hive, &path),
            Undo::FileRenamed(from, to) => {
                crate::kernel32::move_file(Some(to), Some(from), MOVEFILE_REPLACE_EXISTING);
            }
            Undo::FileHeld(held, original) => {
                crate::kernel32::move_file(Some(held), Some(original), MOVEFILE_REPLACE_EXISTING);
            }
        }
    }
    1
}

extern "win64" fn GetTransactionId(h: u64, guid: *mut u8) -> i32 {
    let Some(Object::Transaction(t)) = objects::get(h) else { return fail(ERROR_INVALID_HANDLE, 0) };
    if guid.is_null() {
        return fail(ERROR_NOACCESS, 0);
    }
    // SAFETY: the caller's GUID.
    unsafe { core::ptr::copy_nonoverlapping(t.id.as_ptr(), guid, 16) };
    1
}

const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
const MOVEFILE_COPY_ALLOWED: u32 = 0x2;
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

/// A name next to `win` to keep a file aside under while a transaction
/// is open.
fn aside(win: &str) -> String {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let (dir, file) = win.rsplit_once(['\\', '/']).unwrap_or((".", win));
    alloc::format!("{dir}\\.~tx{}-{n}-{file}", crate::kernel32::pid())
}

fn win_name(p: *const u16) -> Option<String> {
    // SAFETY: NULL or a NUL-terminated name.
    unsafe { crate::path::wide_to_string(p) }
}

/// DeleteFileTransactedW: the file is moved aside; the transaction's commit
/// deletes it and its rollback restores it.
extern "win64" fn DeleteFileTransactedW(name: *const u16, tx: u64) -> i32 {
    let t = match active(tx) {
        Ok(t) => t,
        Err(e) => return fail(e, 0),
    };
    let Some(n) = win_name(name) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    match crate::kernel32::locate(&n) {
        Ok((_, true)) => {}
        Ok((_, false)) => return fail(ERROR_FILE_NOT_FOUND, 0),
        Err(e) => return fail(e, 0),
    }
    let held = aside(&n);
    if crate::kernel32::move_file(Some(n.clone()), Some(held.clone()), 0) == 0 {
        return 0;
    }
    t.record(Undo::FileHeld(held, n));
    1
}

/// MoveFileTransactedW: a rename the transaction's rollback undoes; a
/// destination it replaces (MOVEFILE_REPLACE_EXISTING) is kept aside until
/// the commit. The progress routine is called once with the whole size,
/// as for CopyFileEx (the move is one step).
#[allow(clippy::too_many_arguments)]
extern "win64" fn MoveFileTransactedW(from: *const u16, to: *const u16, _progress: u64, _data: u64, flags: u32, tx: u64) -> i32 {
    if flags & !(MOVEFILE_REPLACE_EXISTING | MOVEFILE_COPY_ALLOWED | MOVEFILE_WRITE_THROUGH) != 0 {
        return fail(ERROR_INVALID_PARAMETER, 0);
    }
    let t = match active(tx) {
        Ok(t) => t,
        Err(e) => return fail(e, 0),
    };
    let (Some(f), Some(dst)) = (win_name(from), win_name(to)) else { return fail(ERROR_INVALID_PARAMETER, 0) };
    let mut held = None;
    if matches!(crate::kernel32::locate(&dst), Ok((_, true))) {
        if flags & MOVEFILE_REPLACE_EXISTING == 0 {
            return fail(ERROR_ALREADY_EXISTS, 0);
        }
        let a = aside(&dst);
        if crate::kernel32::move_file(Some(dst.clone()), Some(a.clone()), 0) == 0 {
            return 0;
        }
        held = Some(a);
    }
    if crate::kernel32::move_file(Some(f.clone()), Some(dst.clone()), flags & !MOVEFILE_REPLACE_EXISTING) == 0 {
        let e = crate::teb::last_error();
        if let Some(a) = held {
            crate::kernel32::move_file(Some(a), Some(dst), 0);
        }
        return fail(e, 0);
    }
    if let Some(a) = held {
        t.record(Undo::FileHeld(a, dst.clone()));
    }
    t.record(Undo::FileRenamed(f, dst));
    1
}

pub fn exports() -> Vec<(&'static str, u64)> {
    export_table![CreateTransaction, CommitTransaction, RollbackTransaction, GetTransactionId]
}

/// The kernel32 side of transactions.
pub fn kernel32_exports() -> Vec<(&'static str, u64)> {
    export_table![DeleteFileTransactedW, MoveFileTransactedW]
}
