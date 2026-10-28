#!/usr/bin/env python3
"""Builds the OSyx live ISO: ISO9660 with an El Torito boot catalog whose
boot image is a FAT16 volume. The ESP tree is also visible as regular ISO
files, so UEFI-aware USB tools can extract it. A plain optical-disc image:
the first 16 sectors are the zero-filled ISO system area, with no MBR.

    mkiso.py <output.iso> <fat16 image> <ESP directory>
"""
import datetime
from pathlib import Path
import re
import struct
import sys

SECTOR = 2048
PVD_LBA = 16
BOOT_RECORD_LBA = 17
TERMINATOR_LBA = 18
CATALOG_LBA = 19
ROOT_LBA = 20
LPATH_LBA = 21
MPATH_LBA = 22
README_LBA = 23
IMAGE_LBA = 32
README_NAME = "README.TXT"
README = b"""OSyx live image

This CD/DVD image boots the OSyx system entirely from RAM: the kernel is
loaded by the UEFI loader, which also passes it the compressed root file
system (EFI\\OSYX\\BIN\\ROOTFS.Z). Nothing is written to any disk.

Requirements: x86-64 UEFI firmware, at least 512 MiB of RAM, USB keyboard
and mouse.
"""
FILE_NAME = re.compile(r"[A-Z0-9_]{1,8}(\.[A-Z0-9_]{1,3})?\Z")
DIR_NAME = re.compile(r"[A-Z0-9_]{1,8}\Z")


def both16(v):
    return struct.pack("<H", v) + struct.pack(">H", v)


def both32(v):
    return struct.pack("<I", v) + struct.pack(">I", v)


def date7():
    now = datetime.datetime.now(datetime.timezone.utc)
    return bytes([now.year - 1900, now.month, now.day, now.hour, now.minute, now.second, 0])


def date17():
    now = datetime.datetime.now(datetime.timezone.utc)
    return ("%04d%02d%02d%02d%02d%02d00\0" % (now.year, now.month, now.day, now.hour, now.minute, now.second)).encode()


def dir_record(name, extent, size, is_dir):
    rec = bytearray(33 + len(name) + (len(name) % 2 == 0))
    rec[0] = len(rec)
    rec[2:10] = both32(extent)
    rec[10:18] = both32(size)
    rec[18:25] = date7()
    rec[25] = 0x02 if is_dir else 0
    rec[28:32] = both16(1)
    rec[32] = len(name)
    rec[33:33 + len(name)] = name
    return bytes(rec)


def pvd(total_blocks, root_record, path_table_size):
    d = bytearray(SECTOR)
    d[0] = 1
    d[1:6] = b"CD001"
    d[6] = 1
    d[8:40] = b"OSYX".ljust(32)
    d[40:72] = b"OSYXLIVE".ljust(32)
    d[80:88] = both32(total_blocks)
    d[120:124] = both16(1)
    d[124:128] = both16(1)
    d[128:132] = both16(SECTOR)
    d[132:140] = both32(path_table_size)
    d[140:144] = struct.pack("<I", LPATH_LBA)
    d[148:152] = struct.pack(">I", MPATH_LBA)
    d[156:156 + len(root_record)] = root_record
    d[190:318] = b" " * 128
    d[318:446] = b" " * 128
    d[446:574] = b" " * 128
    d[574:702] = b" " * 128
    d[702:739] = b" " * 37
    d[739:776] = b" " * 37
    d[776:813] = b" " * 37
    d[813:830] = date17()
    d[830:847] = date17()
    d[847:864] = b"0" * 16 + b"\0"
    d[864:881] = b"0" * 16 + b"\0"
    d[881] = 1
    return bytes(d)


def boot_record():
    d = bytearray(SECTOR)
    d[0] = 0
    d[1:6] = b"CD001"
    d[6] = 1
    d[7:39] = b"EL TORITO SPECIFICATION".ljust(32, b"\0")
    d[71:75] = struct.pack("<I", CATALOG_LBA)
    return bytes(d)


def boot_catalog(fat_sectors):
    d = bytearray(SECTOR)
    d[0] = 1
    d[1] = 0xEF
    d[4:28] = b"OSYX LIVE".ljust(24, b"\0")
    d[30:32] = b"\x55\xaa"
    words = struct.unpack("<16H", bytes(d[0:32]))
    d[28:30] = struct.pack("<H", (-sum(words)) & 0xFFFF)
    d[32] = 0x88
    d[33] = 0
    d[38:40] = struct.pack("<H", min(fat_sectors, 0xFFFF))
    d[40:44] = struct.pack("<I", IMAGE_LBA)
    return bytes(d)


def scan_dir(path):
    entries = []
    for child in sorted(path.iterdir(), key=lambda p: p.name):
        name = child.name.upper()
        if child.is_dir():
            if not DIR_NAME.fullmatch(name):
                sys.exit(f"mkiso: directory name is not ISO9660 8.3: {child}")
            entries.append({"name": name.encode(), "dir": True, "children": scan_dir(child)})
        else:
            if not FILE_NAME.fullmatch(name):
                sys.exit(f"mkiso: file name is not ISO9660 8.3: {child}")
            entries.append({"name": (name + ";1").encode(), "dir": False, "data": child.read_bytes()})
    return entries


def directory_data(entry):
    data = bytearray()
    data += dir_record(b"\0", entry["extent"], SECTOR, True)
    data += dir_record(b"\1", entry.get("parent", entry)["extent"], SECTOR, True)
    for child in entry["children"]:
        data += dir_record(child["name"], child["extent"], SECTOR if child["dir"] else len(child["data"]), child["dir"])
    if len(data) > SECTOR:
        sys.exit("mkiso: a directory exceeds one ISO sector")
    return bytes(data)


def allocate_dirs(entries, next_lba, parent=None):
    for entry in entries:
        if not entry["dir"]:
            continue
        entry["extent"] = next_lba
        entry["parent"] = parent or entry
        next_lba += 1
        next_lba = allocate_dirs(entry["children"], next_lba, entry)
    return next_lba


def allocate_files(entries, next_lba):
    for entry in entries:
        if entry["dir"]:
            next_lba = allocate_files(entry["children"], next_lba)
        elif "extent" not in entry:
            entry["extent"] = next_lba
            next_lba += (len(entry["data"]) + SECTOR - 1) // SECTOR
    return next_lba


def path_records(entries, number, parent_number, records):
    for entry in entries:
        if not entry["dir"]:
            continue
        records.append((entry, number, parent_number))
        number, records = path_records(entry["children"], number + 1, number, records)
    return number, records


def path_tables(root):
    _, records = path_records(root["children"], 2, 1, [])
    records.insert(0, (root, 1, 1))
    little, big = bytearray(), bytearray()
    for entry, number, parent in records:
        name = b"\0" if number == 1 else entry["name"]
        record = bytes([len(name), 0]) + struct.pack("<I", entry["extent"]) + struct.pack("<H", parent) + name
        little += record + (b"\0" if len(name) % 2 else b"")
        record = bytes([len(name), 0]) + struct.pack(">I", entry["extent"]) + struct.pack(">H", parent) + name
        big += record + (b"\0" if len(name) % 2 else b"")
    if len(little) > SECTOR or len(big) > SECTOR:
        sys.exit("mkiso: path table exceeds one ISO sector")
    return bytes(little), bytes(big)


def write_tree(image, entries):
    for entry in entries:
        start = entry["extent"] * SECTOR
        data = directory_data(entry) if entry["dir"] else entry["data"]
        image[start:start + len(data)] = data
        if entry["dir"]:
            write_tree(image, entry["children"])


def main():
    if len(sys.argv) != 4:
        sys.exit(__doc__)
    out, fat_path, esp_path = sys.argv[1:]
    fat = Path(fat_path).read_bytes()
    fat_sectors = (len(fat) + 511) // 512
    if fat_sectors > 0xFFFF:
        sys.exit("mkiso: the boot image exceeds the El Torito 16-bit sector count")
    readme = README.ljust(((len(README) + SECTOR - 1) // SECTOR) * SECTOR, b"\n")

    root = {"name": b"\0", "dir": True, "extent": ROOT_LBA, "children": scan_dir(Path(esp_path))}
    root["children"].append({"name": (README_NAME + ";1").encode(), "dir": False, "extent": README_LBA, "data": README})
    next_lba = IMAGE_LBA + (len(fat) + SECTOR - 1) // SECTOR
    next_lba = allocate_dirs(root["children"], next_lba, root)
    total = allocate_files(root["children"], next_lba)
    image = bytearray(total * SECTOR)

    root_record = dir_record(b"\0", ROOT_LBA, SECTOR, True)
    ltab, mtab = path_tables(root)
    image[PVD_LBA * SECTOR:(PVD_LBA + 1) * SECTOR] = pvd(total, root_record, len(ltab))
    image[BOOT_RECORD_LBA * SECTOR:(BOOT_RECORD_LBA + 1) * SECTOR] = boot_record()
    terminator = bytearray(SECTOR)
    terminator[0] = 255
    terminator[1:6] = b"CD001"
    terminator[6] = 1
    image[TERMINATOR_LBA * SECTOR:(TERMINATOR_LBA + 1) * SECTOR] = terminator
    image[CATALOG_LBA * SECTOR:(CATALOG_LBA + 1) * SECTOR] = boot_catalog(fat_sectors)
    image[ROOT_LBA * SECTOR:ROOT_LBA * SECTOR + SECTOR] = directory_data(root).ljust(SECTOR, b"\0")
    image[LPATH_LBA * SECTOR:LPATH_LBA * SECTOR + len(ltab)] = ltab
    image[MPATH_LBA * SECTOR:MPATH_LBA * SECTOR + len(mtab)] = mtab
    image[README_LBA * SECTOR:README_LBA * SECTOR + len(readme)] = readme
    image[IMAGE_LBA * SECTOR:IMAGE_LBA * SECTOR + len(fat)] = fat
    write_tree(image, root["children"])

    Path(out).write_bytes(image)
    print(
        "%s: ISO9660 %d KiB (boot image %d KiB at LBA %d, %d sectors; EFI files visible)"
        % (out, len(image) >> 10, len(fat) >> 10, IMAGE_LBA, fat_sectors)
    )


if __name__ == "__main__":
    main()
