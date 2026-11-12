# Packages

`/bin/pkg` installs single applications from a repository. A package is
signed like a system update (`shared/update`, Ed25519) and can write only
its own directory.

```text
pkg list              what the repository has, and what is installed
pkg info NAME         a package's version, size and files
pkg install NAME...   install or upgrade (root)
pkg upgrade           upgrade every installed package (root)
pkg remove NAME...    remove (root)
pkg installed         what is installed, without asking the repository
```

Installing, upgrading and removing need root: `elevate pkg install NAME`
asks in the desktop's User Account Control prompt. `elevate` does not show
the output of the program it starts, so what root's `pkg` did, and why it
refused, is also in `/var/log/osyx-pkg.log`.

## What a package may write

* `/opt/pkg/<name>/`: its files;
* `/system/share/applications/pkg-<name>.app`: its launcher entry, in the
  format of the system's own (`Name=`, `Exec=`, `Icon=`, `Comment=`,
  `Handles=`).

Nothing else (`osyx_update::package::allowed_path`): a manifest that names
any other path is refused whole, so a package cannot replace a part of the
system or of another package. System updates never write `/opt/pkg`.

## The repository

A directory of files over HTTPS; a GitHub Releases download
directory will do. `url=` in `/etc/osyx/packages.conf` names it; without
that file the system update's `url=` (`/etc/osyx/update.conf`) is used.

| File | Content |
|---|---|
| `packages.txt`, `packages.sig` | the list of packages and its signature |
| `<name>.pkg.txt`, `<name>.pkg.sig` | a package's manifest and its signature |
| `<name>.pkg.bin` | the package's files, concatenated in manifest order |

```text
osyx-packages 1
package hello 1.1.0 A sample package
```

```text
osyx-package 1
name hello
version 1.1.0
description A sample package
payload <sha256> <size>
file <sha256> <size> <mode, octal> opt/pkg/hello/hello.ax
```

Names are lower case letters, digits and hyphens. Versions are dotted
numbers, ordered as for system updates.

## What is checked

1. The list and each manifest must carry a signature of a trusted key: the
   OSyx release key built into the system, or a key in
   `/etc/osyx/packages.keys` (one public key in hex per line; `#` starts a
   comment). A file without one is refused before anything is downloaded.
2. A package is installed only in the version the signed list names: a
   manifest of another version, however well signed, is refused.
3. The payload may be exactly as long as the manifest says; more bytes are
   refused as they arrive. It is checked against the manifest's SHA-256,
   and each file against its own while it is written.
4. Every file is written next to its target and renamed into place only
   when all of them were written. If a rename fails half way, the files of
   the installed version are put back.
5. An older version than the installed one is not installed.

A repository given as plain HTTP is refused: someone on the network path
could answer with an older signed list together with its packages, and keep
a system from its fixes. A redirect from HTTPS to plain HTTP is refused
before the HTTP request is made, for every download of the system
(`osyx_web::get`: packages, system updates, `fetch`, `get`).

What is installed is recorded in `/var/lib/osyx/pkg/<name>.txt` (the
manifest). An upgrade removes the files the new version no longer has;
`remove` removes the files of the manifest and the directories they leave
empty, and keeps the record when a file could not be removed, so that it
can be tried again. Files an upgrade drops are listed in `<name>.stale`
before the new version takes the old one's place, and stay listed until
they are gone; they are tried again by every `install`, `upgrade` and
`remove`, also when the package is already up to date.

One `pkg` changes what is installed at a time. The lock is a service name
held in the kernel (`pkg-lock`), not a file: it is free again the moment
its holder ends, however it ends.

## Publishing

```sh
# A key, once (the public half goes to the systems' packages.keys):
cargo run -p osyx-update --example release -- keygen my.key my.pub

# The tree holds the files where they go: tree/opt/pkg/hello/...,
# tree/system/share/applications/pkg-hello.app
cargo run -p osyx-update --example package -- build hello 1.1.0 "A sample package" tree my.key repo
cargo run -p osyx-update --example package -- index my.key repo
```

`build` refuses a tree with a file outside the package's own paths.
`index` lists a package only when its signature and payload are there and
match its manifest. Files
with an execute bit are installed 0755, the others 0644.

## Limits

* There are no dependencies between packages and no scripts run at
  installation.
* One repository at a time.
* There is no official package repository yet: the default URL is the
  system update's, which publishes no `packages.txt`.
* No graphical front end; Settings lists only the system's applications.
