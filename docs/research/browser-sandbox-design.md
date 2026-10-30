# Browser content sandbox: implementation notes

Status: design only. Gecko content processes are not currently sandboxed.

The VFS has experimental checks for `open` and program loading when an
explicit lookup policy is supplied. No process currently receives such a
policy. Other filesystem operations, open handles, process inheritance and
system calls remain unrestricted, so these checks are not a sandbox.

OSyx must enforce the boundary in the kernel, independently of the Win32
compatibility process. A command-line flag or environment variable alone is
not an authority to enter or leave the sandbox.

## Boundary

- A trusted browser parent requests a content-process policy at creation.
  The kernel installs it before the child can execute userspace code.
- Restrictions survive `exec` and propagate to every descendant. A child
  cannot relax them or create an unrestricted descendant.
- Deny access by default. Explicitly allow only the files, IPC endpoints,
  inherited objects and system calls needed by content processes. The exact
  policy must come from observing and testing a real Gecko content process.
- Check filesystem access during kernel path resolution, including symlink
  expansion, case-insensitive lookups, mount crossings, creation and rename.
  Checking a caller-supplied string alone is insufficient.
- Account for already-open and IPC-transferred handles. A path restriction
  must not be bypassed by receiving a file object or using an inherited one.
  Define and enforce which objects a trusted broker may grant to a child.
- Apply syscall restrictions before all dispatcher fast paths. Cover process
  control, privileged devices, networking, IPC, memory and handle operations;
  a filtered call must fail closed rather than silently succeed.

## Validation before enabling

Test direct and indirect file access, symlink and rename escapes, `exec` and
child inheritance, pre-opened and transferred objects, and access to other
processes. Exercise concurrent policy installation and process startup. Test
the intended broker exchanges and Gecko startup, page rendering and network
access under the actual policy. Keep the existing launcher behaviour until
these checks pass; a partial filter must not be described as a sandbox.
