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
- `destroy_address_space` frees every user-half leaf frame and page table.

## Heaps

- Kernel: `kernel/mm/src/heap.rs` (first-fit, address-ordered free list,
  coalescing, 16-byte granularity) behind an `IrqSpinLock`; starts at 4 MiB and
  maps more frames when an allocation does not fit.
- User: the same algorithm in OSyx Std, growing through the `map` system call
  in chunks of at least 256 KiB.
