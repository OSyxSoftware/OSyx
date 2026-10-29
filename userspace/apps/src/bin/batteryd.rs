//! batteryd: battery and AC adapter state from ACPI, for the taskbar.
//!
//! Loads the DSDT and SSDTs into the AML interpreter (`third_party/aml`) and
//! evaluates the firmware's own methods: `_BST` and `_BIF`/`_BIX` of every
//! battery device (`PNP0C0A`), `_PSR` of the AC adapter (`ACPI0003`). Their
//! fields usually live in the embedded controller, which is driven through
//! its two I/O ports. Every five seconds the state goes to
//! `osyx_abi::BATTERY_STATUS_PATH` as `percent charging ac`, or `none`.
//!
//! The interpreter runs here, not in the kernel: firmware byte code this
//! interpreter does not handle ends this process, never the system.
#![no_std]
#![no_main]

use aml::value::Args;
use aml::{AmlContext, AmlName, AmlValue, DebugVerbosity, Handler, LevelType};
use os_std::prelude::*;
use os_std::time::Duration;
use os_std::{fs, thread};
use osyx_abi::firmware as op;

os_std::entry!(main);

fn fw(code: u64, a: u64, b: u64, c: u64, d: u64) -> u64 {
    os_sys::firmware(code, a, b, c, d).unwrap_or(u64::MAX)
}

/// The embedded controller's command/status and data ports (ACPI 12.2).
#[derive(Clone, Copy)]
struct Ec {
    command: u16,
    data: u16,
}

const EC_OBF: u64 = 1;
const EC_IBF: u64 = 2;
const EC_READ: u64 = 0x80;
const EC_WRITE: u64 = 0x81;

impl Ec {
    fn status(&self) -> u64 {
        fw(op::PORT, self.command as u64, 1, 0, 0)
    }

    /// Waits until `(status & mask == 0) == clear`; false after a timeout.
    fn wait(&self, mask: u64, clear: bool) -> bool {
        for _ in 0..20_000 {
            let s = self.status();
            if s != u64::MAX && (s & mask == 0) == clear {
                return true;
            }
        }
        false
    }

    fn out(&self, port: u16, value: u64) {
        fw(op::PORT, port as u64, 1, value, 1);
    }

    fn read(&self, addr: u8) -> Option<u8> {
        if !self.wait(EC_IBF, true) {
            return None;
        }
        self.out(self.command, EC_READ);
        if !self.wait(EC_IBF, true) {
            return None;
        }
        self.out(self.data, addr as u64);
        if !self.wait(EC_OBF, false) {
            return None;
        }
        Some(fw(op::PORT, self.data as u64, 1, 0, 0) as u8)
    }

    fn write(&self, addr: u8, value: u8) {
        if self.wait(EC_IBF, true) {
            self.out(self.command, EC_WRITE);
            if self.wait(EC_IBF, true) {
                self.out(self.data, addr as u64);
                if self.wait(EC_IBF, true) {
                    self.out(self.data, value as u64);
                }
            }
        }
    }
}

/// The AML interpreter's view of the hardware, through `nr::FIRMWARE`.
struct Firmware {
    ec: Option<Ec>,
}

fn pci_address(bus: u8, device: u8, function: u8) -> u64 {
    (bus as u64) << 16 | (device as u64) << 8 | function as u64
}

impl Handler for Firmware {
    fn read_u8(&self, address: usize) -> u8 {
        fw(op::PHYS, address as u64, 1, 0, 0) as u8
    }
    fn read_u16(&self, address: usize) -> u16 {
        fw(op::PHYS, address as u64, 2, 0, 0) as u16
    }
    fn read_u32(&self, address: usize) -> u32 {
        fw(op::PHYS, address as u64, 4, 0, 0) as u32
    }
    fn read_u64(&self, address: usize) -> u64 {
        fw(op::PHYS, address as u64, 8, 0, 0)
    }
    fn write_u8(&mut self, address: usize, value: u8) {
        fw(op::PHYS, address as u64, 1, value as u64, 1);
    }
    fn write_u16(&mut self, address: usize, value: u16) {
        fw(op::PHYS, address as u64, 2, value as u64, 1);
    }
    fn write_u32(&mut self, address: usize, value: u32) {
        fw(op::PHYS, address as u64, 4, value as u64, 1);
    }
    fn write_u64(&mut self, address: usize, value: u64) {
        fw(op::PHYS, address as u64, 8, value, 1);
    }
    fn read_io_u8(&self, port: u16) -> u8 {
        fw(op::PORT, port as u64, 1, 0, 0) as u8
    }
    fn read_io_u16(&self, port: u16) -> u16 {
        fw(op::PORT, port as u64, 2, 0, 0) as u16
    }
    fn read_io_u32(&self, port: u16) -> u32 {
        fw(op::PORT, port as u64, 4, 0, 0) as u32
    }
    fn write_io_u8(&self, port: u16, value: u8) {
        fw(op::PORT, port as u64, 1, value as u64, 1);
    }
    fn write_io_u16(&self, port: u16, value: u16) {
        fw(op::PORT, port as u64, 2, value as u64, 1);
    }
    fn write_io_u32(&self, port: u16, value: u32) {
        fw(op::PORT, port as u64, 4, value as u64, 1);
    }
    fn read_pci_u8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u8 {
        if segment != 0 { return 0xff; }
        fw(op::PCI, pci_address(bus, device, function), offset as u64, 1, 0) as u8
    }
    fn read_pci_u16(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u16 {
        if segment != 0 { return 0xffff; }
        fw(op::PCI, pci_address(bus, device, function), offset as u64, 2, 0) as u16
    }
    fn read_pci_u32(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16) -> u32 {
        if segment != 0 { return u32::MAX; }
        fw(op::PCI, pci_address(bus, device, function), offset as u64, 4, 0) as u32
    }
    fn write_pci_u8(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16, value: u8) {
        if segment == 0 { fw(op::PCI, pci_address(bus, device, function), offset as u64, 0x101, value as u64); }
    }
    fn write_pci_u16(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16, value: u16) {
        if segment == 0 { fw(op::PCI, pci_address(bus, device, function), offset as u64, 0x102, value as u64); }
    }
    fn write_pci_u32(&self, segment: u16, bus: u8, device: u8, function: u8, offset: u16, value: u32) {
        if segment == 0 { fw(op::PCI, pci_address(bus, device, function), offset as u64, 0x104, value as u64); }
    }
    fn read_ec_u8(&self, offset: u8) -> u8 {
        self.ec.and_then(|ec| ec.read(offset)).unwrap_or(0)
    }
    fn write_ec_u8(&self, offset: u8, value: u8) {
        if let Some(ec) = self.ec {
            ec.write(offset, value);
        }
    }
}

/// The `n`-th ACPI table with signature `sig`.
fn table(sig: &[u8; 4], n: u64) -> Option<Vec<u8>> {
    let sig = u32::from_le_bytes(*sig) as u64;
    let len = os_sys::firmware(op::TABLE, sig, n, 0, 0).ok()? as usize;
    let mut buf = vec![0u8; len];
    os_sys::firmware(op::TABLE, sig, n, buf.as_mut_ptr() as u64, len as u64).ok()?;
    Some(buf)
}

/// The EC ports from the ECDT (control at byte 44, data at 56, as generic
/// addresses whose address is 4 bytes in), else the conventional 0x66/0x62.
fn ec_ports() -> Ec {
    let gas = |t: &[u8], at: usize| t.get(at + 4..at + 6).map(|b| u16::from_le_bytes([b[0], b[1]]));
    match table(b"ECDT", 0) {
        Some(t) => match (gas(&t, 44), gas(&t, 56)) {
            (Some(command), Some(data)) if command != 0 && data != 0 => Ec { command, data },
            _ => Ec { command: 0x66, data: 0x62 },
        },
        None => Ec { command: 0x66, data: 0x62 },
    }
}

/// An `_HID` as text: EISA IDs are compressed into integers.
fn hid_text(v: &AmlValue, ctx: &AmlContext) -> Option<String> {
    match v {
        AmlValue::String(s) => Some(s.clone()),
        _ => {
            let id = v.as_integer(ctx).ok()? as u32;
            let id = id.swap_bytes();
            let letter = |shift: u32| (((id >> shift) & 0x1f) as u8 + b'@') as char;
            Some(format!("{}{}{}{:04X}", letter(26), letter(21), letter(16), id & 0xffff))
        }
    }
}

fn path(base: &AmlName, child: &str) -> Option<AmlName> {
    AmlName::from_str(&format!("{}.{}", base.as_string(), child)).ok()
}

fn call(ctx: &mut AmlContext, base: &AmlName, method: &str, args: Args) -> Option<AmlValue> {
    ctx.invoke_method(&path(base, method)?, args).ok()
}

fn integer(ctx: &AmlContext, v: &AmlValue) -> Option<u64> {
    v.as_integer(ctx).ok()
}

/// Devices by `_HID`.
fn devices(ctx: &mut AmlContext, wanted: &[&str]) -> Vec<(String, AmlName)> {
    let mut all = Vec::new();
    let _ = ctx.namespace.traverse(|name, level| {
        if level.typ == LevelType::Device {
            all.push(name.clone());
        }
        Ok(true)
    });
    let mut found = Vec::new();
    for name in all {
        let Some(hid) = call(ctx, &name, "_HID", Args::EMPTY) else { continue };
        if let Some(text) = hid_text(&hid, ctx) {
            if wanted.contains(&text.as_str()) {
                found.push((text, name));
            }
        }
    }
    found
}

fn present(ctx: &mut AmlContext, dev: &AmlName) -> bool {
    // Without _STA a device is present; bit 4 means a battery is inserted.
    match call(ctx, dev, "_STA", Args::EMPTY) {
        Some(v) => integer(ctx, &v).is_some_and(|s| s & 0x10 != 0),
        None => true,
    }
}

fn package(v: Option<AmlValue>) -> Option<Vec<AmlValue>> {
    match v? {
        AmlValue::Package(items) => Some(items),
        _ => None,
    }
}

const UNKNOWN: u64 = 0xffff_ffff;

/// (remaining, full, charging, discharging) of one battery.
fn battery(ctx: &mut AmlContext, dev: &AmlName) -> Option<(u64, u64, bool, bool)> {
    if !present(ctx, dev) {
        return None;
    }
    // _BIX starts with a revision, so its capacities are one slot later.
    let full = match package(call(ctx, dev, "_BIX", Args::EMPTY)) {
        Some(p) if p.len() > 3 => (integer(ctx, &p[3]), integer(ctx, &p[2])),
        _ => {
            let p = package(call(ctx, dev, "_BIF", Args::EMPTY))?;
            (p.get(2).and_then(|v| integer(ctx, v)), p.get(1).and_then(|v| integer(ctx, v)))
        }
    };
    let full = match full {
        (Some(last), _) if last != 0 && last != UNKNOWN => last,
        (_, Some(design)) if design != 0 && design != UNKNOWN => design,
        _ => return None,
    };
    let st = package(call(ctx, dev, "_BST", Args::EMPTY))?;
    let state = st.first().and_then(|v| integer(ctx, v))?;
    let remaining = st.get(2).and_then(|v| integer(ctx, v)).filter(|&r| r != UNKNOWN)?;
    Some((remaining.min(full), full, state & 2 != 0, state & 1 != 0))
}

fn write_status(text: &str) {
    let tmp = "/run/battery.tmp";
    if fs::write(tmp, text).is_ok() {
        let _ = fs::rename(tmp, osyx_abi::BATTERY_STATUS_PATH);
    }
}

fn main() -> i32 {
    let ec = ec_ports();
    let mut ctx = AmlContext::new(Box::new(Firmware { ec: Some(ec) }), DebugVerbosity::None);
    let Some(dsdt) = table(b"DSDT", 0) else {
        eprintln!("batteryd: no DSDT");
        write_status("none\n");
        return 0;
    };
    // Each table's AML starts after its 36-byte header.
    if let Err(e) = ctx.parse_table(&dsdt[36.min(dsdt.len())..]) {
        eprintln!("batteryd: DSDT: {e:?}");
    }
    for n in 0.. {
        let Some(ssdt) = table(b"SSDT", n) else { break };
        if let Err(e) = ctx.parse_table(&ssdt[36.min(ssdt.len())..]) {
            eprintln!("batteryd: SSDT {n}: {e:?}");
        }
    }
    if let Err(e) = ctx.initialize_objects() {
        eprintln!("batteryd: initialising objects: {e:?}");
    }
    // Tell the firmware its embedded controller region may be used
    // (_REG(EmbeddedControl, connected)); battery methods check this.
    for (_, ec_dev) in devices(&mut ctx, &["PNP0C09"]) {
        if let Ok(args) = Args::from_list(vec![AmlValue::Integer(3), AmlValue::Integer(1)]) {
            let _ = call(&mut ctx, &ec_dev, "_REG", args);
        }
    }
    let batteries: Vec<AmlName> = devices(&mut ctx, &["PNP0C0A"]).into_iter().map(|(_, n)| n).collect();
    let adapters: Vec<AmlName> = devices(&mut ctx, &["ACPI0003"]).into_iter().map(|(_, n)| n).collect();
    println!("batteryd: {} battery device(s), {} AC adapter(s)", batteries.len(), adapters.len());
    if batteries.is_empty() {
        write_status("none\n");
        return 0;
    }
    let mut last = String::new();
    loop {
        let (mut remaining, mut full, mut charging) = (0, 0, false);
        for b in &batteries {
            if let Some((r, f, c, _)) = battery(&mut ctx, b) {
                remaining += r;
                full += f;
                charging |= c;
            }
        }
        let mut ac = false;
        for a in &adapters {
            if let Some(v) = call(&mut ctx, a, "_PSR", Args::EMPTY) {
                ac |= integer(&ctx, &v) == Some(1);
            }
        }
        let text = if full == 0 {
            String::from("none\n")
        } else {
            format!("{} {} {}\n", (remaining * 100 / full).min(100), charging as u8, ac as u8)
        };
        if text != last {
            println!("batteryd: {}", text.trim_end());
            write_status(&text);
            last = text;
        }
        thread::sleep(Duration::from_secs(5));
    }
}
