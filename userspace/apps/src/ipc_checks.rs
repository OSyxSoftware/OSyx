//! Checks of the ABI v7 kernel objects run by `init`: pipes, standard
//! stream redirection, channels and services, shared memory passed between
//! processes, waiting on handles, kill, and the process and mount listings.

use super::{worker, Tests};
use os_std::error::ErrorKind;
use os_std::io::{Read, Write};
use os_std::ipc::{self, item, wait, Channel, Listener, SharedMemory, WAIT_HANGUP, WAIT_READABLE};
use os_std::os;
use os_std::prelude::*;
use os_std::system;
use os_std::time::{Duration, Instant};

pub fn run(t: &mut Tests) {
    t.check("ipc: pipe round trip and end of data", || {
        let Ok((mut r, mut w)) = ipc::pipe() else { return false };
        let wrote = w.write_all(b"through the pipe").is_ok();
        let mut buf = [0u8; 32];
        let n = r.read(&mut buf).unwrap_or(0);
        drop(w);
        wrote && &buf[..n] == b"through the pipe" && r.read(&mut buf).ok() == Some(0)
    });
    t.check("ipc: writing to a pipe without readers is BrokenPipe", || {
        let Ok((r, mut w)) = ipc::pipe() else { return false };
        drop(r);
        w.write(b"x").map_err(|e| e.kind()) == Err(ErrorKind::BrokenPipe)
    });
    t.check("ipc: a 200 KiB stream through a 64 KiB pipe to a child and back", || {
        // The child reads all of it (the pipe fills and blocks the writer
        // until the child drains it) and echoes it upper-cased.
        let Ok((child, mut stdin, mut stdout)) = worker(&["echo-stdin"]).spawn_piped() else { return false };
        let data: Vec<u8> = (0..200 * 1024).map(|i| b"abcdefghij\n"[i % 11]).collect();
        let writer = os_std::thread::spawn(move || {
            let ok = stdin.write_all(&data).is_ok();
            drop(stdin);
            ok
        });
        let mut out = Vec::new();
        let read_ok = stdout.read_to_end(&mut out).is_ok();
        let wrote = writer.join().unwrap_or(false);
        let status = child.wait().map(|s| s.code());
        let text = String::from_utf8_lossy(&out);
        read_ok
            && wrote
            && status.ok() == Some(Some(0))
            && text.starts_with("ABCDEFGHIJ\nABCDEFGHIJ\n")
            && text.ends_with(&format!("stdin bytes: {}\n", 200 * 1024))
    });
    t.check("ipc: services, connections, messages and peer identity", || {
        let Ok(server) = Listener::listen("init-check") else { return false };
        let refused = Listener::listen("init-check").err().map(|e| e.kind()) == Some(ErrorKind::AlreadyExists);
        let missing = Channel::connect("no-such-service").err().map(|e| e.kind()) == Some(ErrorKind::NotFound);
        let Ok(client) = Channel::connect("init-check") else { return false };
        let Ok(Some(conn)) = server.accept() else { return false };
        let none_pending = matches!(server.accept(), Ok(None));
        let mut buf = [0u8; 16];
        let empty = matches!(conn.recv(&mut buf), Ok(None));
        let sent = client.send(b"ping", &[]).is_ok() && conn.send(b"pong", &[]).is_ok();
        let a = conn.recv(&mut buf).ok().flatten().map(|(n, _)| buf[..n].to_vec());
        let b = client.recv(&mut buf).ok().flatten().map(|(n, _)| buf[..n].to_vec());
        let me = os_std::process::id().unwrap_or(0);
        refused
            && missing
            && none_pending
            && empty
            && sent
            && a.as_deref() == Some(b"ping".as_slice())
            && b.as_deref() == Some(b"pong".as_slice())
            && conn.peer().ok() == Some((me, 0))
    });
    t.check("ipc: a closed peer is reported as hang-up, then BrokenPipe", || {
        let Ok(server) = Listener::listen("init-hangup") else { return false };
        let Ok(client) = Channel::connect("init-hangup") else { return false };
        let Ok(Some(conn)) = server.accept() else { return false };
        let queued = client.send(b"last words", &[]).is_ok();
        drop(client);
        let mut items = [item(conn.handle(), WAIT_READABLE)];
        let ready = wait(&mut items, Some(Duration::from_secs(1))) == Ok(1) && items[0].revents & WAIT_HANGUP != 0;
        let mut buf = [0u8; 16];
        // Queued messages are still delivered before the hang-up error.
        let last = conn.recv(&mut buf).ok().flatten().map(|(n, _)| n) == Some(10);
        let gone = conn.recv(&mut buf).err().map(|e| e.kind()) == Some(ErrorKind::BrokenPipe);
        let send_gone = conn.send(b"x", &[]).map_err(|e| e.kind()) == Err(ErrorKind::BrokenPipe);
        queued && ready && last && gone && send_gone
    });
    t.check("ipc: bounded queues say WouldBlock instead of blocking the sender", || {
        let Ok(server) = Listener::listen("init-bounded") else { return false };
        let Ok(client) = Channel::connect("init-bounded") else { return false };
        let Ok(Some(_conn)) = server.accept() else { return false };
        let mut sent = 0;
        let err = loop {
            match client.send(&[0u8; 32], &[]) {
                Ok(()) => sent += 1,
                Err(e) => break e.kind(),
            }
            if sent > 10_000 {
                break ErrorKind::Other;
            }
        };
        let big = client.send(&vec![0u8; ipc::MAX_MESSAGE + 1], &[]).map_err(|e| e.kind());
        sent == 256 && err == ErrorKind::WouldBlock && big == Err(ErrorKind::InvalidInput)
    });
    t.check("ipc: channel handles cannot travel in messages", || {
        let Ok(server) = Listener::listen("init-nochan") else { return false };
        let Ok(client) = Channel::connect("init-nochan") else { return false };
        let Ok(Some(_conn)) = server.accept() else { return false };
        client.send(b"x", &[client.handle()]).map_err(|e| e.kind()) == Err(ErrorKind::InvalidInput)
    });
    t.check("ipc: shared memory shared with another process over a channel", || {
        let Ok(server) = Listener::listen("init-shm") else { return false };
        let Ok(shm) = SharedMemory::create(64 * 1024) else { return false };
        let Ok(map) = shm.map(false) else { return false };
        let Ok(child) = worker(&["ipc-client", "init-shm"]).spawn() else { return false };
        // Wait for the child's connection.
        let mut items = [item(server.handle(), WAIT_READABLE)];
        if wait(&mut items, Some(Duration::from_secs(5))) != Ok(1) {
            return false;
        }
        let Ok(Some(conn)) = server.accept() else { return false };
        if conn.send(b"fill", &[shm.handle()]).is_err() {
            return false;
        }
        let mut items = [item(conn.handle(), WAIT_READABLE)];
        let answered = wait(&mut items, Some(Duration::from_secs(5))) == Ok(1);
        let mut buf = [0u8; 32];
        let reply = conn.recv(&mut buf).ok().flatten().map(|(n, _)| String::from_utf8_lossy(&buf[..n]).into_owned());
        let status = child.wait().map(|s| s.code()).ok().flatten();
        let msg = b"written by the worker through shared memory";
        // SAFETY: the mapping is at least 64 KiB; the child has exited.
        let seen = unsafe { core::slice::from_raw_parts(map.as_ptr(), msg.len()) } == msg;
        let child_pid = reply.as_deref().and_then(|r| r.strip_prefix("done ")).and_then(|p| p.parse::<u64>().ok());
        answered && seen && status == Some(0) && child_pid.is_some_and(|p| conn.peer().map(|x| x.0).ok() == Some(p))
    });
    t.check("ipc: wait_handles times out, then sees data", || {
        let Ok((mut r, mut w)) = ipc::pipe() else { return false };
        let mut items = [item(r.handle(), WAIT_READABLE)];
        let start = Instant::now();
        let none = wait(&mut items, Some(Duration::from_millis(50))) == Ok(0) && items[0].revents == 0;
        let waited = start.elapsed() >= Duration::from_millis(40);
        let polled = wait(&mut items, Some(Duration::ZERO)) == Ok(0);
        let _ = w.write_all(b"!");
        let ready = wait(&mut items, None) == Ok(1) && items[0].revents & WAIT_READABLE != 0;
        let mut b = [0u8; 1];
        none && waited && polled && ready && r.read(&mut b).ok() == Some(1)
    });
    t.check("ipc: kill ends a busy child with KILLED_STATUS", || {
        let Ok(child) = worker(&["spin", "20000"]).spawn() else { return false };
        os_std::thread::sleep(Duration::from_millis(50));
        let killed = system::kill(child.id()).is_ok();
        let status = child.wait().map(|s| s.raw());
        killed && status.ok() == Some(os_std::os::raw::KILLED_STATUS)
    });
    t.check("ipc: process and mount listings", || {
        let procs = system::processes().unwrap_or_default();
        let me = os_std::process::id().unwrap_or(0);
        let init = procs.iter().find(|p| p.pid == me);
        let mounts = system::mounts().unwrap_or_default();
        let root = mounts.iter().find(|m| system::field_str(&m.path) == "/");
        init.is_some_and(|p| system::process_name(p) == "init" && p.threads >= 1 && p.frames > 0)
            && root.is_some_and(|m| system::field_str(&m.fstype) == "ext4")
            && mounts.iter().any(|m| system::field_str(&m.path) == "/dev" && system::field_str(&m.fstype) == "devfs")
    });
    t.check("ipc: shared memory is returned to the kernel", || {
        let before = os::sysinfo().map(|i| i.free_memory).unwrap_or(0);
        for _ in 0..64 {
            let Ok(s) = SharedMemory::create(256 * 1024) else { return false };
            let Ok(m) = s.map(true) else { return false };
            drop(m);
        }
        let after = os::sysinfo().map(|i| i.free_memory).unwrap_or(0);
        // Page tables of the mappings may stay; 16 MiB of objects must not.
        before.abs_diff(after) < 512 * 1024
    });
    t.check("ipc: ordinary users cannot register services, take the display or read input", || {
        worker(&["user-ipc", "1000"]).status().map(|s| s.code()).ok() == Some(Some(0))
    });
}
