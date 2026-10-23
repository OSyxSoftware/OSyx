//! System Information (`/system/bin/sysinfo`): live facts about the running
//! system, all read from the kernel and the file systems: OSyx and kernel
//! version, CPU (CPUID), memory, uptime, display, input devices, mounted
//! file systems with their usage, and the process list. Refreshed every
//! second.

#![no_std]
#![no_main]

#[path = "../shell_common.rs"]
mod common;

use common::{become_user, human_duration, human_size, run_app, system_tz, unix_now, App, Ctx};
use os_std::prelude::*;
use os_std::time::Instant;
use os_std::{fs, os, system};
use osyx_gfx::Rect;
use osyx_ui::layout::{inset, split_left, split_top};
use osyx_ui::time::Tz;
use osyx_ui::widget::{Column, ListItem, ListView, Panel};
use osyx_ui::{Align, Event as Ui, FontKind, Icon, Painter};
use prism_client::WindowSpec;

os_std::entry!(main);

struct Mount {
    path: String,
    fstype: String,
    source: String,
    total: u64,
    free: u64,
}

struct Info {
    release: String,
    kernel: String,
    abi: u32,
    cpu: String,
    hostname: String,
    total_mem: u64,
    free_mem: u64,
    uptime_s: u64,
    display: String,
    inputs: Vec<String>,
    mounts: Vec<Mount>,
    procs: usize,
    threads: u32,
}

struct SysInfo {
    info: Info,
    tz: Tz,
    procs: ListView,
    cards: [Panel; 4],
    size: (u32, u32),
    refreshes: u64,
}

fn cpu_brand() -> String {
    use core::arch::x86_64::__cpuid;
    // CPUID exists on every x86-64 CPU and has no side effects.
    let max_ext = __cpuid(0x8000_0000).eax;
    let vendor = __cpuid(0);
    let mut v = Vec::new();
    for r in [vendor.ebx, vendor.edx, vendor.ecx] {
        v.extend_from_slice(&r.to_le_bytes());
    }
    let vendor = String::from_utf8_lossy(&v).into_owned();
    if max_ext < 0x8000_0004 {
        return vendor;
    }
    let mut b = Vec::new();
    for leaf in 0x8000_0002u32..=0x8000_0004 {
        let r = __cpuid(leaf);
        for x in [r.eax, r.ebx, r.ecx, r.edx] {
            b.extend_from_slice(&x.to_le_bytes());
        }
    }
    let brand = String::from_utf8_lossy(&b).trim_matches(|c: char| c == '\0' || c == ' ').to_string();
    format!("{brand} ({vendor})")
}

fn gather(procs: &mut Vec<ListItem>) -> Info {
    let si = os::sysinfo().ok();
    let mut mounts = Vec::new();
    for m in system::mounts().unwrap_or_default() {
        let path = String::from(system::field_str(&m.path));
        let (total, free) = os::statfs(&path).map_or((0, 0), |s| (s.total_blocks * s.block_size, s.free_blocks * s.block_size));
        mounts.push(Mount { fstype: String::from(system::field_str(&m.fstype)), source: String::from(system::field_str(&m.source)), path, total, free });
    }
    let list = system::processes().unwrap_or_default();
    let threads = list.iter().map(|p| p.threads).sum();
    procs.clear();
    for p in &list {
        let user = if p.uid == 0 { String::from("root") } else if p.uid == 1000 { String::from("user") } else { format!("{}", p.uid) };
        let zombie = p.state == system::PROC_ZOMBIE;
        let mut item = ListItem::new(
            Some(if zombie { Icon::Close } else { Icon::Program }),
            &[&system::process_name(p), &format!("{}", p.pid), &user, &format!("{}", p.threads), &human_size(p.frames * 4096)],
        );
        item.dim = zombie;
        procs.push(item);
    }
    let display = system::display_info().map_or(String::from("none"), |d| {
        let fmt = if d.format == system::pixel::RGBX { "RGBX" } else { "BGRX" };
        format!("{}x{} framebuffer, 32-bit {fmt}, owned by process {}", d.width, d.height, d.owner)
    });
    let mut inputs: Vec<String> = fs::read_dir("/dev/input").map(|d| d.map(|e| format!("/dev/input/{}", e.file_name())).collect()).unwrap_or_default();
    inputs.sort();
    Info {
        release: fs::read_to_string("/etc/osyx-release").map(|s| String::from(s.trim())).unwrap_or_else(|_| String::from("OSyx")),
        kernel: si.as_ref().map_or(String::from("?"), |i| String::from(os::kernel_version(i))),
        abi: si.as_ref().map_or(0, |i| i.abi_version),
        cpu: cpu_brand(),
        hostname: fs::read_to_string("/etc/hostname").map(|s| String::from(s.trim())).unwrap_or_default(),
        total_mem: si.as_ref().map_or(0, |i| i.total_memory),
        free_mem: si.as_ref().map_or(0, |i| i.free_memory),
        uptime_s: Instant::now().as_nanos_since_boot() / 1_000_000_000,
        display,
        inputs,
        mounts,
        procs: list.len(),
        threads,
    }
}

fn main() -> i32 {
    become_user();
    let mut procs = ListView::new(vec![
        Column::new("Process", 0, Align::Left),
        Column::new("PID", 48, Align::Right),
        Column::new("User", 56, Align::Left),
        Column::new("Threads", 60, Align::Right),
        Column::new("Memory", 80, Align::Right),
    ]);
    procs.row_height = 28;
    let mut items = Vec::new();
    let info = gather(&mut items);
    procs.set_items(items);
    println!("sysinfo: {} | kernel {} | {} | {} processes, {} mounts", info.release, info.kernel, info.cpu, info.procs, info.mounts.len());
    let mut app = SysInfo {
        info,
        tz: system_tz(),
        procs,
        cards: [Panel::new(Some("System")), Panel::new(Some("Memory")), Panel::new(Some("File systems")), Panel::new(Some("Processes"))],
        size: (0, 0),
        refreshes: 0,
    };
    let mut spec = WindowSpec::normal("System Information", 900, 600);
    spec.min_width = 640;
    spec.min_height = 420;
    run_app("sysinfo", spec, &mut app)
}

fn bar(p: &mut Painter, r: Rect, used: u64, total: u64) {
    let t = *p.theme;
    p.rounded(r, r.h / 2, t.field, 255);
    if let (Some(w), Some(frac)) = ((r.w as u64 * used.min(total)).checked_div(total), (used * 100).checked_div(total)) {
        let w = w as u32;
        let c = if frac >= 90 { t.danger } else if frac >= 75 { t.warning } else { t.accent };
        if w > 0 {
            p.rounded(Rect::new(r.x, r.y, w.max(r.h), r.h), r.h / 2, c, 255);
        }
    }
}

impl SysInfo {
    fn rows(p: &mut Painter, r: Rect, rows: &[(&str, String)]) -> i32 {
        let t = *p.theme;
        let lh = p.fonts.line_height(FontKind::Ui) as i32 + 6;
        let mut y = r.y;
        for (k, v) in rows {
            p.text_in(FontKind::Ui, Rect::new(r.x, y, 104, lh as u32), k, t.text_dim, Align::Left);
            p.text_in(FontKind::Ui, Rect::new(r.x + 110, y, r.w.saturating_sub(110), lh as u32), v, t.text, Align::Left);
            y += lh;
        }
        y
    }
}

impl App for SysInfo {
    fn layout(&mut self, w: u32, h: u32, cx: &mut Ctx) {
        self.size = (w, h);
        let full = inset(Rect::new(0, 0, w, h), 16);
        let (_, body) = split_top(full, 64);
        let (left, right) = split_left(body, body.w * 46 / 100);
        let left = Rect::new(left.x, left.y, left.w.saturating_sub(8), left.h);
        let right = Rect::new(right.x + 8, right.y, right.w.saturating_sub(8), right.h);
        let sys_h = 236.min(left.h * 60 / 100);
        self.cards[0].rect = Rect::new(left.x, left.y, left.w, sys_h);
        self.cards[1].rect = Rect::new(left.x, left.y + sys_h as i32 + 12, left.w, left.h.saturating_sub(sys_h + 12));
        let fs_h = (44 + self.info.mounts.len() as u32 * 46).min(right.h / 2);
        self.cards[2].rect = Rect::new(right.x, right.y, right.w, fs_h);
        self.cards[3].rect = Rect::new(right.x, right.y + fs_h as i32 + 12, right.w, right.h.saturating_sub(fs_h + 12));
        let c = self.cards[3].content(&cx.fonts);
        self.procs.rect = Rect::new(c.x - 6, c.y, c.w + 12, c.h + 6);
    }

    fn event(&mut self, ev: Ui, cx: &mut Ctx) -> bool {
        self.procs.event(&ev, cx.focused).needs_redraw()
    }

    fn tick_ms(&self) -> Option<u64> {
        Some(1000)
    }

    fn tick(&mut self, cx: &mut Ctx) -> bool {
        let mut items = Vec::new();
        self.info = gather(&mut items);
        self.procs.set_items(items);
        self.refreshes += 1;
        if self.refreshes.is_multiple_of(30) {
            println!("sysinfo: {} processes, {} free of {}", self.info.procs, human_size(self.info.free_mem), human_size(self.info.total_mem));
        }
        let (w, h) = self.size;
        self.layout(w, h, cx);
        true
    }

    fn draw(&mut self, p: &mut Painter, w: u32, h: u32, cx: &Ctx) {
        let t = cx.theme;
        let i = &self.info;
        p.fill(Rect::new(0, 0, w, h), t.window);
        let full = inset(Rect::new(0, 0, w, h), 16);
        p.text(FontKind::Display, full.x + 2, full.y - 4, "OSyx", t.text);
        let dw = p.fonts.measure(FontKind::Display, "OSyx") as i32;
        let sub = format!("{}  ·  kernel {}  ·  ABI {}  ·  x86_64", i.release, i.kernel, i.abi);
        p.text(FontKind::Ui, full.x + dw + 18, full.y + 14, &sub, t.text_dim);
        let now = self.tz.local(unix_now());
        let clock = format!("{} {}", now.iso(), now.zone);
        p.text_in(FontKind::Ui, Rect::new(full.x, full.y + 6, full.w, 24), &clock, t.text_faint, Align::Right);
        for c in &self.cards {
            c.draw(p);
        }
        // System.
        let r = self.cards[0].content(&cx.fonts);
        Self::rows(
            p,
            r,
            &[
                ("Host", i.hostname.clone()),
                ("Architecture", String::from("x86_64 (long mode, ring 3 user space)")),
                ("Processor", i.cpu.clone()),
                ("Uptime", human_duration(i.uptime_s)),
                ("Display", i.display.clone()),
                ("Input", if i.inputs.is_empty() { String::from("no devices") } else { i.inputs.join(", ") }),
            ],
        );
        // Memory.
        let r = self.cards[1].content(&cx.fonts);
        let used = i.total_mem.saturating_sub(i.free_mem);
        let y = Self::rows(p, r, &[("Total", human_size(i.total_mem)), ("In use", human_size(used)), ("Free", human_size(i.free_mem))]);
        bar(p, Rect::new(r.x, y + 6, r.w, 10), used, i.total_mem);
        let y2 = y + 28;
        Self::rows(p, Rect::new(r.x, y2, r.w, 60), &[("Processes", format!("{} ({} threads)", i.procs, i.threads))]);
        // File systems.
        let r = self.cards[2].content(&cx.fonts);
        let old = p.push_clip(r);
        for (k, m) in i.mounts.iter().enumerate() {
            let y = r.y + k as i32 * 46;
            let used = m.total.saturating_sub(m.free);
            let label = format!("{}  ·  {}  ·  {}", m.path, m.fstype, m.source);
            p.text_in(FontKind::Ui, Rect::new(r.x, y, r.w.saturating_sub(150), 22), &label, t.text, Align::Left);
            let size = if m.total > 0 { format!("{} of {}", human_size(used), human_size(m.total)) } else { String::from("virtual") };
            p.text_in(FontKind::Ui, Rect::new(r.x, y, r.w, 22), &size, t.text_dim, Align::Right);
            bar(p, Rect::new(r.x, y + 26, r.w, 8), used, m.total);
        }
        p.restore_clip(old);
        self.procs.draw(p, cx.focused);
    }
}
