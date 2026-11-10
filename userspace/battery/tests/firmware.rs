//! The battery stack against firmware tables compiled by the reference ASL
//! compiler (`iasl`), written in the style of real laptop firmware. The
//! hand-assembled tables of `acpi.rs` test single operators; these test the
//! shape of whole vendor implementations.
//!
//! The `.aml` files are checked in beside their `.asl` sources; rebuild one
//! with `iasl -tc <name>.asl` after changing it.

use osyx_abi::battery::State;
use osyx_battery::model::Monitor;
use osyx_battery::{Acpi, EcPorts, Hardware};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// An embedded controller whose registers 0xA0-0xAF show the page selected
/// by register `page_reg` (the ThinkPad "HIID" scheme) when it has one.
#[derive(Default)]
struct EcState {
    regs: Vec<u8>,
    page_reg: Option<u8>,
    pages: HashMap<u8, [u8; 16]>,
    phase: u8,
    addr: u8,
    out: Option<u8>,
    reads: u64,
    slept_ms: u64,
    /// Physical memory the firmware maps (a memory-mapped EC).
    mem: HashMap<u64, u8>,
    /// CMOS behind an index/data port pair at 0x72/0x73.
    cmos: Vec<u8>,
    cmos_index: u8,
    /// Accesses the machine refused because they were not aligned.
    unaligned: u64,
}

impl EcState {
    fn read(&mut self, a: u8) -> u8 {
        self.reads += 1;
        if let (Some(pr), 0xA0..=0xAF) = (self.page_reg, a) {
            let page = self.regs[pr as usize];
            return self.pages.get(&page).map_or(0, |p| p[(a - 0xA0) as usize]);
        }
        self.regs[a as usize]
    }
}

#[derive(Clone)]
struct Machine {
    ec: Arc<Mutex<EcState>>,
    clock: Arc<AtomicU64>,
}

impl Machine {
    fn new(page_reg: Option<u8>) -> Machine {
        let ec = EcState { regs: vec![0; 256], cmos: vec![0; 256], page_reg, ..EcState::default() };
        Machine { ec: Arc::new(Mutex::new(ec)), clock: Arc::new(AtomicU64::new(0)) }
    }

    fn set(&self, addr: usize, bytes: &[u8]) {
        self.ec.lock().unwrap().regs[addr..addr + bytes.len()].copy_from_slice(bytes);
    }

    /// Sets 16-bit words of a page, starting at word `first`.
    fn page_words(&self, page: u8, first: usize, words: &[u16]) {
        let mut ec = self.ec.lock().unwrap();
        let p = ec.pages.entry(page).or_insert([0; 16]);
        for (i, w) in words.iter().enumerate() {
            p[(first + i) * 2..(first + i) * 2 + 2].copy_from_slice(&w.to_le_bytes());
        }
    }

    fn mem(&self, addr: u64, bytes: &[u8]) {
        let mut ec = self.ec.lock().unwrap();
        for (i, b) in bytes.iter().enumerate() {
            ec.mem.insert(addr + i as u64, *b);
        }
    }

    fn mem_words(&self, addr: u64, words: &[u16]) {
        for (i, w) in words.iter().enumerate() {
            self.mem(addr + 2 * i as u64, &w.to_le_bytes());
        }
    }

    fn cmos(&self, index: usize, bytes: &[u8]) {
        self.ec.lock().unwrap().cmos[index..index + bytes.len()].copy_from_slice(bytes);
    }

    fn page_bytes(&self, page: u8, bytes: &[u8]) {
        let mut ec = self.ec.lock().unwrap();
        let p = ec.pages.entry(page).or_insert([0; 16]);
        p[..bytes.len()].copy_from_slice(bytes);
    }
}

impl Hardware for Machine {
    fn port_in(&self, port: u16, _width: u8) -> u64 {
        let mut ec = self.ec.lock().unwrap();
        match port {
            0x66 => u64::from(ec.out.is_some()),
            0x62 => ec.out.take().map_or(0xFF, |v| v as u64),
            0x72 => ec.cmos_index as u64,
            0x73 => ec.cmos[ec.cmos_index as usize] as u64,
            _ => u64::MAX,
        }
    }

    fn port_out(&self, port: u16, _width: u8, value: u64) {
        let mut ec = self.ec.lock().unwrap();
        match (port, ec.phase) {
            (0x66, _) => {
                ec.phase = match value {
                    0x80 => 1,
                    0x81 => 2,
                    _ => 0,
                }
            }
            (0x62, 1) => {
                let v = ec.read(value as u8);
                ec.out = Some(v);
                ec.phase = 0;
            }
            (0x62, 2) => {
                ec.addr = value as u8;
                ec.phase = 3;
            }
            (0x62, 3) => {
                let a = ec.addr as usize;
                ec.regs[a] = value as u8;
                ec.phase = 0;
            }
            (0x72, _) => ec.cmos_index = value as u8,
            (0x73, _) => {
                let i = ec.cmos_index as usize;
                ec.cmos[i] = value as u8;
            }
            _ => {}
        }
    }

    /// Like the kernel: only accesses aligned to their width.
    fn phys_read(&self, address: u64, width: u8) -> u64 {
        let mut ec = self.ec.lock().unwrap();
        if !address.is_multiple_of(width as u64) {
            ec.unaligned += 1;
            return u64::MAX;
        }
        (0..width as u64).fold(0, |v, i| v | (ec.mem.get(&(address + i)).copied().unwrap_or(0) as u64) << (8 * i))
    }
    fn phys_write(&self, address: u64, width: u8, value: u64) {
        let mut ec = self.ec.lock().unwrap();
        if !address.is_multiple_of(width as u64) {
            ec.unaligned += 1;
            return;
        }
        for i in 0..width as u64 {
            ec.mem.insert(address + i, (value >> (8 * i)) as u8);
        }
    }
    fn pci_read(&self, _: u64, _: u64, _: u8) -> u64 {
        u64::MAX
    }
    fn pci_write(&self, _: u64, _: u64, _: u8, _: u64) {}

    fn now_ms(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed)
    }

    fn sleep_ms(&self, ms: u64) {
        self.ec.lock().unwrap().slept_ms += ms;
    }
}

/// Loads `tables`, collecting what the loader complained about.
fn load(m: &Machine, ec: Option<EcPorts>, tables: &[&[u8]]) -> (Acpi, Vec<String>) {
    let tables: Vec<Vec<u8>> = tables.iter().map(|t| t.to_vec()).collect();
    let mut problems = Vec::new();
    let acpi = Acpi::load(m.clone(), ec, &tables, &mut |p| problems.push(p.to_string()));
    (acpi, problems)
}

// ---- ThinkPad style ---------------------------------------------------------------------------

const THINKPAD: &[u8] = include_bytes!("fixtures/thinkpad.aml");

/// A 50 Wh battery at half charge, discharging 1 A at 11.4 V.
fn thinkpad_machine() -> Machine {
    let m = Machine::new(Some(0x81));
    m.set(0x38, &[0x80 | 0x40 | 0x03]); // present, discharging, level 3
    m.set(0x46, &[0x00]); // no AC
    // Page 0: remaining, full, -, -, current, voltage.
    m.page_words(0, 0, &[2250, 4500, 0, 0, 0u16.wrapping_sub(1000), 11_400]);
    // Page 1: mode (bit 15: capacities in 10 mWh), -, cycle count.
    m.page_words(1, 0, &[0x8000, 0, 123]);
    // Page 2: design capacity, design voltage, -, -, -, serial number.
    m.page_words(2, 0, &[5000, 11_520, 0, 0, 0, 4321]);
    m.page_bytes(4, b"LION");
    m.page_bytes(5, b"OSYX CELLS\0");
    m.page_bytes(6, b"45N1234\0");
    m
}

#[test]
fn thinkpad_style_firmware_reports_the_battery() {
    let m = thinkpad_machine();
    let (mut acpi, problems) = load(&m, None, &[THINKPAD]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((acpi.battery_devices(), acpi.adapter_devices(), acpi.ec_devices()), (1, 1, 1));
    let mut monitor = Monitor::new();
    let poll = acpi.poll();
    assert_eq!(poll.batteries.len(), 1, "battery present: {poll:?}");
    let sample = poll.batteries[0].expect("the firmware's _BIX and _BST ran");
    assert_eq!(sample.last_full_capacity, Some(45_000));
    assert_eq!(sample.design_capacity, Some(50_000));
    assert_eq!(sample.remaining, Some(22_500));
    assert_eq!(sample.rate, Some(11_400));
    assert_eq!(sample.voltage, Some(11_400));
    assert!(sample.discharging && !sample.charging);
    let s = monitor.update(&poll);
    assert_eq!((s.state, s.percent, s.ac), (State::Discharging, 50, false));
    assert_eq!(s.minutes_to_empty, Some(22_500 * 60 / 11_400));
}

#[test]
fn thinkpad_style_battery_identity() {
    let m = thinkpad_machine();
    let (mut acpi, _) = load(&m, None, &[THINKPAD]);
    assert!(acpi.identities().is_empty(), "nothing is known before the first poll");
    acpi.poll();
    let ids = acpi.identities();
    assert_eq!(ids.len(), 1);
    let id = &ids[0];
    assert_eq!(
        (id.device.as_str(), id.model.as_str(), id.serial.as_str(), id.technology.as_str(), id.vendor.as_str(), id.cycles),
        ("BAT0", "45N1234", "4321", "LION", "OSYX CELLS", Some(123))
    );
    assert!(acpi.take_problems().is_empty());
}

#[test]
fn thinkpad_style_firmware_keeps_working_poll_after_poll() {
    // The info method creates named objects (`Name (SERN, ...)`) each time
    // it runs; the second run must not collide with the first.
    let m = thinkpad_machine();
    let (mut acpi, _) = load(&m, None, &[THINKPAD]);
    let mut monitor = Monitor::new();
    for round in 0..60 {
        let poll = acpi.poll();
        assert!(poll.batteries.first().is_some_and(Option::is_some), "round {round}: {poll:?}");
        let s = monitor.update(&poll);
        assert_eq!(s.percent, 50, "round {round}");
    }
    // Plug in: charging 2 A.
    m.set(0x38, &[0x80 | 0x20 | 0x03]);
    m.set(0x46, &[0x10]);
    m.page_words(0, 4, &[2000]);
    let s = monitor.update(&acpi.poll());
    assert_eq!((s.state, s.ac), (State::Charging, true));
    assert_eq!(s.rate_mw, Some(11_400 * 2));
}

#[test]
fn thinkpad_style_battery_removed_and_inserted() {
    let m = thinkpad_machine();
    let (mut acpi, _) = load(&m, None, &[THINKPAD]);
    let mut monitor = Monitor::new();
    assert_eq!(monitor.update(&acpi.poll()).state, State::Discharging);
    m.set(0x38, &[0x00]);
    m.set(0x46, &[0x10]);
    assert_eq!(monitor.update(&acpi.poll()).state, State::None);
    m.set(0x38, &[0x80 | 0x20 | 0x03]);
    assert_eq!(monitor.update(&acpi.poll()).state, State::Charging);
}


// ---- two more firmware families in one table ------------------------------------------------------

const LAPTOP: &[u8] = include_bytes!("fixtures/laptop.aml");
/// Where the table maps the embedded controller's RAM.
const ERAM: u64 = 0xFE0B_0300;

/// BAT0 (memory-mapped, mAh): 4400 mAh design, 4000 full, 3000 left at
/// 11.1 V design, discharging 1.5 A. BAT1 (EC, mAh): 4000 design, 3600
/// full, 1800 left at 14.8 V, charging 2 A; AC present.
fn laptop_machine() -> Machine {
    let m = Machine::new(None);
    m.mem_words(ERAM + 0xB1, &[4400, 4000, 3000, 11_100, 12_000, 0xFFFFu16 - 1500, 0x0000]);
    m.mem(ERAM + 0xC1, &[0b1000_0101]); // present, discharging; AC adapter present
    m.mem_words(ERAM + 0x93, &[45]); // 45 degrees Celsius
    m.set(0x10, &[0b10]); // battery slot 1 has valid data
    m.set(0x21, &[0b101]); // BAT1 present, charging
    let words = [4000u16, 3600, 1800, 14_800, 15_000, 2000, 54_321, (2026 - 1980) << 9 | 11 << 5 | 10];
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    m.set(0x22, &bytes);
    m.set(0x40, b"OSYX-B1\0");
    m.cmos(0x42, &[0x21]); // lid flag set
    m
}

fn evaluate(acpi: &mut Acpi, path: &str) -> String {
    acpi.evaluate(path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn laptop_table_loads_completely() {
    let m = laptop_machine();
    let (acpi, problems) = load(&m, None, &[LAPTOP]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((acpi.battery_devices(), acpi.adapter_devices(), acpi.ec_devices()), (2, 1, 1));
}

#[test]
fn battery_with_firmware_side_unit_conversion_and_memory_mapped_ec() {
    let m = laptop_machine();
    let (mut acpi, _) = load(&m, None, &[LAPTOP]);
    let bif = evaluate(&mut acpi, "\\_SB.PCI0.BAT0._BIF");
    // mAh * design voltage / 1000: 4400 -> 48840 mWh, 4000 -> 44400 mWh.
    assert!(bif.starts_with("Package([Integer(0), Integer(48840), Integer(44400), Integer(1), Integer(11100)"), "{bif}");
    let bst = evaluate(&mut acpi, "\\_SB.PCI0.BAT0._BST");
    // AC is present but the cell is not charging: state 0, 1500 mA and
    // 3000 mAh at 11.1 V, present voltage 12 V.
    assert_eq!(bst, "Package([Integer(0), Integer(16650), Integer(33300), Integer(12000)])");
    assert!(m.ec.lock().unwrap().unaligned == 0, "odd addresses are accessed byte by byte");
}

#[test]
fn battery_in_a_package_of_packages_with_built_strings() {
    let m = laptop_machine();
    let (mut acpi, _) = load(&m, None, &[LAPTOP]);
    let bif = evaluate(&mut acpi, "\\_SB.PCI0.LPCB.EC0.BAT1._BIF");
    assert!(
        bif.starts_with("Package([Integer(1), Integer(4000), Integer(3600), Integer(1), Integer(14800), Integer(360), Integer(180)"),
        "{bif}"
    );
    assert!(bif.contains("String(\"OSYX-B1\")"), "model from a 128-bit EC field: {bif}");
    assert!(bif.contains("String(\"54321 10/11/2026\")"), "serial and date: {bif}");
    let bst = evaluate(&mut acpi, "\\_SB.PCI0.LPCB.EC0.BAT1._BST");
    assert_eq!(bst, "Package([Integer(2), Integer(2000), Integer(1800), Integer(15000)])");
}

#[test]
fn laptop_table_through_the_monitor() {
    let m = laptop_machine();
    let (mut acpi, _) = load(&m, None, &[LAPTOP]);
    let poll = acpi.poll();
    assert_eq!(poll.batteries.len(), 2, "{poll:?}");
    assert!(poll.batteries.iter().all(Option::is_some), "{poll:?}");
    assert_eq!(poll.ac, Some(true));
    let s = Monitor::new().update(&poll);
    assert_eq!(s.batteries, 2);
    // 33.3 Wh of 44.4 Wh and 1800 of 3600 mAh at 14.8 V (26.64 of 53.28 Wh).
    assert_eq!(s.percent, 61);
    assert!(s.ac);
    assert_ne!(s.state, State::Unknown);
}

#[test]
fn adapter_lid_and_thermal_zone_methods() {
    let m = laptop_machine();
    let (mut acpi, _) = load(&m, None, &[LAPTOP]);
    assert_eq!(evaluate(&mut acpi, "\\_SB.PCI0.AC0._PSR"), "Integer(1)");
    // An IndexField: CMOS register 0x42, low nibble.
    assert_eq!(evaluate(&mut acpi, "\\_SB.PCI0.LID0._LID"), "Integer(1)");
    // 45 C in tenths of a kelvin.
    assert_eq!(evaluate(&mut acpi, "\\_TZ.THM0._TMP"), "Integer(3182)");
    m.mem(ERAM + 0xC1, &[0b0000_0101]);
    assert_eq!(evaluate(&mut acpi, "\\_SB.PCI0.AC0._PSR"), "Integer(0)");

    // The same through the service's own interface.
    assert_eq!(acpi.thermal_zones(), 1);
    let temps = acpi.temperatures();
    assert_eq!(temps.len(), 1);
    assert_eq!((temps[0].zone.as_str(), temps[0].decicelsius), ("THM0", 450));
    assert_eq!(acpi.lid_open(), Some(true));
    m.cmos(0x42, &[0x20]);
    assert_eq!(acpi.lid_open(), Some(false));
    // A sensor that reads nonsense (an EC not ready: 0 K) is left out.
    m.mem_words(ERAM + 0x93, &[0xFFFF]);
    assert!(acpi.temperatures().is_empty());
    assert!(acpi.take_problems().is_empty());
}

// ---- a secondary table ------------------------------------------------------------------------------

const SSDT: &[u8] = include_bytes!("fixtures/ssdt.aml");

#[test]
fn secondary_table_adds_to_the_first_without_losses() {
    let m = laptop_machine();
    let (mut acpi, problems) = load(&m, None, &[LAPTOP, SSDT]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((acpi.battery_devices(), acpi.adapter_devices()), (2, 2));
    // The adapter the SSDT adds calls into the DSDT's embedded controller.
    assert_eq!(evaluate(&mut acpi, "\\_SB.PCI0.LPCB.EC0.ADP1._PSR"), "Integer(1)");
    // The scope of a device that does not exist was skipped by its `If`.
    assert!(acpi.evaluate("\\_SB.PCI0.NOPE.GONE").is_err());
    assert!(acpi.evaluate("\\NOEC").is_err());
    assert_eq!(evaluate(&mut acpi, "\\CFG2"), "Integer(1)");
}

#[test]
fn secondary_table_alone_loses_only_what_depends_on_the_first() {
    // Without the DSDT its `External` devices do not exist: the table
    // still loads, and the `Else` branch runs.
    let m = laptop_machine();
    let (mut acpi, problems) = load(&m, None, &[SSDT]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((acpi.battery_devices(), acpi.adapter_devices()), (0, 0));
    assert_eq!(evaluate(&mut acpi, "\\NOEC"), "Integer(1)");
}

// ---- operator conformance ---------------------------------------------------------------------------

const OPERATORS: &[u8] = include_bytes!("fixtures/operators.aml");

/// Every `Tnnn` method of `operators.asl` returns one, or the number of the
/// check inside it that the interpreter got wrong.
#[test]
fn operators_compute_what_the_specification_says() {
    let m = Machine::new(None);
    let (mut acpi, problems) = load(&m, None, &[OPERATORS]);
    assert!(problems.is_empty(), "{problems:?}");
    let mut failures = Vec::new();
    for n in 1..=9 {
        let path = format!("\\T{n:03}");
        // Twice: a method that leaves objects behind fails the second time.
        for run in 1..=2 {
            match acpi.evaluate(&path) {
                Ok(v) if v == "Integer(1)" => {}
                other => failures.push(format!("{path} (run {run}): {other:?}")),
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

// ---- the table QEMU is given ------------------------------------------------------------------------

/// `scripts/acpi/qemu-battery.aml`: what `-acpitable` gives QEMU so the
/// desktop can be looked at with a battery.
const QEMU_BATTERY: &[u8] = include_bytes!("../../../scripts/acpi/qemu-battery.aml");

#[test]
fn qemu_battery_table() {
    let m = Machine::new(None);
    let (mut acpi, problems) = load(&m, None, &[QEMU_BATTERY]);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!((acpi.battery_devices(), acpi.adapter_devices(), acpi.thermal_zones()), (1, 1, 1));
    let mut monitor = Monitor::new();
    let s = monitor.update(&acpi.poll());
    // The mock clock is at a few milliseconds: the first step of the cycle.
    assert_eq!((s.state, s.percent, s.ac), (State::Discharging, 76, false));
    assert_eq!((s.capacity_mwh, s.design_mwh, s.rate_mw), (Some(45_000), Some(50_000), Some(12_000)));
    assert_eq!(s.health_percent(), Some(90));
    let ids = acpi.identities();
    assert_eq!(
        (ids[0].model.as_str(), ids[0].serial.as_str(), ids[0].technology.as_str(), ids[0].vendor.as_str(), ids[0].cycles),
        ("OSYX-QEMU-50", "03001", "LION", "OSyx", Some(87))
    );
    assert_eq!(acpi.lid_open(), Some(true));
    assert_eq!(acpi.temperatures()[0].decicelsius / 10, 45);
    // Half a cycle later (71 steps of 9 s) it charges from 5 %.
    m.clock.store(71 * 9 * 1000 + 500, Ordering::Relaxed);
    let s = monitor.update(&acpi.poll());
    assert_eq!((s.state, s.percent, s.ac), (State::Charging, 5, true));
    assert!(acpi.take_problems().is_empty());
}
