//! OSyx Settings (`/system/bin/settings`): a sidebar of categories and a
//! panel of grouped options, in the style of a modern desktop settings
//! app. Every option writes real system configuration:
//!
//! * Appearance: colour scheme (`/etc/osyx/theme`: dark, light or by the
//!   sun), accent colour (`/etc/osyx/accent`, read by every OSyx
//!   application when it starts) and the desktop background
//!   (`/etc/osyx/wallpaper`, applied by the desktop shell within seconds);
//! * Keyboard: layout (`/etc/osyx/keyboard`, followed by PRISM);
//! * Mouse & Touchpad: pointer speed, natural scrolling, left-handed
//!   buttons (`/etc/osyx/pointer`, followed by PRISM);
//! * Sound: output volume and mute (the kernel mixer), the default output
//!   and input card (the mixer, and `/etc/osyx/audio`, which the desktop
//!   shell applies when it starts);
//! * Power: the battery as `batteryd` reads it from the firmware, its
//!   identity, the AC adapter, the lid and the thermal zones; whether the
//!   task bar shows the percentage and when it warns (`/etc/osyx/power`);
//! * Date & Time: time zone (`/etc/TZ`, a POSIX TZ string) and how the
//!   task bar shows the time (`/etc/osyx/clock`);
//! * Network: DHCP or a static IPv4 address for each interface (the
//!   kernel's network configuration, `net_config`);
//! * General: host name (`/etc/hostname`);
//! * Display, Storage and About show the live system state.
//!
//! Keyboard, Mouse, Sound, Power and Date & Time are built from one model
//! (groups of rows: information, switches, choices, sliders, meters) that
//! lays itself out and scrolls; the older sections draw themselves.
//!
//! Settings runs as root (it changes system files); it does not drop to
//! the desktop user like the other applications.

#![no_std]
#![no_main]

#[path = "../shell_common.rs"]
mod common;

use common::{human_size, installed_programs, run_app, App, Ctx, InstalledProgram};
use os_std::prelude::*;
use os_std::process::Command;
use os_std::{fs, os, system};
use osyx_gfx::{Color, Rect};
use osyx_abi::battery::{Info as BatteryInfo, State as BatteryState, Status as Battery};
use osyx_input::layout::Layout;
use osyx_ui::options::{ClockOptions, PowerOptions};
use osyx_ui::time::ZONES;
use osyx_ui::theme::SchemeChoice;
use osyx_ui::widget::{Button, ButtonStyle, TextField};
use osyx_ui::{icon, Align, Event as Ui, FontKind, Icon, Painter, Response};
use prism_core::PointerSettings;
use prism_client::WindowSpec;

os_std::entry!(main);

const SIDEBAR: u32 = 240;
/// Programs listed at once in the Applications section.
const PROGRAM_ROWS: usize = 8;
/// Written by the update daemon (root) and read here.
const UPDATE_STATUS: &str = "/run/update.status";
/// Where Settings asks the daemon (`osyx-update --daemon`, root) for a check
/// (`check`) or an installation (`install`); the daemon removes it.
const UPDATE_REQUEST: &str = "/etc/osyx/update.request";

/// State of the system update service: (state, version, detail).
fn update_state() -> (String, String, String) {
    let text = fs::read_to_string(UPDATE_STATUS).unwrap_or_default();
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).map(|v| String::from(v.trim()))).unwrap_or_default();
    (field("state "), field("version "), field("detail "))
}

/// What to tell the user about the update service.
fn update_text() -> String {
    if fs::metadata("/etc/osyx/installed").is_err() {
        return String::from("Updates are only available on an installed system");
    }
    if fs::metadata("/etc/osyx/update.conf").is_err() {
        return String::from("Updates are not configured (no /etc/osyx/update.conf)");
    }
    let (state, version, detail) = update_state();
    match state.as_str() {
        "checking" => String::from("Checking for updates..."),
        "up-to-date" => String::from("OSyx is up to date"),
        "available" => format!("Version {version} is available"),
        "downloading" => format!("Downloading {version} {detail}"),
        "installing" => format!("Installing {version}..."),
        "installed" => format!("{version} is installed: restart to finish"),
        "failed" => format!("Failed: {detail}"),
        _ => String::from("Not checked yet (the service checks once a day)"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    General,
    Appearance,
    Display,
    Keyboard,
    Mouse,
    Sound,
    Power,
    DateTime,
    Network,
    Storage,
    Apps,
    About,
}

const SECTIONS: [(Section, &str, Icon); 12] = [
    (Section::General, "General", Icon::Program),
    (Section::Appearance, "Appearance", Icon::Apps),
    (Section::Display, "Displays", Icon::Device),
    (Section::Keyboard, "Keyboard", Icon::Device),
    (Section::Mouse, "Mouse & Touchpad", Icon::Device),
    (Section::Sound, "Sound", Icon::Volume),
    (Section::Power, "Power", Icon::Power),
    (Section::DateTime, "Date & Time", Icon::Bell),
    (Section::Network, "Network", Icon::Link),
    (Section::Storage, "Storage", Icon::Drive),
    (Section::Apps, "Applications", Icon::Apps),
    (Section::About, "About", Icon::Info),
];

pub const ACCENTS: [(&str, Color); 8] = [
    ("Blue", osyx_gfx::rgb(76, 141, 255)),
    ("Purple", osyx_gfx::rgb(160, 110, 240)),
    ("Pink", osyx_gfx::rgb(236, 92, 160)),
    ("Red", osyx_gfx::rgb(240, 84, 84)),
    ("Orange", osyx_gfx::rgb(245, 150, 60)),
    ("Yellow", osyx_gfx::rgb(240, 200, 60)),
    ("Green", osyx_gfx::rgb(80, 196, 120)),
    ("Graphite", osyx_gfx::rgb(140, 146, 160)),
];

const WALLPAPERS: [&str; 4] = ["OSyx", "Deep blue", "Aurora", "Graphite"];
/// The pointer speeds the slider snaps to (percent of the device's motion).
const SPEED_STEP: u32 = 25;

// ---- the model of the newer sections ------------------------------------------------------------

/// What a click on a row does.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Act {
    Layout(Layout),
    Zone(usize),
    TwelveHour(bool),
    ClockSeconds,
    ClockDate,
    NaturalScroll,
    LeftHanded,
    Mute,
    Output(usize),
    Input(usize),
    ShowPercent,
    LowAt(u8),
}

/// What a slider sets.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Slide {
    Volume,
    PointerSpeed,
}

enum Row {
    /// A fact: label and value.
    Info(String, String),
    /// A switch.
    Toggle(Act, String, bool),
    /// One of a set of choices.
    Radio(Act, String, bool),
    /// A value from `min` to `max`, with its text.
    Slider(Slide, String, u32, u32, u32, String),
    /// A level: label, text, parts per thousand, colour.
    Meter(String, String, u32, Color),
    /// A remark under the rows.
    Note(String),
}

impl Row {
    fn height(&self) -> u32 {
        match self {
            Row::Slider(..) | Row::Meter(..) => 52,
            Row::Note(_) => 34,
            _ => 40,
        }
    }
}

struct Group {
    title: String,
    rows: Vec<Row>,
}

impl Group {
    fn new(title: &str, rows: Vec<Row>) -> Group {
        Group { title: String::from(title), rows }
    }

    /// Title, card padding and rows.
    fn height(&self) -> u32 {
        26 + 8 + self.rows.iter().map(Row::height).sum::<u32>()
    }
}

/// Space between two groups.
const GROUP_GAP: u32 = 18;

/// Where the groups and their rows are, scrolled by `scroll` pixels:
/// (card of each group, rows of each group), and the height of it all.
fn place(view: Rect, scroll: i32, groups: &[Group]) -> (Vec<(Rect, Vec<Rect>)>, u32) {
    let mut y = view.y - scroll;
    let mut placed = Vec::with_capacity(groups.len());
    for g in groups {
        let card = Rect::new(view.x, y + 26, view.w, g.height() - 26);
        let mut row_y = card.y + 4;
        let rows = g
            .rows
            .iter()
            .map(|r| {
                let rect = Rect::new(card.x + 6, row_y, card.w.saturating_sub(12), r.height());
                row_y += r.height() as i32;
                rect
            })
            .collect();
        placed.push((card, rows));
        y += (g.height() + GROUP_GAP) as i32;
    }
    (placed, (y + scroll - view.y).max(0) as u32)
}

/// The track of a slider row.
fn slider_track(row: Rect) -> Rect {
    let x = row.x + (row.w as i32 * 38 / 100).max(170);
    Rect::new(x, row.y + 22, (row.x + row.w as i32 - 96 - x).max(60) as u32, 8)
}

/// The value a pointer at `x` picks on a slider from `min` to `max`.
fn slider_value(row: Rect, x: i32, min: u32, max: u32) -> u32 {
    let t = slider_track(row);
    let at = (x - t.x).clamp(0, t.w as i32) as u64;
    min + ((max - min) as u64 * at / t.w.max(1) as u64) as u32
}

/// Output volume and mute from the kernel's `/run/audio.status`.
fn mixer_state() -> Option<(u32, bool)> {
    let status = fs::read_to_string("/run/audio.status").ok()?;
    let field = |k: &str| status.lines().find_map(|l| l.strip_prefix(k).map(str::trim));
    field("device=")?;
    Some((field("volume=").and_then(|v| v.parse().ok()).unwrap_or(0), field("mute=") == Some("1")))
}

fn set_mixer(volume: u32, muted: bool) -> Result<(), os_std::error::Error> {
    use os_std::io::Write;
    fs::OpenOptions::new()
        .write(true)
        .open("/dev/mixer")
        .and_then(|mut f| f.write_all(format!("volume={}\nmute={}\n", volume.min(100), muted as u8).as_bytes()))
}

/// `45.0 Wh` from milliwatt hours; `12.0 W` and `11.5 V` likewise.
fn milli(value: u32, unit: &str) -> String {
    format!("{}.{} {unit}", value / 1000, value % 1000 / 100)
}

struct Settings {
    section: Section,
    size: (u32, u32),
    accent: usize,
    /// The colour scheme: dark, light, or by the sun.
    scheme: SchemeChoice,
    wallpaper: usize,
    /// Pixels the model sections are scrolled down.
    scroll: i32,
    /// The slider being dragged.
    drag: Option<Slide>,
    hostname: TextField,
    save_host: Button,
    dhcp: Button,
    static_btn: Button,
    addr: TextField,
    gateway: TextField,
    apply_net: Button,
    check_update: Button,
    install_update: Button,
    /// Programs with an uninstaller, the selected row and the button.
    programs: Vec<InstalledProgram>,
    selected_program: Option<usize>,
    /// First program row shown.
    programs_top: usize,
    uninstall: Button,
    status: String,
    focus: usize,
}

const SCHEMES: [SchemeChoice; 3] = [SchemeChoice::Dark, SchemeChoice::Light, SchemeChoice::Automatic];

/// The button of the `i`th colour scheme in Appearance.
fn scheme_button(c: Rect, i: usize) -> Rect {
    let w = ((c.w as i32 - 40 - 2 * 10) / 3).max(60);
    Rect::new(c.x + 20 + i as i32 * (w + 10), c.y + 202, w as u32, 34)
}

fn read(path: &str) -> String {
    fs::read_to_string(path).map(|s| String::from(s.trim())).unwrap_or_default()
}

/// The file as it is (`key=value` files keep their lines).
fn read_raw(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn write(path: &str, value: &str) -> String {
    let _ = fs::create_dir("/etc/osyx");
    match fs::write(path, format!("{value}\n").as_bytes()) {
        Ok(()) => {
            println!("settings: saved {path}={value}");
            format!("Saved {path}")
        },
        Err(e) => format!("Cannot write {path}: {e}"),
    }
}

/// Writes `contents` to `path`; `done` on success, the reason otherwise.
fn save(path: &str, contents: &str, done: &str) -> String {
    let _ = fs::create_dir("/etc/osyx");
    match fs::write(path, contents.as_bytes()) {
        Ok(()) => {
            println!("settings: saved {path}={}", contents.trim_end().replace('\n', " "));
            String::from(done)
        }
        Err(e) => format!("Cannot write {path}: {e}"),
    }
}

/// The sound cards from the kernel's `/run/audio.status`: the playback
/// ones (`audioN=card` lines), the recording ones (`audioinN=card`), and
/// the default card of each direction (`device=`, `input=`).
struct Cards {
    outputs: Vec<String>,
    inputs: Vec<String>,
    output: String,
    input: String,
}

impl Cards {
    fn read() -> Cards {
        let status = read("/run/audio.status");
        let mut cards = Cards { outputs: Vec::new(), inputs: Vec::new(), output: String::new(), input: String::new() };
        for line in status.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let v = String::from(v.trim());
            match k {
                "device" => cards.output = v,
                "input" => cards.input = v,
                _ if k.strip_prefix("audioin").is_some_and(|n| n.parse::<u32>().is_ok()) => cards.inputs.push(v),
                _ if k.strip_prefix("audio").is_some_and(|n| n.parse::<u32>().is_ok()) => cards.outputs.push(v),
                _ => {}
            }
        }
        cards
    }
}

/// Makes `card` the default output or input (`key` "output" or "input"):
/// now through the kernel mixer, and at the next start through
/// `/etc/osyx/audio`, which keeps one line per direction.
fn choose_card(key: &str, card: &str) -> String {
    let line = format!("{key}={card}\n");
    if let Err(e) = fs::OpenOptions::new().write(true).open("/dev/mixer").and_then(|mut f| {
        use os_std::io::Write;
        f.write_all(line.as_bytes())
    }) {
        return format!("Cannot switch to {card}: {e}");
    }
    let prefix = format!("{key}=");
    let mut saved: Vec<String> = read("/etc/osyx/audio")
        .lines()
        .filter(|l| !l.starts_with(prefix.as_str()) && !l.trim().is_empty())
        .map(String::from)
        .collect();
    saved.push(format!("{key}={card}"));
    let _ = fs::create_dir("/etc/osyx");
    match fs::write("/etc/osyx/audio", format!("{}\n", saved.join("\n")).as_bytes()) {
        Ok(()) => format!("{card} is now the default {key}"),
        Err(e) => format!("Switched to {card}, but cannot save /etc/osyx/audio: {e}"),
    }
}

/// Clickable option rows: (rect, index).
fn option_rows(area: Rect, n: usize, top: i32) -> Vec<Rect> {
    (0..n).map(|i| Rect::new(area.x + 20, top + i as i32 * 40, area.w.saturating_sub(40), 36)).collect()
}

fn inside(r: Rect, x: i32, y: i32) -> bool {
    x >= r.x && y >= r.y && x < r.x + r.w as i32 && y < r.y + r.h as i32
}

impl Settings {
    fn configure_network(&self, dhcp: bool) -> os_std::error::Result<()> {
        use os_std::error::{Error, ErrorKind};
        use os_std::net::{self, Ipv4Addr, NetConfig};
        let bad = |s| Error::new(ErrorKind::InvalidInput, s);
        let iface = Self::net_iface().ok_or(bad("no network adapter"))?;
        let mut config = NetConfig { iface: iface.index as u32, method: net::CONFIG_DHCP as u32, ..Default::default() };
        if !dhcp {
            let (ip, prefix) = self.addr.text().split_once('/').ok_or(bad("use address/prefix"))?;
            config.method = net::CONFIG_STATIC as u32;
            config.addr = Ipv4Addr::parse(ip).ok_or(bad("invalid IPv4 address"))?.0;
            config.prefix = prefix.parse().ok().filter(|&p| p <= 32).ok_or(bad("invalid prefix"))?;
            config.gateway = Ipv4Addr::parse(self.gateway.text()).ok_or(bad("invalid gateway"))?.0;
        }
        net::configure(&config)
    }
    /// Keeps the list inside its rows and the selected program in view, so
    /// Uninstall never acts on a program the user cannot see.
    fn show_selected_program(&mut self) {
        self.programs_top = self.programs_top.min(self.programs.len().saturating_sub(PROGRAM_ROWS));
        if let Some(i) = self.selected_program {
            if i < self.programs_top {
                self.programs_top = i;
            } else if i >= self.programs_top + PROGRAM_ROWS {
                self.programs_top = i + 1 - PROGRAM_ROWS;
            }
        }
    }

    fn content(&self) -> Rect {
        let (w, h) = self.size;
        Rect::new(SIDEBAR as i32 + 16, 16, w.saturating_sub(SIDEBAR + 32), h.saturating_sub(32))
    }

    fn group(p: &mut Painter, r: Rect, title: &str) -> Rect {
        let t = *p.theme;
        p.text(FontKind::Bold, r.x, r.y, title, t.text);
        let card = Rect::new(r.x, r.y + 26, r.w, r.h.saturating_sub(26));
        p.card(card, 10, t.raised, t.border);
        card
    }

    fn row(p: &mut Painter, r: Rect, label: &str, value: &str) {
        let t = *p.theme;
        p.text(FontKind::Ui, r.x + 16, r.y + 10, label, t.text);
        p.text_in(FontKind::Ui, Rect::new(r.x, r.y + 10, r.w.saturating_sub(16), 20), value, t.text_dim, Align::Right);
    }

    fn choices(p: &mut Painter, rows: &[Rect], labels: &[&str], selected: usize) {
        let t = *p.theme;
        for (i, (r, l)) in rows.iter().zip(labels).enumerate() {
            if i == selected {
                p.rounded(*r, 6, t.selection, 255);
            }
            p.ring(r.x + 18, r.y + 18, 7, 2, if i == selected { t.accent } else { t.text_faint }, 255, false);
            if i == selected {
                p.disc(r.x + 18, r.y + 18, 4, t.accent, 255);
            }
            p.text(FontKind::Ui, r.x + 36, r.y + 9, l, t.text);
        }
    }

    fn net_iface() -> Option<osyx_abi::net::NetIface> {
        os_std::net::preferred_ethernet(&os_std::net::interfaces().ok()?)
    }

    /// The area the model sections scroll in: under the title, above the
    /// status line.
    fn view(&self) -> Rect {
        let c = self.content();
        Rect::new(c.x, c.y + 44, c.w, c.h.saturating_sub(44 + 30))
    }

    /// The groups of the current section, from the system's state now;
    /// `None` for the sections that draw themselves.
    fn model(&self) -> Option<Vec<Group>> {
        Some(match self.section {
            Section::Keyboard => {
                let current = Layout::from_name(&read("/etc/osyx/keyboard")).unwrap_or(Layout::Us);
                let layouts = Layout::ALL.iter().map(|l| Row::Radio(Act::Layout(*l), String::from(l.label()), *l == current)).collect();
                vec![
                    Group::new("Input source", layouts),
                    Group::new(
                        "Typing",
                        vec![
                            Row::Info(String::from("Right Alt"), String::from("AltGr: third character of a key")),
                            Row::Info(String::from("Key repeat"), String::from("after 500 ms, 30 per second")),
                            Row::Note(String::from("The layout applies within a few seconds, in every application.")),
                        ],
                    ),
                ]
            }
            Section::Mouse => {
                let s = PointerSettings::parse(&read_raw("/etc/osyx/pointer"));
                vec![
                    Group::new(
                        "Pointer",
                        vec![
                            Row::Slider(
                                Slide::PointerSpeed,
                                String::from("Pointer speed"),
                                s.speed,
                                PointerSettings::MIN_SPEED,
                                PointerSettings::MAX_SPEED,
                                format!("{} %", s.speed),
                            ),
                            Row::Toggle(Act::LeftHanded, String::from("Left-handed: swap the primary and secondary button"), s.left_handed),
                        ],
                    ),
                    Group::new(
                        "Scrolling",
                        vec![
                            Row::Toggle(Act::NaturalScroll, String::from("Natural scrolling: content follows the fingers"), s.natural_scroll),
                            Row::Note(String::from("These options apply within a few seconds, to mice and touchpads.")),
                            Row::Note(String::from("Tablets and touch screens report positions: the speed does not change them.")),
                        ],
                    ),
                ]
            }
            Section::Sound => {
                let cards = Cards::read();
                let mut groups = Vec::new();
                groups.push(match mixer_state() {
                    Some((volume, muted)) => Group::new(
                        "Volume",
                        vec![
                            Row::Slider(
                                Slide::Volume,
                                String::from("Output volume"),
                                volume.min(100),
                                0,
                                100,
                                if muted { String::from("muted") } else { format!("{} %", volume.min(100)) },
                            ),
                            Row::Toggle(Act::Mute, String::from("Mute"), muted),
                        ],
                    ),
                    None => Group::new("Volume", vec![Row::Note(String::from("No sound card is in use."))]),
                });
                let list = |title: &str, names: &[String], current: &str, none: &str, act: fn(usize) -> Act| {
                    let rows = if names.is_empty() {
                        vec![Row::Note(String::from(none))]
                    } else {
                        names.iter().enumerate().map(|(i, n)| Row::Radio(act(i), n.clone(), n == current)).collect()
                    };
                    Group::new(title, rows)
                };
                groups.push(list("Output", &cards.outputs, &cards.output, "No sound card can play", Act::Output));
                groups.push(list("Input", &cards.inputs, &cards.input, "No sound card can record", Act::Input));
                groups
            }
            Section::Power => self.power_model(),
            Section::DateTime => {
                let zone = read("/etc/TZ");
                let o = ClockOptions::parse(&read_raw(ClockOptions::PATH));
                vec![
                    Group::new(
                        "Clock in the task bar",
                        vec![
                            Row::Radio(Act::TwelveHour(false), String::from("24-hour (14:05)"), !o.twelve_hour),
                            Row::Radio(Act::TwelveHour(true), String::from("12-hour (2:05 PM)"), o.twelve_hour),
                            Row::Toggle(Act::ClockSeconds, String::from("Show seconds"), o.seconds),
                            Row::Toggle(Act::ClockDate, String::from("Show the date"), o.date),
                        ],
                    ),
                    Group::new(
                        "Time zone",
                        ZONES.iter().enumerate().map(|(i, (z, name))| Row::Radio(Act::Zone(i), String::from(*name), *z == zone)).collect(),
                    ),
                ]
            }
            _ => return None,
        })
    }

    /// Power: what `batteryd` publishes, and the options of the task bar.
    fn power_model(&self) -> Vec<Group> {
        let options = PowerOptions::parse(&read_raw(PowerOptions::PATH));
        let battery = fs::read_to_string(osyx_abi::BATTERY_STATUS_PATH).ok().and_then(|t| Battery::parse(&t));
        let mut groups = Vec::new();
        match battery.filter(Battery::present) {
            Some(b) => {
                let unknown = b.state == BatteryState::Unknown && b.percent == 0;
                let mut status = String::from(match b.state {
                    BatteryState::Charging => "Charging",
                    BatteryState::Discharging => "On battery",
                    BatteryState::Full => "Fully charged",
                    BatteryState::Idle => "Plugged in, not charging",
                    _ => "The firmware does not answer",
                });
                if let Some(t) = b.time_text() {
                    status.push_str(&match b.state {
                        BatteryState::Charging => format!(", {t} until full"),
                        _ => format!(", {t} remaining"),
                    });
                }
                let colour = if b.charging() {
                    osyx_gfx::rgb(80, 200, 120)
                } else if b.percent <= options.low.max(5) {
                    osyx_gfx::rgb(232, 72, 72)
                } else {
                    osyx_gfx::rgb(76, 141, 255)
                };
                let mut rows = vec![
                    Row::Meter(
                        String::from("Charge"),
                        if unknown { String::from("unknown") } else { format!("{} %", b.percent) },
                        b.percent as u32 * 10,
                        colour,
                    ),
                    Row::Info(String::from("Status"), status),
                ];
                if let Some(mw) = b.rate_mw.filter(|&r| r > 0) {
                    rows.push(Row::Info(String::from(if b.charging() { "Charging power" } else { "Power draw" }), milli(mw, "W")));
                }
                if let (Some(e), Some(c)) = (b.energy_mwh, b.capacity_mwh) {
                    rows.push(Row::Info(String::from("Energy"), format!("{} of {}", milli(e, "Wh"), milli(c, "Wh"))));
                }
                if let (Some(h), Some(d)) = (b.health_percent(), b.design_mwh) {
                    rows.push(Row::Info(String::from("Health"), format!("{h} % of the design capacity ({})", milli(d, "Wh"))));
                }
                if let Some(mv) = b.voltage_mv {
                    rows.push(Row::Info(String::from("Voltage"), format!("{}.{:02} V", mv / 1000, mv % 1000 / 10)));
                }
                if b.batteries > 1 {
                    rows.push(Row::Info(String::from("Batteries"), format!("{}", b.batteries)));
                }
                if b.stale {
                    rows.push(Row::Note(String::from("The firmware stopped answering: these are the last values it gave.")));
                }
                if unknown {
                    rows.push(Row::Note(String::from(
                        "A battery is present but its firmware methods could not be run; the system log (batteryd) names the method.",
                    )));
                }
                groups.push(Group::new("Battery", rows));
                for info in BatteryInfo::parse(&read_raw(osyx_abi::BATTERY_INFO_PATH)) {
                    let mut rows = Vec::new();
                    for (label, value) in
                        [("Model", &info.model), ("Manufacturer", &info.vendor), ("Chemistry", &info.technology), ("Serial number", &info.serial)]
                    {
                        if !value.is_empty() {
                            rows.push(Row::Info(String::from(label), value.clone()));
                        }
                    }
                    if let Some(cycles) = info.cycles {
                        rows.push(Row::Info(String::from("Charge cycles"), format!("{cycles}")));
                    }
                    if !rows.is_empty() {
                        groups.push(Group::new(&format!("About {}", info.device), rows));
                    }
                }
                groups.push(Group::new(
                    "Task bar",
                    vec![Row::Toggle(Act::ShowPercent, String::from("Show the percentage beside the battery"), options.show_percent)],
                ));
                groups.push(Group::new(
                    "Low battery warning",
                    PowerOptions::LOW_CHOICES
                        .iter()
                        .map(|&low| {
                            let label = if low == 0 { String::from("Never") } else { format!("At {low} %") };
                            Row::Radio(Act::LowAt(low), label, options.low == low)
                        })
                        .collect(),
                ));
            }
            None => groups.push(Group::new(
                "Battery",
                vec![Row::Note(String::from(if battery.is_some() {
                    "This computer has no battery."
                } else {
                    "The battery service has not reported yet."
                }))],
            )),
        }
        let mut machine = Vec::new();
        if let Some(b) = battery.filter(Battery::present) {
            machine.push(Row::Info(String::from("Power adapter"), String::from(if b.ac { "Connected" } else { "Not connected" })));
        }
        match read(osyx_abi::LID_STATUS_PATH).as_str() {
            "open" => machine.push(Row::Info(String::from("Lid"), String::from("Open"))),
            "closed" => machine.push(Row::Info(String::from("Lid"), String::from("Closed"))),
            _ => {}
        }
        for (zone, tenths) in osyx_abi::battery::parse_thermal(&read_raw(osyx_abi::THERMAL_STATUS_PATH)) {
            let sign = if tenths < 0 { "-" } else { "" };
            machine.push(Row::Info(
                format!("Temperature ({zone})"),
                format!("{sign}{}.{} °C", tenths.unsigned_abs() / 10, tenths.unsigned_abs() % 10),
            ));
        }
        if !machine.is_empty() {
            groups.push(Group::new("This computer", machine));
        }
        groups
    }

    /// Pointer and wheel on a model section. `None`: not for the model.
    fn model_event(&mut self, ev: &Ui, groups: &[Group]) -> Option<bool> {
        let view = self.view();
        let (placed, total) = place(view, self.scroll, groups);
        let max_scroll = (total as i32 - view.h as i32).max(0);
        let slide_at = |x: i32, y: i32, any_row: bool| {
            placed.iter().zip(groups).flat_map(|((_, rects), g)| rects.iter().zip(&g.rows)).find_map(|(r, row)| match row {
                Row::Slider(slide, _, _, min, max, _) if any_row || inside(*r, x, y) => Some((*slide, *r, *min, *max)),
                _ => None,
            })
        };
        match *ev {
            Ui::Wheel { x, dy, .. } if x >= SIDEBAR as i32 => {
                let before = self.scroll;
                self.scroll = (self.scroll - dy * 48).clamp(0, max_scroll);
                Some(self.scroll != before)
            }
            Ui::PointerDown { x, y, .. } if inside(view, x, y) => {
                if let Some((slide, row, min, max)) = slide_at(x, y, false) {
                    self.drag = Some(slide);
                    self.slide(slide, slider_value(row, x, min, max));
                    return Some(true);
                }
                let hit = placed.iter().zip(groups).flat_map(|((_, rects), g)| rects.iter().zip(&g.rows)).find_map(|(r, row)| match row {
                    Row::Toggle(act, ..) | Row::Radio(act, ..) if inside(*r, x, y) => Some(*act),
                    _ => None,
                });
                hit.map(|act| {
                    self.apply(act);
                    true
                })
            }
            Ui::PointerMove { x, .. } => {
                let slide = self.drag?;
                // The dragged slider follows the pointer wherever it is.
                let (_, row, min, max) = placed
                    .iter()
                    .zip(groups)
                    .flat_map(|((_, rects), g)| rects.iter().zip(&g.rows))
                    .find_map(|(r, row)| match row {
                        Row::Slider(s, _, _, min, max, _) if *s == slide => Some((*s, *r, *min, *max)),
                        _ => None,
                    })?;
                self.slide(slide, slider_value(row, x, min, max));
                Some(true)
            }
            Ui::PointerUp { .. } | Ui::PointerLeave => self.drag.take().map(|_| true),
            _ => None,
        }
    }

    /// A slider moved to `value`.
    fn slide(&mut self, slide: Slide, value: u32) {
        match slide {
            Slide::Volume => {
                self.status = match set_mixer(value, false) {
                    Ok(()) => format!("Volume {value} %"),
                    Err(e) => format!("Cannot set the volume: {e}"),
                };
            }
            Slide::PointerSpeed => {
                let mut s = PointerSettings::parse(&read_raw("/etc/osyx/pointer"));
                // Steps of 25 %: the difference between 137 % and 138 % is not felt.
                let speed = ((value + SPEED_STEP / 2) / SPEED_STEP * SPEED_STEP).clamp(PointerSettings::MIN_SPEED, PointerSettings::MAX_SPEED);
                if speed != s.speed {
                    s.speed = speed;
                    self.status = save("/etc/osyx/pointer", &s.to_text(), &format!("Pointer speed {speed} %"));
                }
            }
        }
    }

    /// A switch or a choice was clicked.
    fn apply(&mut self, act: Act) {
        let pointer = |change: &dyn Fn(&mut PointerSettings), done: &str| {
            let mut s = PointerSettings::parse(&read_raw("/etc/osyx/pointer"));
            change(&mut s);
            save("/etc/osyx/pointer", &s.to_text(), done)
        };
        let clock = |change: &dyn Fn(&mut ClockOptions)| {
            let mut o = ClockOptions::parse(&read_raw(ClockOptions::PATH));
            change(&mut o);
            save(ClockOptions::PATH, &o.to_text(), "The task bar clock follows within seconds")
        };
        let power = |change: &dyn Fn(&mut PowerOptions), done: &str| {
            let mut o = PowerOptions::parse(&read_raw(PowerOptions::PATH));
            change(&mut o);
            save(PowerOptions::PATH, &o.to_text(), done)
        };
        self.status = match act {
            Act::Layout(l) => save("/etc/osyx/keyboard", &format!("{}\n", l.name()), &format!("{}: applies within seconds", l.label())),
            Act::Zone(i) => save("/etc/TZ", &format!("{}\n", ZONES[i].0), &format!("Time zone: {}", ZONES[i].1)),
            Act::TwelveHour(on) => clock(&|o| o.twelve_hour = on),
            Act::ClockSeconds => clock(&|o| o.seconds = !o.seconds),
            Act::ClockDate => clock(&|o| o.date = !o.date),
            Act::NaturalScroll => pointer(&|s| s.natural_scroll = !s.natural_scroll, "Scrolling direction changed"),
            Act::LeftHanded => pointer(&|s| s.left_handed = !s.left_handed, "Mouse buttons swapped"),
            Act::Mute => match mixer_state() {
                Some((volume, muted)) => match set_mixer(volume, !muted) {
                    Ok(()) => String::from(if muted { "Sound on" } else { "Sound muted" }),
                    Err(e) => format!("Cannot change the mixer: {e}"),
                },
                None => String::from("No sound card is in use"),
            },
            Act::Output(i) => Cards::read().outputs.get(i).map_or(String::new(), |card| choose_card("output", card)),
            Act::Input(i) => Cards::read().inputs.get(i).map_or(String::new(), |card| choose_card("input", card)),
            Act::ShowPercent => power(&|o| o.show_percent = !o.show_percent, "The task bar follows within seconds"),
            Act::LowAt(low) => power(
                &|o| o.low = low,
                &if low == 0 { String::from("No low battery warning") } else { format!("Warning when the battery reaches {low} %") },
            ),
        };
    }

    fn draw_model(&mut self, p: &mut Painter, groups: &[Group]) {
        let t = *p.theme;
        let view = self.view();
        let (placed, total) = place(view, self.scroll, groups);
        // Content that became shorter (a battery removed) must not leave
        // the view scrolled past its end.
        let max_scroll = (total as i32 - view.h as i32).max(0);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
            return self.draw_model(p, groups);
        }
        let old = p.push_clip(view);
        for ((card, rects), g) in placed.iter().zip(groups) {
            if card.y > view.y + view.h as i32 || card.y + (card.h as i32) < view.y - 26 {
                continue;
            }
            p.text(FontKind::Bold, card.x, card.y - 26, &g.title, t.text);
            p.card(*card, 10, t.raised, t.border);
            for (r, row) in rects.iter().zip(&g.rows) {
                match row {
                    Row::Info(label, value) => {
                        p.text(FontKind::Ui, r.x + 10, r.y + 10, label, t.text);
                        let room = Rect::new(r.x + r.w as i32 / 3, r.y + 10, r.w * 2 / 3 - 10, 20);
                        p.text_in(FontKind::Ui, room, value, t.text_dim, Align::Right);
                    }
                    Row::Toggle(_, label, on) => {
                        p.text(FontKind::Ui, r.x + 10, r.y + 10, label, t.text);
                        // A switch: a pill with the knob on the side it is on.
                        let pill = Rect::new(r.x + r.w as i32 - 54, r.y + 9, 44, 22);
                        p.rounded(pill, 11, if *on { t.accent } else { t.field }, 255);
                        let knob = if *on { pill.x + 33 } else { pill.x + 11 };
                        p.disc(knob, pill.y + 11, 8, osyx_gfx::rgb(255, 255, 255), 255);
                    }
                    Row::Radio(_, label, selected) => {
                        if *selected {
                            p.rounded(Rect::new(r.x, r.y + 2, r.w, r.h - 4), 6, t.selection, 255);
                        }
                        p.ring(r.x + 20, r.y + 20, 7, 2, if *selected { t.accent } else { t.text_faint }, 255, false);
                        if *selected {
                            p.disc(r.x + 20, r.y + 20, 4, t.accent, 255);
                        }
                        p.text(FontKind::Ui, r.x + 38, r.y + 10, label, t.text);
                    }
                    Row::Slider(_, label, value, min, max, text) => {
                        p.text(FontKind::Ui, r.x + 10, r.y + 16, label, t.text);
                        let track = slider_track(*r);
                        p.rounded(track, 4, t.field, 255);
                        let filled = (track.w as u64 * (*value).saturating_sub(*min) as u64 / (*max - *min).max(1) as u64) as u32;
                        if filled > 0 {
                            p.rounded(Rect::new(track.x, track.y, filled.max(8), 8), 4, t.accent, 255);
                        }
                        p.disc(track.x + filled as i32, track.y + 4, 9, osyx_gfx::rgb(255, 255, 255), 255);
                        let room = Rect::new(track.x + track.w as i32 + 12, r.y + 16, 74, 20);
                        p.text_in(FontKind::Ui, room, text, t.text_dim, Align::Right);
                    }
                    Row::Meter(label, text, permille, colour) => {
                        p.text(FontKind::Ui, r.x + 10, r.y + 8, label, t.text);
                        p.text_in(FontKind::Bold, Rect::new(r.x, r.y + 8, r.w - 10, 20), text, t.text, Align::Right);
                        let bar = Rect::new(r.x + 10, r.y + 34, r.w.saturating_sub(20), 8);
                        p.rounded(bar, 4, t.field, 255);
                        let filled = (bar.w as u64 * (*permille).min(1000) as u64 / 1000) as u32;
                        if filled > 0 {
                            p.rounded(Rect::new(bar.x, bar.y, filled.max(8), 8), 4, *colour, 255);
                        }
                    }
                    Row::Note(text) => {
                        p.text_in(FontKind::Ui, Rect::new(r.x + 10, r.y + 8, r.w.saturating_sub(20), 20), text, t.text_faint, Align::Left);
                    }
                }
            }
        }
        p.restore_clip(old);
        // More below or above: a scroll bar at the right edge.
        if total > view.h {
            let h = (view.h as u64 * view.h as u64 / total as u64).max(24) as u32;
            let y = view.y + ((view.h - h) as i64 * self.scroll as i64 / max_scroll.max(1) as i64) as i32;
            p.rounded(Rect::new(view.x + view.w as i32 + 5, y, 4, h), 2, t.text_faint, 160);
        }
    }
}

impl App for Settings {
    fn layout(&mut self, w: u32, h: u32, _cx: &mut Ctx) {
        self.size = (w, h);
        let c = self.content();
        self.hostname.rect = Rect::new(c.x + 36, c.y + 112, c.w.saturating_sub(200).min(360), 34);
        self.save_host.rect = Rect::new(self.hostname.rect.x + self.hostname.rect.w as i32 + 12, c.y + 112, 100, 34);
        self.dhcp.rect = Rect::new(c.x + 36, c.y + 210, 150, 34);
        self.static_btn.rect = Rect::new(c.x + 196, c.y + 210, 150, 34);
        self.addr.rect = Rect::new(c.x + 160, c.y + 262, 220, 34);
        self.gateway.rect = Rect::new(c.x + 160, c.y + 306, 220, 34);
        self.apply_net.rect = Rect::new(c.x + 390, c.y + 306, 120, 34);
        self.check_update.rect = Rect::new(c.x + 16, c.y + 419, 180, 34);
        self.install_update.rect = Rect::new(c.x + 206, c.y + 419, 160, 34);
        self.uninstall.rect = Rect::new(c.x + c.w as i32 - 150, c.y + c.h as i32 - 56, 130, 34);
    }

    fn event(&mut self, ev: Ui, cx: &mut Ctx) -> bool {
        let c = self.content();
        if let Some(groups) = self.model() {
            if let Some(redraw) = self.model_event(&ev, &groups) {
                return redraw;
            }
        }
        // The wheel scrolls the program list (dy > 0 is up).
        if let (Section::Apps, Ui::Wheel { dy, .. }) = (self.section, &ev) {
            let before = self.programs_top;
            let max_top = self.programs.len().saturating_sub(PROGRAM_ROWS);
            self.programs_top = if *dy > 0 {
                before.saturating_sub(*dy as usize)
            } else {
                (before + dy.unsigned_abs() as usize).min(max_top)
            };
            // Uninstall must not act on a row the user can no longer see.
            if self.selected_program.is_some_and(|i| i < self.programs_top || i >= self.programs_top + PROGRAM_ROWS) {
                self.selected_program = None;
                return true;
            }
            return self.programs_top != before;
        }
        if let Ui::PointerDown { x, y, .. } = ev {
            if inside(self.hostname.rect, x, y) {
                self.focus = 0;
            }
            if inside(self.addr.rect, x, y) {
                self.focus = 1;
            }
            if inside(self.gateway.rect, x, y) {
                self.focus = 2;
            }
            if x < SIDEBAR as i32 {
                for (i, (s, _, _)) in SECTIONS.iter().enumerate() {
                    if inside(Rect::new(10, 64 + i as i32 * 40, SIDEBAR - 20, 36), x, y) {
                        self.section = *s;
                        self.scroll = 0;
                        self.drag = None;
                        self.status.clear();
                        if *s == Section::Apps {
                            self.programs = installed_programs();
                            self.selected_program = None;
                            self.programs_top = 0;
                        }
                        return true;
                    }
                }
                return false;
            }
            match self.section {
                Section::Appearance => {
                    for (i, accent) in ACCENTS.iter().enumerate() {
                        let r = Rect::new(c.x + 20 + i as i32 * 52, c.y + 80, 40, 40);
                        if inside(r, x, y) {
                            self.accent = i;
                            self.status =
                                write("/etc/osyx/accent", accent.0);
                            return true;
                        }
                    }
                    for (i, choice) in SCHEMES.iter().enumerate() {
                        if inside(scheme_button(c, i), x, y) {
                            self.scheme = *choice;
                            self.status = write(common::scheme::THEME_FILE, choice.name());
                            return true;
                        }
                    }
                    for (i, r) in option_rows(c, WALLPAPERS.len(), c.y + 300).into_iter().enumerate() {
                        if inside(r, x, y) {
                            self.wallpaper = i;
                            self.status = write("/etc/osyx/wallpaper", &format!("{i}"));
                            return true;
                        }
                    }
                }
                Section::Apps => {
                    for (i, r) in option_rows(c, self.programs.len().min(PROGRAM_ROWS), c.y + 74).into_iter().enumerate() {
                        if inside(r, x, y) {
                            self.selected_program = Some(self.programs_top + i);
                            return true;
                        }
                    }
                }
                _ => {}
            }
        }
        let mut redraw = false;
        match self.section {
            Section::General => {
                let typed = self.hostname.event_with_clipboard(&ev, cx.focused && self.focus == 0, &cx.fonts, &mut cx.clipboard);
                redraw |= typed.needs_redraw();
                let activated = matches!(self.save_host.event(&ev, cx.focused), Response::Activated(_));
                if activated || typed == Response::Submitted {
                    let h = self.hostname.text().trim().to_string();
                    self.status =
                        if h.is_empty() || h.len() > 63 || !h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                            String::from("A host name has 1-63 letters, digits or '-'")
                        } else {
                            write("/etc/hostname", &h)
                        };
                    redraw = true;
                }
            }
            Section::Apps => {
                let chosen = self.selected_program.and_then(|i| self.programs.get(i)).cloned();
                if let (Some(program), Response::Activated(_)) = (chosen, self.uninstall.event(&ev, cx.focused)) {
                    // The uninstaller is the program's own: run as root like Settings.
                    self.status = match Command::new("/bin/winrun").arg(&program.uninstaller).spawn() {
                        Ok(child) => format!("Started the uninstaller of {} (process {})", program.name, child.id()),
                        Err(e) => format!("Cannot start {}: {e}", program.uninstaller),
                    };
                    redraw = true;
                }
            }
            Section::About => {
                let can_ask = fs::metadata("/etc/osyx/installed").is_ok();
                let available = update_state().0 == "available";
                if matches!(self.check_update.event(&ev, cx.focused), Response::Activated(_)) {
                    self.status = if !can_ask {
                        String::from("Updates are only available on an installed system")
                    } else {
                        match fs::write(UPDATE_REQUEST, "check\n") {
                            Ok(()) => String::from("Checking for updates..."),
                            Err(e) => format!("Cannot ask the update service: {e}"),
                        }
                    };
                    redraw = true;
                }
                if available && matches!(self.install_update.event(&ev, cx.focused), Response::Activated(_)) {
                    self.status = match fs::write(UPDATE_REQUEST, "install\n") {
                        Ok(()) => String::from("Installing the update: the status above follows the download"),
                        Err(e) => format!("Cannot ask the update service: {e}"),
                    };
                    redraw = true;
                }
            }
            Section::Network => {
                redraw |= self.addr.event_with_clipboard(&ev, cx.focused && self.focus == 1, &cx.fonts, &mut cx.clipboard).needs_redraw();
                redraw |= self.gateway.event_with_clipboard(&ev, cx.focused && self.focus == 2, &cx.fonts, &mut cx.clipboard).needs_redraw();
                if matches!(self.dhcp.event(&ev, cx.focused), Response::Activated(_)) {
                    self.status = match self.configure_network(true) {
                        Ok(()) => String::from("DHCP requested"),
                        Err(e) => format!("Cannot switch to DHCP: {e}"),
                    };
                    redraw = true;
                }
                let _ = self.static_btn.event(&ev, cx.focused);
                if matches!(self.apply_net.event(&ev, cx.focused), Response::Activated(_)) {
                    self.status = match self.configure_network(false) {
                        Ok(()) => format!("Static address {} applied", self.addr.text()),
                        Err(e) => format!("Cannot apply the static address: {e}"),
                    };
                    redraw = true;
                }
            }
            _ => {}
        }
        redraw
    }

    fn tick_ms(&self) -> Option<u64> {
        Some(2000)
    }

    fn tick(&mut self, _cx: &mut Ctx) -> bool {
        if self.section == Section::Apps {
            // An uninstaller finishing removes its folder: follow the disk.
            let now = installed_programs();
            let changed = now.iter().map(|p| &p.uninstaller).ne(self.programs.iter().map(|p| &p.uninstaller));
            if changed {
                // The selection is the program, not its row: another one may
                // move into the row when this one is gone.
                let kept = self.selected_program.and_then(|i| self.programs.get(i)).map(|p| p.uninstaller.clone());
                self.programs = now;
                self.selected_program = kept.and_then(|path| self.programs.iter().position(|p| p.uninstaller == path));
                self.show_selected_program();
            }
            return changed;
        }
        // Live state: also the battery, the mixer and the clock.
        matches!(
            self.section,
            Section::Network | Section::Storage | Section::Display | Section::About | Section::Power | Section::Sound | Section::DateTime
        ) && self.drag.is_none()
    }

    fn draw(&mut self, p: &mut Painter, w: u32, h: u32, cx: &Ctx) {
        let t = cx.theme;
        p.fill(Rect::new(0, 0, w, h), t.window);
        // Sidebar.
        p.fill(Rect::new(0, 0, SIDEBAR, h), t.surface);
        p.text(FontKind::Large, 20, 18, "Settings", t.text);
        for (i, (s, label, ic)) in SECTIONS.iter().enumerate() {
            let r = Rect::new(10, 64 + i as i32 * 40, SIDEBAR - 20, 36);
            if *s == self.section {
                p.rounded(r, 7, t.accent, 255);
            }
            p.rounded(
                Rect::new(r.x + 8, r.y + 6, 24, 24),
                6,
                if *s == self.section { osyx_gfx::rgb(255, 255, 255) } else { t.accent },
                if *s == self.section { 60 } else { 255 },
            );
            icon::draw(p, *ic, Rect::new(r.x + 11, r.y + 9, 18, 18), osyx_gfx::rgb(255, 255, 255));
            p.text(FontKind::Ui, r.x + 44, r.y + 9, label, if *s == self.section { t.on_accent } else { t.text });
        }
        let c = self.content();
        let title = SECTIONS.iter().find(|(s, _, _)| *s == self.section).map_or("", |(_, l, _)| *l);
        p.text(FontKind::Large, c.x + 4, c.y, title, t.text);
        match self.section {
            Section::General => {
                let g = Self::group(p, Rect::new(c.x, c.y + 50, c.w, 120), "Host name");
                p.text(FontKind::Ui, g.x + 16, g.y + 12, "The name of this computer on the network", t.text_dim);
                self.hostname.draw(p, cx.focused && self.focus == 0);
                self.save_host.draw(p, cx.focused);
                let si = os::sysinfo().ok();
                let g2 = Self::group(p, Rect::new(c.x, c.y + 190, c.w, 26 + 3 * 40), "This computer");
                Self::row(p, Rect::new(g2.x, g2.y, g2.w, 40), "Host name", &read("/etc/hostname"));
                Self::row(
                    p,
                    Rect::new(g2.x, g2.y + 40, g2.w, 40),
                    "Memory",
                    &si.as_ref().map_or(String::new(), |i| human_size(i.total_memory)),
                );
                Self::row(
                    p,
                    Rect::new(g2.x, g2.y + 80, g2.w, 40),
                    "Up time",
                    &si.as_ref().map_or(String::new(), |i| common::human_duration(i.uptime_ns / 1_000_000_000)),
                );
            }
            Section::Appearance => {
                let g = Self::group(p, Rect::new(c.x, c.y + 40, c.w, 110), "Accent colour");
                for (i, (name, col)) in ACCENTS.iter().enumerate() {
                    let r = Rect::new(g.x + 20 + i as i32 * 52, g.y + 14, 40, 40);
                    if i == self.accent {
                        p.ring(r.x + 20, r.y + 20, 20, 2, t.text, 255, false);
                    }
                    p.disc(r.x + 20, r.y + 20, 15, *col, 255);
                    if i == self.accent {
                        p.text_in(FontKind::Ui, Rect::new(g.x, g.y + 58, g.w, 20), name, t.text_dim, Align::Center);
                    }
                }
                let _ = Self::group(p, Rect::new(c.x, c.y + 166, c.w, 84), "Theme");
                for (i, choice) in SCHEMES.iter().enumerate() {
                    let r = scheme_button(c, i);
                    let on = *choice == self.scheme;
                    p.rounded(r, 7, if on { t.accent } else { t.field }, 255);
                    let label = match choice {
                        SchemeChoice::Dark => "Dark",
                        SchemeChoice::Light => "Light",
                        SchemeChoice::Automatic => "Automatic (light by day)",
                    };
                    p.text_in(FontKind::Ui, r, label, if on { t.on_accent } else { t.text }, Align::Center);
                }
                let rows = option_rows(c, WALLPAPERS.len(), c.y + 300);
                let _ = Self::group(
                    p,
                    Rect::new(c.x, c.y + 266, c.w, 34 + WALLPAPERS.len() as u32 * 40 + 8),
                    "Desktop background",
                );
                Self::choices(p, &rows, &WALLPAPERS, self.wallpaper);
            }
            Section::Display => {
                let d = system::display_info().ok();
                let g = Self::group(p, Rect::new(c.x, c.y + 40, c.w, 26 + 5 * 40), "Built-in display");
                let rows = [
                    ("Resolution", d.map_or(String::from("none"), |d| format!("{} x {}", d.width, d.height))),
                    (
                        "Preferred mode",
                        d.filter(|d| d.native_width > 0)
                            .map_or(String::from("unknown"), |d| format!("{} x {}", d.native_width, d.native_height)),
                    ),
                    (
                        "Output",
                        d.map_or(String::new(), |d| {
                            String::from(if d.backend == system::BACKEND_VIRTIO_GPU {
                                "VirtIO-GPU 2D scanout"
                            } else {
                                "UEFI GOP framebuffer"
                            })
                        }),
                    ),
                    (
                        "Colour format",
                        d.map_or(String::new(), |d| {
                            String::from(if d.format == system::pixel::RGBX { "32-bit RGBX" } else { "32-bit BGRX" })
                        }),
                    ),
                    (
                        "Compositor",
                        d.filter(|d| d.owner != 0)
                            .map_or(String::from("kernel console"), |d| format!("PRISM (process {})", d.owner)),
                    ),
                ];
                for (i, (k, v)) in rows.iter().enumerate() {
                    Self::row(p, Rect::new(g.x, g.y + i as i32 * 40, g.w, 40), k, v);
                }
                p.text(
                    FontKind::Ui,
                    c.x + 4,
                    g.y + g.h as i32 + 16,
                    "Resolution changes need the compositor to restart; boot with display.mode=WIDTHxHEIGHT.",
                    t.text_faint,
                );
            }
            Section::Keyboard | Section::Mouse | Section::Sound | Section::Power | Section::DateTime => {
                if self.section == Section::DateTime {
                    let o = ClockOptions::parse(&read_raw(ClockOptions::PATH));
                    let now = common::system_tz().local(common::unix_now());
                    p.text_in(
                        FontKind::Large,
                        Rect::new(c.x, c.y, c.w.saturating_sub(4), 30),
                        &format!("{}  ·  {}", now.clock(o.twelve_hour, true), now.long_date()),
                        t.text_dim,
                        Align::Right,
                    );
                }
                let groups = self.model().unwrap_or_default();
                self.draw_model(p, &groups);
            }
            Section::Network => {
                let i = Self::net_iface();
                let g = Self::group(p, Rect::new(c.x, c.y + 40, c.w, 26 + 3 * 40), "Ethernet");
                let name =
                    i.as_ref().map_or(String::from("no network adapter"), |i| String::from(system::field_str(&i.name)));
                let ip = i.as_ref().map_or(String::from("-"), |i| {
                    format!("{}.{}.{}.{}/{}", i.addr[0], i.addr[1], i.addr[2], i.addr[3], i.prefix)
                });
                let method = i.as_ref().map_or("", |i| match i.method {
                    osyx_abi::net::CONFIG_DHCP => "DHCP",
                    osyx_abi::net::CONFIG_STATIC => "Static",
                    _ => "Not configured",
                });
                Self::row(p, Rect::new(g.x, g.y, g.w, 40), "Adapter", &name);
                Self::row(p, Rect::new(g.x, g.y + 40, g.w, 40), "IPv4 address", &ip);
                Self::row(p, Rect::new(g.x, g.y + 80, g.w, 40), "Configured by", method);
                p.text(FontKind::Bold, c.x, c.y + 186, "Configure IPv4", t.text);
                self.dhcp.draw(p, cx.focused);
                self.static_btn.draw(p, cx.focused);
                p.text(FontKind::Ui, c.x + 36, c.y + 270, "Address/prefix", t.text_dim);
                p.text(FontKind::Ui, c.x + 36, c.y + 314, "Gateway", t.text_dim);
                self.addr.draw(p, cx.focused && self.focus == 1);
                self.gateway.draw(p, cx.focused && self.focus == 2);
                self.apply_net.draw(p, cx.focused);
            }
            Section::Storage => {
                let mounts = system::mounts().unwrap_or_default();
                let g = Self::group(p, Rect::new(c.x, c.y + 40, c.w, 26 + mounts.len() as u32 * 56), "Volumes");
                for (k, m) in mounts.iter().enumerate() {
                    let path = system::field_str(&m.path);
                    let (total, free) = os::statfs(path)
                        .map_or((0, 0), |s| (s.total_blocks * s.block_size, s.free_blocks * s.block_size));
                    let y = g.y + 8 + k as i32 * 56;
                    p.text(
                        FontKind::Ui,
                        g.x + 16,
                        y,
                        &format!("{path}  ·  {}  ·  {}", system::field_str(&m.fstype), system::field_str(&m.source)),
                        t.text,
                    );
                    let v = if total > 0 {
                        format!("{} free of {}", human_size(free), human_size(total))
                    } else {
                        String::from("virtual")
                    };
                    p.text_in(
                        FontKind::Ui,
                        Rect::new(g.x, y, g.w.saturating_sub(16), 20),
                        &v,
                        t.text_dim,
                        Align::Right,
                    );
                    let bar = Rect::new(g.x + 16, y + 26, g.w.saturating_sub(32), 8);
                    p.rounded(bar, 4, t.field, 255);
                    if let Some(used) = total.saturating_sub(free).saturating_mul(bar.w as u64).checked_div(total) {
                        let used = used as u32;
                        if used > 0 {
                            p.rounded(Rect::new(bar.x, bar.y, used.max(8), 8), 4, t.accent, 255);
                        }
                    }
                }
            }
            Section::Apps => {
                let rows = self.programs.len().min(PROGRAM_ROWS);
                let title = if self.programs.len() > rows {
                    format!("Installed programs ({}-{} of {}, scroll for more)", self.programs_top + 1, self.programs_top + rows, self.programs.len())
                } else {
                    String::from("Installed programs")
                };
                let g = Self::group(p, Rect::new(c.x, c.y + 40, c.w, 34 + (rows.max(1) * 40) as u32 + 8), &title);
                if self.programs.is_empty() {
                    p.text(FontKind::Ui, g.x + 16, g.y + 12, "No program with an uninstaller is installed in Program Files.", t.text_dim);
                }
                for (i, r) in option_rows(c, rows, c.y + 74).into_iter().enumerate() {
                    let program = &self.programs[self.programs_top + i];
                    if self.selected_program == Some(self.programs_top + i) {
                        p.rounded(r, 6, t.selection, 255);
                    }
                    icon::draw(p, program.icon, Rect::new(r.x + 8, r.y + 4, 28, 28), t.text);
                    p.text(FontKind::Ui, r.x + 46, r.y + 9, &program.name, t.text);
                    p.text_in(FontKind::Ui, Rect::new(r.x, r.y + 9, r.w.saturating_sub(12), 20), &program.folder, t.text_dim, Align::Right);
                }
                if self.selected_program.is_some() {
                    self.uninstall.draw(p, cx.focused);
                }
            }
            Section::About => {
                let si = os::sysinfo().ok();
                p.text(FontKind::Display, c.x + 4, c.y + 40, "OSyx", t.text);
                let g = Self::group(p, Rect::new(c.x, c.y + 110, c.w, 26 + 5 * 40), "Software");
                let rows = [
                    ("Version", read("/etc/osyx-release")),
                    ("Kernel", si.as_ref().map_or(String::new(), |i| String::from(os::kernel_version(i)))),
                    ("System call ABI", si.as_ref().map_or(String::new(), |i| format!("{}", i.abi_version))),
                    ("Compositor", String::from("PRISM")),
                    (
                        "Windows programs",
                        String::from(if fs::metadata("/bin/winrun").is_ok() {
                            "winrun (PE32+, Win32 subset)"
                        } else {
                            "not installed"
                        }),
                    ),
                ];
                for (i, (k, v)) in rows.iter().enumerate() {
                    Self::row(p, Rect::new(g.x, g.y + i as i32 * 40, g.w, 40), k, v);
                }
                let u = Self::group(p, Rect::new(c.x, c.y + 350, c.w, 26 + 2 * 40), "System update");
                Self::row(p, Rect::new(u.x, u.y, u.w, 40), "Status", &update_text());
                self.check_update.draw(p, cx.focused);
                if update_state().0 == "available" {
                    self.install_update.draw(p, cx.focused);
                }
            }
        }
        if !self.status.is_empty() {
            p.text_in(
                FontKind::Ui,
                Rect::new(c.x, c.y + c.h as i32 - 22, c.w, 20),
                &self.status,
                t.text_dim,
                Align::Left,
            );
        }
    }
}

fn main() -> i32 {
    let accent = read("/etc/osyx/accent");
    let wallpaper: usize = read("/etc/osyx/wallpaper").parse().unwrap_or(0);
    let host = read("/etc/hostname");
    let mut hostname = TextField::new("");
    hostname.set_text(&host);
    // `settings <section>` opens at that section (the launcher's search).
    let section = match os_std::env::args().nth(1).as_deref() {
        Some("appearance") => Section::Appearance,
        Some("display") => Section::Display,
        Some("keyboard") => Section::Keyboard,
        Some("mouse") => Section::Mouse,
        Some("sound") => Section::Sound,
        Some("power") => Section::Power,
        Some("datetime") => Section::DateTime,
        Some("network") => Section::Network,
        Some("storage") => Section::Storage,
        Some("apps") => Section::Apps,
        Some("about") => Section::About,
        _ => Section::General,
    };
    let mut app = Settings {
        section,
        size: (0, 0),
        scheme: common::scheme::choice(),
        accent: ACCENTS.iter().position(|(n, _)| n.eq_ignore_ascii_case(&accent)).unwrap_or(0),
        wallpaper: wallpaper.min(WALLPAPERS.len() - 1),
        scroll: 0,
        drag: None,
        hostname,
        save_host: Button::new(1, "Save", ButtonStyle::Primary),
        dhcp: Button::new(2, "Use DHCP", ButtonStyle::Secondary),
        static_btn: Button::new(3, "Static address:", ButtonStyle::Flat),
        addr: TextField::new(
            &Settings::net_iface().map_or(String::new(), |i| format!("{}/{}", os_std::net::Ipv4Addr(i.addr), i.prefix)),
        ),
        gateway: TextField::new(
            &Settings::net_iface().map_or(String::from("0.0.0.0"), |i| format!("{}", os_std::net::Ipv4Addr(i.gateway))),
        ),
        apply_net: Button::new(4, "Apply", ButtonStyle::Primary),
        check_update: Button::new(5, "Check for updates", ButtonStyle::Secondary),
        install_update: Button::new(6, "Install update", ButtonStyle::Primary),
        programs: installed_programs(),
        selected_program: None,
        programs_top: 0,
        uninstall: Button::new(7, "Uninstall", ButtonStyle::Danger),
        status: String::new(),
        focus: 0,
    };
    println!("settings: started");
    let mut spec = WindowSpec::normal("Settings", 980, 700);
    spec.min_width = 780;
    spec.min_height = 580;
    run_app("settings", spec, &mut app)
}
