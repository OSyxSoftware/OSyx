# Per-process mapping locks

Anonymous mappings, file views, shared memory, image COW, prefaults and
compressed-memory reclaim now use a separate `ProcessMemory` lock per process.
Previously those operations held the global process `TABLE` throughout page
allocation, zeroing, page-table edits, shootdowns and compression.

`MEMORY` is an independent PID-to-`Arc` index. Readers clone the reference and
release the index before acquiring a memory lock. This lets faults proceed even
when lifecycle or process-info operations hold `TABLE`. Metadata and lifecycle
operations still use `TABLE`; operations in the same address space still
serialize, and the low-level `PAGING` lock still serializes page-table edits.

Spawn publishes memory before waking the initial thread. Exec replaces mapping
state under the process memory lock. Exit removes the index entry, marks memory
inactive and releases frames under that lock. A retained reference cannot make
a released root usable again. Shared futex keys remain protected by their own
index; acquire the memory lock before that index, never in reverse order.

`MAP`, `MAP_EX`, `UNMAP`, `PROTECT` and `DISCARD` now bypass the kernel entry lock.
They use scalar arguments and synchronize with faults and teardown through the
same memory lock. Syscall filtering still runs before dispatch, and the fast
path retains kill and suspension safe points. Other syscall paths still retain
the entry lock; this change does **not** complete its removal.

## Verification

Run `./scripts/memory-lock-test.ps1` with an existing UEFI loader in
`build/esp/EFI/BOOT/BOOTX64.EFI`. It builds the kernel, init and worker, then boots
a temporary FAT directory under four-CPU QEMU TCG. No physical disk is used.
`-CpuCount 2` exercises the same paths on two CPUs; `-SkipBuild` uses existing
binaries.

The internal test resolves a real anonymous fault while holding `TABLE` and a
different process's memory lock. It checks zeroed contents and invalidation on
teardown. Two userspace processes then create one pinned thread per CPU (up to
four). Each thread repeatedly maps 256 KiB, checks zeros, writes every page,
changes protection, checks retained contents, discards, checks zeros and unmaps.
The run also captures `prof=4` output. This is a targeted test, not the full
system suite. The temporary FAT backend may reject the raw sector write in the
unrelated block-I/O self-test; the script permits only that exact failure.

For a baseline, build an older kernel separately and pass
`-SkipBuild -BaselineKernel <kernel.elf>`. The identical init and worker binaries
run against that kernel. This mode cannot require the new internal test.

On 2026-11-02, a four-CPU comparison against `d8c6407` completed 5,262 mapping
cycles on the baseline and 6,023 after this change. Another changed-kernel run
completed 8,281 cycles: QEMU TCG results vary substantially. These observations
do not establish a stable speedup on physical hardware or in Zen.

Both compared runs used a 1 GiB VM, two processes, eight pinned threads, six
seconds per thread and `prof=4`. The profiler's dispatch self-samples changed
from 3,829 / 11,021 retained on-CPU samples to 16 / 1,284. Sampling windows include
boot tests, interrupt masking affects sampling, and compiler inlining affects
attribution. Interpret these as evidence of changed contention, not precise
execution-time measurements. `scripts/flamegraph.py` can symbolize each serial
log using that run's kernel ELF.

A separate four-CPU, 192 MiB `init.args=tests,swap-stress` run verified 24,686
pages after memory pressure. Swap counters reported 10,320 evictions and 10,305
restores, with zero compressed pages left after exit. The run reported no frame
leak; it retained the same unrelated FAT block-write self-test failure.

Hardware validation, exhaustive fault/unmap/exec races and a browser workload
remain necessary. The global entry lock and global low-level paging lock remain
separate follow-up work.
