# Wired interrupts through the I/O APIC

The kernel discovers I/O APIC controllers and ISA source overrides from the
ACPI MADT. `osyx-interrupts` validates lengths, checksums, duplicate records,
reserved flag encodings and physical-destination redirection entries on the
host. ISA conforming flags mean active-high, edge-triggered delivery.

The runtime driver serializes the IOREGSEL/IOWIN register pair, rejects
overlapping GSI ranges, and supports destinations with an eight-bit APIC ID.
Discovery does not change existing firmware routes. Callers explicitly claim
a masked GSI with `arch::ioapic::route`, or use `route_isa` to apply a MADT
override. Active firmware routes and duplicate claims are rejected. Dropping
the route guard masks and restores the old redirection entry. An already
in-flight interrupt may still arrive; the caller must retain handler state
until it has accounted for pending delivery.

`scripts/ioapic-test.ps1` builds the kernel and boots a temporary FAT tree
on two QEMU vCPUs. With `test.ioapic`, the kernel temporarily routes PIT IRQ0
through the MADT's GSI override to an allocated device vector, checks delivery
and duplicate-claim rejection, then restores the masked route and checks that
delivery stops. The tick source remains the local APIC timer. `-SkipBuild`
reuses the compiled kernel. An existing `build/esp` boot loader is required.
The minimal image lacks worker and a writable disk, so unrelated process and
disk-write tests fail; this is not the complete system suite.

The driver does not replace the timer routing, implement ACPI `_PRT` PCI INTx
lookup or integrate I2C-HID interrupt resources yet. Level-triggered delivery,
multiple I/O APICs, pending-interrupt teardown and physical hardware require
runtime validation. Host tests cover polarity/trigger encoding.

MADT formats and ISA overrides follow the
[ACPI specification](https://uefi.org/specs/ACPI/6.5/05_ACPI_Software_Programming_Model.html).
