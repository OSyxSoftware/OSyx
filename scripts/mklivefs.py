#!/usr/bin/env python3
"""Packs a staged OSyx root tree into the live root boot module.

Format: b"OSYXLR1\\0" + u64le(uncompressed size) + zlib(cpio newc archive).
The kernel (kernel/src/live.rs) inflates it and replays the archive into
the tmpfs root. Run inside WSL as root, on an ext4 tree, so ownership and
modes are the ones chown/chmod left in the tree.

    mklivefs.py <root dir> <output .z>
"""
import os
import stat
import struct
import sys
import zlib

TRAILER = "TRAILER!!!"


def entry_names(root):
    """Every path below root, parents before children, stable order."""
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        rel_dir = os.path.relpath(dirpath, root)
        for d in dirnames:
            rel = d if rel_dir == "." else rel_dir + "/" + d
            yield os.path.join(dirpath, d), rel, True
        for f in filenames:
            rel = f if rel_dir == "." else rel_dir + "/" + f
            yield os.path.join(dirpath, f), rel, False


def cpio_record(ino, name, st, data):
    name_b = name.encode() + b"\0"
    fields = [
        ino,
        st.st_mode,
        st.st_uid,
        st.st_gid,
        st.st_nlink,
        int(st.st_mtime),
        len(data),
        0,
        0,
        0,
        0,
        len(name_b),
        0,
    ]
    out = bytearray(b"070701")
    for f in fields:
        out += b"%08x" % f
    out += name_b
    out += b"\0" * (-len(out) % 4)
    out += data
    out += b"\0" * (-len(out) % 4)
    return out


def pack(root):
    archive = bytearray()
    ino = 1
    count = 0
    skipped = 0
    for path, rel, _ in entry_names(root):
        st = os.lstat(path)
        kind = stat.S_IFMT(st.st_mode)
        if kind == stat.S_IFDIR:
            data = b""
        elif kind == stat.S_IFLNK:
            data = os.readlink(path).encode()
        elif kind == stat.S_IFREG:
            with open(path, "rb") as f:
                data = f.read()
        else:
            skipped += 1
            continue
        archive += cpio_record(ino, rel, st, data)
        ino += 1
        count += 1
    trailer = cpio_record(ino, TRAILER, os.stat(root), b"")
    archive += trailer
    archive += b"\0" * (-len(archive) % 512)
    return bytes(archive), count, skipped


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    root, out = sys.argv[1], sys.argv[2]
    archive, count, skipped = pack(root)
    blob = zlib.compress(archive, 9)
    with open(out, "wb") as f:
        f.write(b"OSYXLR1\0")
        f.write(struct.pack("<Q", len(archive)))
        f.write(blob)
    print(
        "%s: %d entries (%d skipped), cpio %d bytes, zlib %d bytes (%.1f%%), module %d bytes"
        % (out, count, skipped, len(archive), len(blob), 100.0 * len(blob) / len(archive), len(blob) + 16)
    )


if __name__ == "__main__":
    main()
