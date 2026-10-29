# Memory management

## Virtual address space

| Range | Use |
|---|---|
| `0x0000_0000_0000_0000 – 0x0000_0000_0000_FFFF` | never mapped (null guard) |
| `0x0000_0000_0040_0000 …` | user program segments (`userspace/user.ld`) |
| `0x0000_1000_0000_0000 …` | user anonymous mappings (`map`), guard page after each |
| `… – 0x0000_7FFF_FFFF_0000` | user stack (256 KiB) |
| `0xFFFF_8000_0000_0000 + phys` | HHDM: direct map of RAM (NX) |
| `0xFFFF_C000_0000_0000` | kernel heap (grows on demand, max 1 GiB) |
| `0xFFFF_D000_0000_0000` | scratch page for paging self-tests |
| `0xFFFF_E000_0000_0000` (512 GiB) | MMIO window: device BARs mapped uncached, NX (`map_mmio`) |
| `0xFFFF_FFFF_8000_0000` | kernel image |

## Physical frames (`kernel/mm/src/frames.rs`)

A bitmap with one bit per 4 KiB frame up to the end of the highest usable
region, stored in the first usable region above 1 MiB. Only `Usable` (and
later reclaimed `BootloaderReclaimable`) frames ever become free. Frames below
1 MiB are never handed out. Double frees, unaligned frees and out-of-range
frees return errors. Host unit tests cover exhaustion, uniqueness, reclaim and
invalid frees.

## Paging (`kernel/src/mem/paging.rs`)

- Functions take an explicit PML4 root. All 256 upper-half PDPTs are allocated
  at boot, so every process root shares the kernel half by copying 256 entries.
- `map_in` rejects unaligned and non-canonical addresses, user mappings outside
  `[64 KiB, 128 TiB)`, kernel mappings in the lower half, the HHDM or the
  kernel image, and already-mapped pages.
- Leaf permissions: writable only on request; NX unless executable (when the
  CPU supports NX). `CR0.WP` makes read-only pages read-only for the kernel too.
- `destroy_address_space` frees every user-half leaf frame and page table, so
  frames owned by someone else (shared memory, file views, shared images) are
  unmapped first.
- `edit_range_in` rewrites or clears the present leaves of a range as a
  callback decides, with one TLB shootdown for the whole range;
  `replace_in` swaps the frame of one present leaf (copy-on-write).

## Shared executable images (`kernel/src/image.rs`)

Windows programs map large DLLs (Zen's `xul.dll` is about 170 MiB laid out).
Without sharing, every process of a browser built its own relocated copy.
Now the first process that loads a PE file at some base lays it out and
relocates it in its own anonymous region, then calls `image_publish`: the
region's frames move into an `ImageSection` and stay mapped read-only.
A later process of the same user asks `image_query` for that base, reserves
the same addresses and calls `image_map`, which maps the section's frames
read-only instead of building a copy.

- A write to such a page (import binding, `.data`, relocated pointers patched
  at run time) is a protection fault; `process::cow_fault` gives the process
  a private copy and retries. Kernel copies to user memory take the same path
  (`CR0.WP`). A page is shared exactly while its entry points at the
  section's frame; `protect` keeps such pages read-only even when the region
  becomes writable.
- Sections are keyed by volume, inode, size, modification time, base, page
  count and user id. Writing or truncating the file drops its keys (mappings
  keep their section). The kernel does not check the published bytes, so
  images are only shared between processes of the same user.
- The lookup table holds weak references: a section lives while some
  process maps it. `unmap`, `discard`, `execve` and exit unmap shared frames
  instead of freeing them; frame accounting at teardown stays exact.
- Publishing logs `image: published …` with the live sections, the memory
  they hold and how many maps reused one.

Only images at the same base can be shared: a process whose address space
already uses the published base builds a private copy. Child processes of
the same program load their DLLs in the same order and in practice get the
same bases (Zen: 29 of 29 maps reused). Fixed mappings no longer move the
anonymous-mapping cursor; it skips over them instead. Untouched pages of a
published image are filled with zeroed frames at publication.

Placing DLLs at name-hashed slots high in the address space
(`0x6000_0000_0000`) was tried and dropped: Zen then never showed its window.

## Heaps

- Kernel: `kernel/mm/src/heap.rs` (first-fit, address-ordered free list,
  coalescing, 16-byte granularity) behind an `IrqSpinLock`; starts at 4 MiB and
  maps more frames when an allocation does not fit.
- User: the same algorithm in OSyx Std, growing through the `map` system call
  in chunks of at least 256 KiB.

## Compressed memory

OSyx can reclaim cold, writable private anonymous pages into compressed
kernel memory. This is a RAM-backed mechanism, not Linux's zram block
device. Shared mappings, image sections, and read-only pages are excluded.
All-zero pages need no compressed payload. Other pages are retained only if
they compress to at most three quarters of a page; compressed payloads are
limited to 256 MiB (metadata and allocator overhead are additional).
The `swap.off` boot option disables the worker.

The worker ages bounded windows and keeps a cursor in each region so that
incompressible pages cannot indefinitely hide later pages. It invalidates
cached translations after aging and removes a mapping before compressing.
A later access restores the page. When a user fault has no frame, it wakes
the worker and waits for a bounded time; exhaustion can still fail a fault.

Run `scripts/swap-test.ps1` to build a diagnostic disk and boot a 192 MiB,
two-CPU QEMU guest. The test fills anonymous pages with nonzero markers,
checks them after reclaim, and requires nonzero eviction and restore counts
and zero remaining compressed pages after exit. It does not establish Zen
browser functionality or cover every SMP and memory-exhaustion scenario.
The script replaces `build/osyx.img` with its diagnostic image.
