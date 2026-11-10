//! The firmware's own battery methods, run by the AML interpreter:
//! `_STA`, `_BIF`/`_BIX` and `_BST` of every battery device (`PNP0C0A`),
//! `_PSR` of every AC adapter (`ACPI0003`).

use crate::hw::{EcCell, EcPorts, Firmware, Hardware, Stats};
use crate::model::{field, Poll, Sample};
use alloc::collections::BTreeSet;
use aml::value::Args;
use aml::{AmlContext, AmlName, AmlValue, DebugVerbosity, LevelType};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

/// `_BIF`/`_BIX` change rarely (the last full capacity creeps down over
/// months) and each call costs many EC reads: ask again after this many polls.
const STATIC_INFO_POLLS: u32 = 24;

#[derive(Clone, Copy, Debug, Default)]
struct StaticInfo {
    amp_units: bool,
    design_capacity: Option<u32>,
    last_full_capacity: Option<u32>,
    design_voltage: Option<u32>,
}

/// What a battery says about itself (`_BIX`, or `_BIF`): text the firmware
/// keeps for it, and the cycle count `_BIX` has.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    /// The ACPI device, e.g. `BAT0`.
    pub device: String,
    pub model: String,
    pub serial: String,
    pub technology: String,
    pub vendor: String,
    pub cycles: Option<u32>,
}

/// A thermal zone's temperature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Temperature {
    /// The ACPI thermal zone, e.g. `THM0`.
    pub zone: String,
    /// Tenths of a degree Celsius.
    pub decicelsius: i32,
}

struct Battery {
    name: AmlName,
    info: Option<StaticInfo>,
    info_age: u32,
    identity: Identity,
}

pub struct Acpi {
    ctx: AmlContext,
    batteries: Vec<Battery>,
    adapters: Vec<AmlName>,
    ec_devices: Vec<AmlName>,
    stats: Arc<Stats>,
    polls: u32,
    ec_cell: Arc<EcCell>,
    ec_source: &'static str,
    zones: Vec<AmlName>,
    lids: Vec<AmlName>,
    /// Methods that failed, each reported once: see [`Acpi::take_problems`].
    reported: BTreeSet<String>,
    problems: Vec<String>,
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

/// `_HID` and `_CID` (one id or a package of them). Some firmware puts the
/// battery id in `_CID` only.
fn ids_of(ctx: &mut AmlContext, dev: &AmlName) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(v) = call(ctx, dev, "_HID", Args::EMPTY) {
        ids.extend(hid_text(&v, ctx));
    }
    match call(ctx, dev, "_CID", Args::EMPTY) {
        Some(AmlValue::Package(items)) => ids.extend(items.iter().filter_map(|v| hid_text(v, ctx))),
        Some(v) => ids.extend(hid_text(&v, ctx)),
        None => {}
    }
    ids
}

/// Whether `dev` has `child` (usually a method) in the namespace.
fn has(ctx: &AmlContext, dev: &AmlName, child: &str) -> bool {
    path(dev, child).is_some_and(|p| ctx.namespace.get_by_path(&p).is_ok())
}

fn path(base: &AmlName, child: &str) -> Option<AmlName> {
    AmlName::from_str(&format!("{}.{}", base.as_string(), child)).ok()
}

fn call(ctx: &mut AmlContext, base: &AmlName, method: &str, args: Args) -> Option<AmlValue> {
    ctx.invoke_method(&path(base, method)?, args).ok()
}

/// The last segment of a path: the device's own name.
fn short_name(name: &AmlName) -> String {
    let text = name.as_string();
    String::from(text.rsplit('.').next().unwrap_or("").trim_end_matches('_'))
}

/// Firmware text (a string, or a buffer of characters) made printable:
/// up to the first zero, ASCII only, trimmed, at most 32 characters.
fn text_of(v: Option<&AmlValue>) -> String {
    let raw: Vec<u8> = match v {
        Some(AmlValue::String(s)) => s.as_bytes().to_vec(),
        Some(AmlValue::Buffer(b)) => b.lock().clone(),
        _ => Vec::new(),
    };
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    let text: String = raw[..end].iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { ' ' }).collect();
    text.trim().chars().take(32).collect()
}

fn integer(ctx: &AmlContext, v: &AmlValue) -> Option<u64> {
    v.as_integer(ctx).ok()
}

fn package(v: Option<AmlValue>) -> Option<Vec<AmlValue>> {
    match v? {
        AmlValue::Package(items) => Some(items),
        _ => None,
    }
}

/// The EC ports an EC device's `_CRS` names.
fn crs_ports(ctx: &mut AmlContext, dev: &AmlName) -> Option<EcPorts> {
    match call(ctx, dev, "_CRS", Args::EMPTY)? {
        AmlValue::Buffer(bytes) => EcPorts::from_crs(&bytes.lock()),
        _ => None,
    }
}

fn all_devices(ctx: &mut AmlContext) -> Vec<AmlName> {
    let mut all = Vec::new();
    let _ = ctx.namespace.traverse(|name, level| {
        if level.typ == LevelType::Device {
            all.push(name.clone());
        }
        Ok(true)
    });
    all
}

/// Devices by `_HID` or `_CID`.
fn devices(ctx: &mut AmlContext, wanted: &[&str]) -> Vec<AmlName> {
    all_devices(ctx)
        .into_iter()
        .filter(|name| ids_of(ctx, name).iter().any(|id| wanted.contains(&id.as_str())))
        .collect()
}

/// Devices whose last name segment starts with one of `prefixes` (and, if
/// given, that hold `method`). Some firmware leaves `_HID` unreadable (a
/// table the interpreter lost halfway through); the name and the method
/// still tell a battery from an adapter.
fn named_devices(ctx: &mut AmlContext, prefixes: &[&str], method: Option<&str>) -> Vec<AmlName> {
    all_devices(ctx)
        .into_iter()
        .filter(|name| {
            let text = name.as_string();
            let last = text.rsplit('.').next().unwrap_or("");
            prefixes.iter().any(|p| last.starts_with(p)) && method.is_none_or(|m| has(ctx, name, m))
        })
        .collect()
}

fn merge(found: &mut Vec<AmlName>, more: Vec<AmlName>) {
    for name in more {
        if !found.contains(&name) {
            found.push(name);
        }
    }
}

impl Acpi {
    /// Loads the DSDT and SSDTs (whole tables, header included) and finds
    /// the devices. Tables that fail to parse are skipped: a battery method
    /// this interpreter cannot run costs that battery, not the daemon.
    ///
    /// `ec` are the ports the ECDT gave; with `None` the EC device's `_CRS`
    /// is asked, and the legacy ports (0x62/0x66) are the last resort. The
    /// ports are fixed before `initialize_objects`, whose `_INI` methods may
    /// already read the EC.
    pub fn load<H: Hardware + 'static>(hw: H, ec: Option<EcPorts>, tables: &[Vec<u8>], log: &mut dyn FnMut(&str)) -> Acpi {
        let stats = Arc::new(Stats::default());
        let firmware = Firmware::new(hw, ec, stats.clone());
        let ec_cell = firmware.ec_cell();
        let mut ctx = AmlContext::new(Box::new(firmware), DebugVerbosity::None);
        for (i, t) in tables.iter().enumerate() {
            let header = 36.min(t.len());
            if let Err((e, at)) = ctx.parse_table_at(&t[header..]) {
                // The objects parsed before `at` stay: a table this
                // interpreter cannot follow part-way still yields devices.
                log(&format!("table {i} ({} bytes): {e:?} at AML offset {at}", t.len()));
            }
            // What the interpreter had to skip inside the table. The
            // objects around it are loaded; say what was lost, so a
            // battery that stays unknown can be traced to its cause.
            let (skipped, more) = ctx.take_load_errors();
            for problem in &skipped {
                log(&format!(
                    "table {i}: skipped {} in {}: {:?}",
                    if problem.object_only { "an object" } else { "the rest of the scope" },
                    problem.scope.as_string(),
                    problem.error
                ));
            }
            if more > 0 {
                log(&format!("table {i}: {more} more object(s) skipped"));
            }
        }
        let mut ec_devices = devices(&mut ctx, &["PNP0C09"]);
        merge(&mut ec_devices, named_devices(&mut ctx, &["H_EC", "EC"], None));
        let ec_source = if ec.is_some() {
            "ECDT"
        } else if let Some(ports) = ec_devices.iter().find_map(|d| crs_ports(&mut ctx, d)) {
            ec_cell.set(Some(ports));
            "_CRS"
        } else {
            ec_cell.set(Some(EcPorts::LEGACY));
            "legacy ports"
        };
        if let Err(e) = ctx.initialize_objects() {
            log(&format!("initialising objects: {e:?}"));
        }
        let mut acpi = Acpi {
            ctx,
            batteries: Vec::new(),
            adapters: Vec::new(),
            ec_devices,
            stats,
            polls: 0,
            ec_cell,
            ec_source,
            zones: Vec::new(),
            lids: Vec::new(),
            reported: BTreeSet::new(),
            problems: Vec::new(),
        };
        acpi.connect_ec();
        let mut batteries = devices(&mut acpi.ctx, &["PNP0C0A"]);
        merge(&mut batteries, named_devices(&mut acpi.ctx, &["BAT", "CMB"], Some("_BST")));
        acpi.batteries = batteries
            .into_iter()
            .map(|name| {
                let identity = Identity { device: short_name(&name), ..Identity::default() };
                Battery { name, info: None, info_age: 0, identity }
            })
            .collect();
        let mut adapters = devices(&mut acpi.ctx, &["ACPI0003"]);
        merge(&mut adapters, named_devices(&mut acpi.ctx, &["AC", "ADP"], Some("_PSR")));
        acpi.adapters = adapters;
        let mut zones = Vec::new();
        let _ = acpi.ctx.namespace.traverse(|name, level| {
            if level.typ == LevelType::ThermalZone {
                zones.push(name.clone());
            }
            Ok(true)
        });
        acpi.zones = zones.into_iter().filter(|z| has(&acpi.ctx, z, "_TMP")).collect();
        acpi.lids = devices(&mut acpi.ctx, &["PNP0C0D"]).into_iter().filter(|d| has(&acpi.ctx, d, "_LID")).collect();
        acpi
    }

    /// Runs `method` of `base`. A failure is remembered once per method
    /// and error for [`Acpi::take_problems`]: why a battery stays unknown
    /// on some firmware is the interpreter's answer here.
    fn run(&mut self, base: &AmlName, method: &str) -> Option<AmlValue> {
        let path = path(base, method)?;
        match self.ctx.invoke_method(&path, Args::EMPTY) {
            Ok(v) => Some(v),
            Err(e) => {
                let text = format!("{}: {e:?}", path.as_string());
                if self.reported.len() < 64 && self.reported.insert(text.clone()) {
                    self.problems.push(text);
                }
                None
            }
        }
    }

    /// Firmware methods that failed since the last call, each with the
    /// interpreter's error; a method is listed once however often it fails.
    pub fn take_problems(&mut self) -> Vec<String> {
        core::mem::take(&mut self.problems)
    }

    /// What each present battery says about itself, after a poll.
    pub fn identities(&self) -> Vec<Identity> {
        self.batteries.iter().filter(|b| b.info.is_some()).map(|b| b.identity.clone()).collect()
    }

    pub fn thermal_zones(&self) -> usize {
        self.zones.len()
    }

    /// The thermal zones' temperatures (`_TMP`, tenths of a kelvin).
    /// Readings outside -50..150 degrees Celsius are firmware noise (an
    /// EC that is not ready reads 0 K) and are left out.
    pub fn temperatures(&mut self) -> Vec<Temperature> {
        let mut out = Vec::new();
        for zone in self.zones.clone() {
            let Some(v) = self.run(&zone, "_TMP") else { continue };
            let Some(decikelvin) = integer(&self.ctx, &v) else { continue };
            let decicelsius = decikelvin as i64 - 2732;
            if (-500..=1500).contains(&decicelsius) {
                out.push(Temperature { zone: short_name(&zone), decicelsius: decicelsius as i32 });
            }
        }
        out
    }

    /// `Some(open)` when the machine has a lid switch that answered.
    pub fn lid_open(&mut self) -> Option<bool> {
        let mut state = None;
        for lid in self.lids.clone() {
            if let Some(v) = self.run(&lid, "_LID") {
                if let Some(n) = integer(&self.ctx, &v) {
                    state = Some(state.unwrap_or(false) | (n != 0));
                }
            }
        }
        state
    }

    /// Tells the firmware its embedded controller region may be used
    /// (`_REG(EmbeddedControl, connected)`); battery methods check this.
    pub fn connect_ec(&mut self) {
        for dev in self.ec_devices.clone() {
            if let Ok(args) = Args::from_list(vec![AmlValue::Integer(3), AmlValue::Integer(1)]) {
                let _ = call(&mut self.ctx, &dev, "_REG", args);
            }
        }
    }

    /// The EC ports in use and where they came from.
    pub fn ec_ports(&self) -> (Option<EcPorts>, &'static str) {
        (self.ec_cell.get(), self.ec_source)
    }

    /// How many EC tables, devices ... the namespace holds: EC devices found.
    pub fn ec_devices(&self) -> usize {
        self.ec_devices.len()
    }

    pub fn battery_devices(&self) -> usize {
        self.batteries.len()
    }

    pub fn adapter_devices(&self) -> usize {
        self.adapters.len()
    }

    pub fn stats(&self) -> &Stats {
        &self.stats
    }

    /// Runs the object or method at `path` (no arguments) and describes
    /// the result or the interpreter's error: for diagnostics.
    pub fn evaluate(&mut self, path: &str) -> Result<String, String> {
        let name = AmlName::from_str(path).map_err(|e| format!("{e:?}"))?;
        self.ctx.invoke_method(&name, Args::EMPTY).map(|v| format!("{v:?}")).map_err(|e| format!("{e:?}"))
    }

    /// `_STA` bit 4: a battery is inserted. No `_STA` means present; a
    /// failing one counts as present, the readings then decide.
    fn present(ctx: &mut AmlContext, dev: &AmlName) -> bool {
        match call(ctx, dev, "_STA", Args::EMPTY) {
            Some(v) => integer(ctx, &v).is_none_or(|s| s & 0x10 != 0),
            None => true,
        }
    }

    fn static_info(&mut self, dev: &AmlName) -> Option<(StaticInfo, Identity)> {
        // `_BIX` starts with a revision, so its fields are one slot later:
        // unit at 1, design capacity 2, last full 3, design voltage 5,
        // cycle count 8, model 16, serial 17, type 18, OEM 19.
        // `_BIF`: unit 0, design capacity 1, last full 2, design voltage 4,
        // model 9, serial 10, type 11, OEM 12.
        let bix = if has(&self.ctx, dev, "_BIX") { package(self.run(dev, "_BIX")).filter(|p| p.len() > 5) } else { None };
        let (p, base, text, cycles) = match bix {
            Some(p) => (p, 1, 16, true),
            None => (package(self.run(dev, "_BIF"))?, 0, 9, false),
        };
        let ctx = &self.ctx;
        let get = |i: usize| p.get(base + i).and_then(|v| integer(ctx, v)).and_then(field);
        let info = StaticInfo {
            amp_units: p.get(base).and_then(|v| integer(ctx, v)) == Some(1),
            design_capacity: get(1),
            last_full_capacity: get(2),
            design_voltage: get(4),
        };
        let identity = Identity {
            device: short_name(dev),
            model: text_of(p.get(text)),
            serial: text_of(p.get(text + 1)),
            technology: text_of(p.get(text + 2)),
            vendor: text_of(p.get(text + 3)),
            cycles: if cycles { p.get(8).and_then(|v| integer(ctx, v)).and_then(field) } else { None },
        };
        Some((info, identity))
    }

    fn sample(&mut self, b: &mut Battery) -> Option<Sample> {
        if b.info.is_none() || b.info_age >= STATIC_INFO_POLLS {
            match self.static_info(&b.name) {
                Some((info, identity)) => {
                    b.info = Some(info);
                    b.identity = identity;
                    b.info_age = 0;
                }
                // Keep using the old values if the refresh failed.
                None if b.info.is_some() => {}
                None => return None,
            }
        }
        b.info_age += 1;
        let info = b.info?;
        let st = package(self.run(&b.name, "_BST"))?;
        let ctx = &self.ctx;
        let num = |i: usize| st.get(i).and_then(|v| integer(ctx, v));
        let state = num(0)?;
        Some(Sample {
            amp_units: info.amp_units,
            design_capacity: info.design_capacity,
            last_full_capacity: info.last_full_capacity,
            design_voltage: info.design_voltage,
            remaining: num(2).and_then(field),
            rate: num(1).and_then(field),
            voltage: num(3).and_then(field),
            discharging: state & 1 != 0,
            charging: state & 2 != 0,
            critical: state & 4 != 0,
        })
    }

    /// Runs the firmware's methods once.
    pub fn poll(&mut self) -> Poll {
        self.polls += 1;
        let errors_before = self.stats.errors();
        let mut out = Poll { devices: self.batteries.len(), ..Poll::default() };
        let mut batteries = core::mem::take(&mut self.batteries);
        for b in batteries.iter_mut() {
            if !Self::present(&mut self.ctx, &b.name) {
                // Removed or never inserted: its cached numbers are stale.
                b.info = None;
                continue;
            }
            out.batteries.push(self.sample(b));
        }
        self.batteries = batteries;
        for a in self.adapters.clone() {
            if let Some(v) = call(&mut self.ctx, &a, "_PSR", Args::EMPTY) {
                if let Some(n) = integer(&self.ctx, &v) {
                    out.ac = Some(out.ac.unwrap_or(false) | (n == 1));
                }
            }
        }
        out.ec_errors = self.stats.errors().wrapping_sub(errors_before);
        if out.ec_errors > 0 {
            // The EC may have lost its connection to the firmware (a
            // resume, a reset): announce it again for the next poll, and
            // do not trust the cached static numbers either.
            self.connect_ec();
            for b in self.batteries.iter_mut() {
                b.info_age = STATIC_INFO_POLLS;
            }
        }
        out
    }
}
