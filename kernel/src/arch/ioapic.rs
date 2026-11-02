//! I/O APIC routing for wired interrupts. Firmware routes stay unchanged
//! until a caller explicitly claims a GSI. The selector/window pair is
//! serialized across CPUs; route guards restore the previous masked entry.
use crate::sync::IrqSpinLock;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

struct Controller { base: u64, first: u32, count: u32, claimed: u128 }
struct State { controllers: Vec<Controller>, madt: osyx_interrupts::Madt }
static STATE: IrqSpinLock<Option<State>> = IrqSpinLock::new(None);
const MASK: u32 = 1 << 16;

// Call only while holding STATE. The mapping covers the select and data
// registers; register numbers come from the hardware's bounded pin count.
fn read(base: u64, register: u32) -> u32 {
    unsafe { write_volatile(base as *mut u32, register); read_volatile((base + 16) as *const u32) }
}
fn write(base: u64, register: u32, value: u32) {
    unsafe { write_volatile(base as *mut u32, register); write_volatile((base + 16) as *mut u32, value); }
}

pub fn init() -> bool {
    let Some(table) = crate::acpi::table_bytes(b"APIC", 0) else { return false };
    let Some(madt) = osyx_interrupts::parse_madt(table) else { return false };
    let mut state = STATE.lock();
    if state.is_some() { return true; }
    let mut controllers: Vec<Controller> = Vec::new();
    for info in &madt.controllers {
        let Ok(base) = crate::mem::map_mmio(info.address as u64, 4096) else { return false };
        let count = ((read(base, 1) >> 16) & 0xff) + 1;
        // IOREGSEL is eight bits; each redirection entry uses two registers.
        if count > 120 || info.gsi_base.checked_add(count).is_none() { return false; }
        let end = info.gsi_base + count;
        if controllers.iter().any(|c| info.gsi_base < c.first + c.count && c.first < end) { return false; }
        crate::kprintln!("ioapic: GSI {}..{} at {:#x}, {} pins", info.gsi_base, end - 1, info.address, count);
        controllers.push(Controller { base, first: info.gsi_base, count, claimed: 0 });
    }
    if controllers.is_empty() { return false; }
    *state = Some(State { controllers, madt });
    true
}

pub struct Route { gsi: u32, saved_low: u32, saved_high: u32 }

/// Claim an initially masked firmware entry. The caller owns its IRQ vector
/// until this guard has been dropped. Active firmware routes are not replaced.
/// Masking does not drain an IRQ already in flight; handlers must keep their
/// data alive until pending interrupts have been accounted for.
pub fn route(gsi: u32, vector: u8, low: bool, level: bool) -> Result<Route, &'static str> {
    let entry = osyx_interrupts::redirection(vector, super::apic::id(), low, level).ok_or("invalid vector or APIC destination")?;
    if !super::apic::available() { return Err("no local APIC"); }
    let mut state = STATE.lock();
    let state = state.as_mut().ok_or("no I/O APIC")?;
    let c = state.controllers.iter_mut().find(|c| gsi >= c.first && gsi - c.first < c.count).ok_or("GSI not covered")?;
    let pin = gsi - c.first;
    if c.claimed & (1u128 << pin) != 0 { return Err("GSI already claimed"); }
    let register = 0x10 + pin * 2;
    let saved_low = read(c.base, register);
    if saved_low & MASK == 0 { return Err("firmware route is active"); }
    let saved_high = read(c.base, register + 1);
    write(c.base, register, entry as u32);
    write(c.base, register + 1, (entry >> 32) as u32);
    write(c.base, register, entry as u32 & !MASK);
    c.claimed |= 1u128 << pin;
    Ok(Route { gsi, saved_low, saved_high })
}

pub fn route_isa(source: u8, vector: u8) -> Result<Route, &'static str> {
    let info = STATE.lock().as_ref().and_then(|s| s.madt.isa(source)).ok_or("invalid ISA source")?;
    route(info.gsi, vector, info.low, info.level)
}

impl Drop for Route {
    fn drop(&mut self) {
        let mut state = STATE.lock();
        let Some(state) = state.as_mut() else { return };
        let Some(c) = state.controllers.iter_mut().find(|c| self.gsi >= c.first && self.gsi - c.first < c.count) else { return };
        let pin = self.gsi - c.first;
        let register = 0x10 + pin * 2;
        let current = read(c.base, register);
        write(c.base, register, current | MASK);
        write(c.base, register + 1, self.saved_high);
        write(c.base, register, self.saved_low);
        c.claimed &= !(1u128 << pin);
    }
}

static TEST_IRQS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
fn test_irq(_: u64) { TEST_IRQS.fetch_add(1, core::sync::atomic::Ordering::Relaxed); }

pub fn selftest() -> Result<(), alloc::string::String> {
    use core::sync::atomic::Ordering;
    if crate::arch::timer::source_name() != "local APIC timer" { return Err("I/O APIC test requires local APIC timer".into()); }
    let vector = crate::irq::allocate(test_irq, 0).ok_or("no test vector")?;
    let route = match route_isa(0, vector) {
        Ok(route) => route,
        Err(e) => { crate::irq::free(vector); return Err(e.into()); }
    };
    let before = TEST_IRQS.load(Ordering::Relaxed);
    if route_isa(0, vector).is_ok() { drop(route); crate::irq::free(vector); return Err("duplicate route accepted".into()); }
    crate::arch::timer::sleep_ms(20);
    drop(route);
    let after = TEST_IRQS.load(Ordering::Relaxed);
    if after <= before { crate::irq::free(vector); return Err("PIT IRQ did not arrive through I/O APIC".into()); }
    // Let an in-flight edge drain before checking that the restored masked
    // entry stops delivery. The handler's data is static throughout.
    crate::arch::timer::sleep_ms(5);
    let stopped = TEST_IRQS.load(Ordering::Relaxed);
    crate::arch::timer::sleep_ms(20);
    let quiet = TEST_IRQS.load(Ordering::Relaxed) == stopped;
    crate::irq::free(vector);
    if !quiet { return Err("restored route kept delivering IRQs".into()); }
    crate::kprintln!("ioapic: PIT delivered {} wired interrupts", after - before);
    Ok(())
}
