#!/usr/bin/env python3
"""Builds a FAT16 image from a directory tree, for the live ISO's El Torito
boot image (a FAT32 volume does not fit the 32 MiB / 65535-sector limit).

    mkfat16.py <image> <dir> [size MiB]

Every file name must be 8.3 and upper-case after conversion; nothing else
is representable (the live module tree itself lives in rootfs.z).
"""
import math
import os
import struct
import sys

SECTOR = 512
SPC = 4
CLUSTER = SECTOR * SPC
RESERVED = 1
NUM_FATS = 2
ROOT_ENTRIES = 256
ROOT_SECS = ROOT_ENTRIES * 32 // SECTOR
MIN_MIB = 9  # >= 4085 clusters, the FAT16 lower bound
MAX_MIB = 31  # El Torito sector count is 16 bits of 512-byte sectors
FAT_DATE = ((2026 - 1980) << 9) | (1 << 5) | 1
ATTR_RO = 0x01
ATTR_VOLUME = 0x08
ATTR_DIR = 0x10
ATTR_ARCHIVE = 0x20
EOC = 0xFFFF


class Node:
    def __init__(self, name, is_dir, data=b""):
        self.name = name
        self.is_dir = is_dir
        self.data = data
        self.children = []
        self.first = 0


def short_name(name):
    upper = name.upper()
    if upper in (".", ".."):
        return upper.ljust(11).encode()
    if "." in name:
        base, _, ext = upper.rpartition(".")
    else:
        base, ext = upper, ""
    if not base or len(base) > 8 or len(ext) > 3 or "." in base:
        raise SystemExit("mkfat16: %r is not an 8.3 name" % name)
    allowed = set("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789$%'-_@~`!(){}^#&")
    if any(c not in allowed for c in base + ext):
        raise SystemExit("mkfat16: %r has characters 8.3 cannot hold" % name)
    return (base.ljust(8) + ext.ljust(3)).encode()


def load(path, name="", root=True):
    if root:
        node = Node("", True)
        entries = sorted(os.listdir(path))
    else:
        node = Node(name, os.path.isdir(path))
        if node.is_dir:
            entries = sorted(os.listdir(path))
        else:
            with open(path, "rb") as f:
                node.data = f.read()
            return node
    for entry in entries:
        node.children.append(load(os.path.join(path, entry), entry, False))
    return node


def clusters_for(size):
    return max(1, math.ceil(size / CLUSTER))


def dir_entries(node):
    return 2 + len(node.children)


def needs(node):
    """Clusters required by the subtree (the root directory is fixed-size)."""
    if not node.is_dir:
        return clusters_for(len(node.data))
    total = clusters_for(dir_entries(node) * 32) if node.name else 0
    return total + sum(needs(child) for child in node.children)


def layout(total_sectors):
    fatsz = 1
    while True:
        data_secs = total_sectors - RESERVED - NUM_FATS * fatsz - ROOT_SECS
        if data_secs <= 0:
            return None
        clusters = data_secs // SPC
        need = math.ceil((clusters + 2) * 2 / SECTOR)
        if need <= fatsz:
            return fatsz, clusters
        fatsz = need


def dirent(name11, attr, cluster, size):
    return struct.pack(
        "<11sBBBHHHHHHHI",
        name11,
        attr,
        0,  # NTRes
        0,  # CrtTimeTenth
        0,  # CrtTime
        FAT_DATE,  # CrtDate
        FAT_DATE,  # LstAccDate
        cluster >> 16,  # FstClusHI
        0,  # WrtTime
        FAT_DATE,  # WrtDate
        cluster & 0xFFFF,  # FstClusLO
        size,
    )


def build(root, size_mib=None):
    tree = load(root)
    clusters_needed = needs(tree)

    def fits(mib):
        lay = layout(mib * 1024 * 1024 // SECTOR)
        return lay is not None and clusters_needed <= lay[1]

    if size_mib is None:
        size_mib = MIN_MIB
        while not fits(size_mib):
            size_mib += 1
            if size_mib > MAX_MIB:
                raise SystemExit(
                    "mkfat16: content needs more than the %d MiB El Torito limit (%d clusters)"
                    % (MAX_MIB, clusters_needed)
                )
    elif not fits(size_mib):
        raise SystemExit("mkfat16: content does not fit %d MiB" % size_mib)

    total_sectors = size_mib * 1024 * 1024 // SECTOR
    fatsz, clusters = layout(total_sectors)
    data_start = RESERVED + NUM_FATS * fatsz + ROOT_SECS
    image = bytearray(total_sectors * SECTOR)
    fat = [0] * (clusters + 2)
    fat[0] = 0xFFF8
    fat[1] = 0xFFFF
    state = {"next": 2}

    def alloc(n):
        first = state["next"]
        if first + n > clusters + 2:
            raise SystemExit("mkfat16: out of clusters")
        for i in range(n):
            fat[first + i] = EOC if i == n - 1 else first + i + 1
        state["next"] += n
        return first

    def place(cluster, data):
        at = (data_start + (cluster - 2) * SPC) * SECTOR
        image[at:at + len(data)] = data

    def assign(node):
        for child in node.children:
            if child.is_dir:
                assign(child)
            else:
                if len(child.data) == 0:
                    child.first = 0
                else:
                    child.first = alloc(clusters_for(len(child.data)))
                    place(child.first, child.data)
        if node.name:  # not the root: give the directory its clusters
            node.first = alloc(clusters_for(dir_entries(node) * 32))

    def table(node, parent_first):
        entries = bytearray()
        if node.name:
            entries += dirent(short_name("."), ATTR_DIR, node.first, 0)
            entries += dirent(short_name(".."), ATTR_DIR, parent_first, 0)
        for child in node.children:
            attr = ATTR_DIR if child.is_dir else ATTR_ARCHIVE
            entries += dirent(short_name(child.name), attr, child.first, 0 if child.is_dir else len(child.data))
        return bytes(entries)

    assign(tree)

    root_entries = bytearray()
    root_entries += dirent(short_name("OSYXLIVE"), ATTR_VOLUME, 0, 0)
    root_entries += table(tree, 0)
    root_at = (RESERVED + NUM_FATS * fatsz) * SECTOR
    image[root_at:root_at + len(root_entries)] = root_entries

    def write_dirs(node, parent_first):
        if not node.name:
            for child in node.children:
                if child.is_dir:
                    write_dirs(child, 0)
            return
        body = table(node, parent_first)
        place(node.first, body)
        for child in node.children:
            if child.is_dir:
                write_dirs(child, node.first)

    # Root children get the root (cluster 0) as their parent.
    for child in tree.children:
        if child.is_dir:
            write_dirs(child, 0)

    fat_bytes = b"".join(struct.pack("<H", v) for v in fat)
    assert len(fat_bytes) <= fatsz * SECTOR
    for copy in range(NUM_FATS):
        at = (RESERVED + copy * fatsz) * SECTOR
        image[at:at + len(fat_bytes)] = fat_bytes

    boot = bytearray(SECTOR)
    boot[0:3] = b"\xeb\x3c\x90"
    boot[3:11] = b"MSWIN4.1"
    struct.pack_into("<HBHBHHBHHHII", boot, 11, SECTOR, SPC, RESERVED, NUM_FATS, ROOT_ENTRIES,
                     total_sectors, 0xF8, fatsz, 63, 255, 0, 0)
    boot[36] = 0x80
    boot[38] = 0x29
    struct.pack_into("<I", boot, 39, 0x4F535958)
    boot[43:54] = b"OSYXLIVE   "
    boot[54:62] = b"FAT16   "
    boot[62:64] = b"\xfa\xf4"  # cli; hlt: the volume is never booted as a floppy
    boot[510:512] = b"\x55\xaa"
    image[0:SECTOR] = boot
    return bytes(image), size_mib, clusters, state["next"] - 2


def main():
    if len(sys.argv) not in (3, 4):
        sys.exit(__doc__)
    out, root = sys.argv[1], sys.argv[2]
    size = int(sys.argv[3]) if len(sys.argv) == 4 else None
    image, mib, clusters, used = build(root, size)
    with open(out, "wb") as f:
        f.write(image)
    print("%s: FAT16 %d MiB, %d clusters, %d used, %d bytes" % (out, mib, clusters, used, len(image)))


if __name__ == "__main__":
    main()
