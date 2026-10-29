# OpenBSD driver compatibility layer

`openbsd/` holds unmodified OpenBSD kernel sources (ISC and BSD licensed;
every file keeps its own copyright and permission notice; both licences
are compatible with OSyx's GPL-3.0-or-later). The compatibility layer
itself (`compat/`, `include/osyx_bsd.h`) is GPL-3.0-or-later. They are compiled
with clang by `kernel/build.rs` and run on `compat/compat.c`, which
implements the OpenBSD kernel interfaces they use (mbufs, bus_space,
bus_dma, PCI, autoconf, ifnet/ifq, ifmedia, timeouts, tasks) on OSyx kernel
services (`kernel/src/bsd.rs`). `include/` maps each OpenBSD header that is
not taken unchanged to `include/osyx_bsd.h`.

Built in now: re(4) (RTL8111/8168/8101/8169, RTL8139C+), rl(4) (RTL8139)
and the PHY drivers rgephy, rlphy and ukphy; iwm(4) Wi-Fi on net80211; and
azalia(4) for High Definition Audio controllers and codecs. Interrupt
handlers are polled every millisecond by the `bsd` kernel thread; DMA
memory lies below 4 GiB.

Sound: `compat/audio.c` is the part of OpenBSD's audio(4) that drives a
driver's `audio_hw_if` ring (playback, 48 kHz, 16-bit stereo); the kernel
side (`kernel/src/audio.rs`) offers `/dev/audio` (write PCM), `/dev/mixer`
(`volume=N`, `mute=0|1`) and `/run/audio.status`. HDMI-only controllers
(vendor 1002) are used only when there is no other. While azalia waits for
codec responses with `audio_lock` held, `delay()` acknowledges the RIRB
interrupt flag, because QEMU's intel-hda stops answering until it is
cleared (RINTCNT = 1) and the interrupt handler cannot run then.

Adding a driver: copy its OpenBSD sources (and the PHY drivers it needs)
under `openbsd/`, list them in `BSD_SOURCES` in `kernel/build.rs`, add its
`cfattach`/`cfdriver` to the tables in `compat.c`, and supply whatever
kernel interface it uses that `osyx_bsd.h` lacks.

Source: https://github.com/openbsd/src (master, 2026-10-29).

## Bluetooth from NetBSD (in progress, not built)

`netbsd/` holds NetBSD's Bluetooth stack (`netbt`: HCI, L2CAP, RFCOMM, SCO;
BSD licensed, Iain Hibbert / Itronix) with `ubt(4)` and the in-kernel HID
drivers `bthidev`, `btkbd`, `btms`. `netbsd-shim/` contains placeholder
headers that only let the files be syntax-checked; nothing here is in
`BSD_SOURCES` yet. Measured gap (2026-10-29), about 800 compile errors in
four blocks:

1. NetBSD kernel basics on top of `osyx_bsd.h`: `device_t`/`device_private`,
   `aprint_*`, `bool`, `kmutex_t`/`mutex_enter`, `kmem_alloc`, `callout_*`,
   condvars, `MBUFQ_*`, sysctl stubs, proplib dictionaries (bthidev's
   attach arguments), wscons `wskbd`/`wsmouse` mapped to the OSyx input core.
2. `usbdi` (`usbd_open_pipe`, `usbd_setup_xfer`, `usbd_transfer`, ...) on
   the Rust xHCI driver: control, bulk and interrupt pipes for devices the
   Rust driver leaves unconfigured (class E0/01/01).
3. Intel controller firmware download (8087:0025 needs `ibt-18-16-1.sfi`
   before it leaves bootloader mode), written after FreeBSD's `iwmbtfw`.
4. Pairing and link keys (NetBSD's `bthcid`/`btpin`/`btdevctl` live in user
   space): a small OSyx service or kernel helper answering PIN/SSP and
   creating `bthidev` devices for paired mice and keyboards.

QEMU has no Bluetooth device, so blocks 2-4 can only be verified on hardware.
