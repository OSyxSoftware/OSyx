//! OSyx Task Manager (`/system/bin/taskmgr`): processes and live
//! performance graphs. The layout follows Windows 11's Task Manager (a
//! navigation rail, a process table, a performance page with a device list
//! and one large graph) with btop-style graphs (filled areas coloured from
//! green through yellow to red by load, dense history, numeric readouts).
//!
//! Everything shown is measured: CPU load from the kernel's busy/idle tick
//! counters, per-process CPU from each process's ticks, memory from the
//! frame allocator, disk throughput from the block layer's byte counters,
//! network throughput and addresses from the network stack. Sampled once a
//! second; 60 samples of history.

#![no_std]
#![no_main]

#[path = "../shell_common.rs"]
mod common;

use common::{human_duration, human_size, run_app, App, Ctx};
use os_std::prelude::*;
use os_std::{os, system};
use osyx_gfx::{Color, Rect};
use osyx_ui::widget::{Button, ButtonStyle, Column, ListItem, ListView};
use osyx_ui::{icon, Align, Event as Ui, FontKind, Icon, Painter, Response};
use prism_client::WindowSpec;

os_std::entry!(main);

const HISTORY: usize = 60;
const RAIL: u32 = 220;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Processes,
    Performance,
    Details,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Device {
    Cpu,
    Memory,
    Disk,
    Net,
}

/// A rolling series of samples (0..=100 for percentages, raw otherwise).
#[derive(Clone)]
struct Series {
    v: Vec<f32>,
}

impl Series {
    fn new() -> Series {
        Series { v: vec![0.0; HISTORY] }
    }
    fn push(&mut self, x: f32) {
        self.v.remove(0);
        self.v.push(x);
    }
    fn last(&self) -> f32 {
        *self.v.last().unwrap_or(&0.0)
    }
    fn max(&self) -> f32 {
        self.v.iter().copied().fold(0.0, f32::max)
    }
}

#[derive(Clone, Copy, Default)]
struct Sample {
    ns: u64,
    ticks: u64,
    idle: u64,
    disk_read: u64,
    disk_write: u64,
    rx: u64,
    tx: u64,
}

struct ProcRow {
    pid: u64,
    name: String,
    user: String,
    zombie: bool,
    cpu: f32,
    memory: u64,
    threads: u32,
}

struct TaskMgr {
    page: Page,
    device: Device,
    size: (u32, u32),
    cpu_name: String,
    cpu: Series,
    mem: Series,
    disk_r: Series,
    disk_w: Series,
    rx: Series,
    tx: Series,
    prev: Option<Sample>,
    prev_proc: Vec<(u64, u64)>,
    rows: Vec<ProcRow>,
    sort: (usize, bool),
    list: ListView,
    end_task: Button,
    total_mem: u64,
    free_mem: u64,
    uptime_s: u64,
    threads: u32,
    iface: String,
    ipv4: String,
    status: String,
}

fn cpu_brand() -> String {
    use core::arch::x86_64::__cpuid;
    let max_ext = __cpuid(0x8000_0000).eax;
    if max_ext < 0x8000_0004 {
        return String::from("x86-64 processor");
    }
    let mut b = Vec::new();
    for leaf in 0x8000_0002u32..=0x8000_0004 {
        let r = __cpuid(leaf);
        for x in [r.eax, r.ebx, r.ecx, r.edx] {
            b.extend_from_slice(&x.to_le_bytes());
        }
    }
    String::from_utf8_lossy(&b).trim_matches(|c: char| c == '\0' || c == ' ').to_string()
}

/// btop-like colour for a load fraction: green → yellow → red.
fn heat(f: f32) -> Color {
    let f = f.clamp(0.0, 1.0);
    let (r, g) =
        if f < 0.5 { ((f * 2.0 * 255.0) as u32, 210) } else { (240, (210.0 * (1.0 - (f - 0.5) * 2.0)) as u32) };
    osyx_gfx::rgb(r as u8, g as u8, 90)
}

fn rate(bytes_per_s: f32) -> String {
    let b = bytes_per_s.max(0.0);
    if b >= 1048576.0 {
        format!("{:.1} MB/s", b / 1048576.0)
    } else if b >= 1024.0 {
        format!("{:.0} KB/s", b / 1024.0)
    } else {
        format!("{:.0} B/s", b)
    }
}

fn bits(bytes_per_s: f32) -> String {
    let k = bytes_per_s.max(0.0) * 8.0 / 1000.0;
    if k >= 1000.0 {
        format!("{:.1} Mbps", k / 1000.0)
    } else {
        format!("{:.0} Kbps", k)
    }
}

impl TaskMgr {
    fn sample(&mut self) {
        let si = os::sysinfo().ok();
        let (mut rx, mut tx) = (0u64, 0u64);
        if let Ok(ifs) = os_std::net::interfaces() {
            for i in ifs.iter().filter(|i| i.flags & os_std::net::IFF_LOOPBACK == 0) {
                rx += i.rx_bytes;
                tx += i.tx_bytes;
                if self.iface.is_empty() || self.iface == os_std::net::field(&i.name) {
                    self.iface = String::from(os_std::net::field(&i.name));
                    self.ipv4 = if i.addr == [0, 0, 0, 0] {
                        String::from("not configured")
                    } else {
                        format!("{}.{}.{}.{}/{}", i.addr[0], i.addr[1], i.addr[2], i.addr[3], i.prefix)
                    };
                }
            }
        }
        let s = Sample {
            ns: si.as_ref().map_or(0, |i| i.uptime_ns),
            ticks: si.as_ref().map_or(0, |i| i.ticks),
            idle: si.as_ref().map_or(0, |i| i.idle_ticks),
            disk_read: si.as_ref().map_or(0, |i| i.disk_read_bytes),
            disk_write: si.as_ref().map_or(0, |i| i.disk_write_bytes),
            rx,
            tx,
        };
        self.total_mem = si.as_ref().map_or(0, |i| i.total_memory);
        self.free_mem = si.as_ref().map_or(0, |i| i.free_memory);
        self.uptime_s = si.as_ref().map_or(0, |i| i.uptime_ns / 1_000_000_000);
        let dt_ticks = self.prev.map_or(0, |p| s.ticks.saturating_sub(p.ticks));
        if let Some(p) = self.prev {
            let seconds = s.ns.saturating_sub(p.ns).max(1) as f32 / 1_000_000_000.0;
            let busy = dt_ticks.saturating_sub(s.idle.saturating_sub(p.idle));
            self.cpu.push(if dt_ticks > 0 { busy as f32 * 100.0 / dt_ticks as f32 } else { 0.0 });
            self.disk_r.push(s.disk_read.saturating_sub(p.disk_read) as f32 / seconds);
            self.disk_w.push(s.disk_write.saturating_sub(p.disk_write) as f32 / seconds);
            self.rx.push(s.rx.saturating_sub(p.rx) as f32 / seconds);
            self.tx.push(s.tx.saturating_sub(p.tx) as f32 / seconds);
        }
        if self.total_mem > 0 {
            self.mem.push((self.total_mem - self.free_mem) as f32 * 100.0 / self.total_mem as f32);
        }
        self.prev = Some(s);
        // Processes.
        let list = system::processes().unwrap_or_default();
        self.threads = list.iter().map(|p| p.threads).sum();
        let mut rows = Vec::new();
        let mut now = Vec::new();
        for p in &list {
            let before = self.prev_proc.iter().find(|(pid, _)| *pid == p.pid).map(|(_, t)| *t);
            let cpu = match before {
                Some(b) if dt_ticks > 0 => p.cpu_ticks.saturating_sub(b) as f32 * 100.0 / dt_ticks as f32,
                _ => 0.0,
            };
            now.push((p.pid, p.cpu_ticks));
            rows.push(ProcRow {
                pid: p.pid,
                name: system::process_name(p),
                user: if p.uid == 0 {
                    String::from("root")
                } else if p.uid == 1000 {
                    String::from("user")
                } else {
                    format!("{}", p.uid)
                },
                zombie: p.state == system::PROC_ZOMBIE,
                cpu: cpu.min(100.0),
                memory: p.frames * 4096,
                threads: p.threads,
            });
        }
        self.prev_proc = now;
        let selected_pid = self.list.selected().and_then(|i| self.rows.get(i).map(|r| r.pid));
        self.rows = rows;
        self.list.select(None);
        self.sort_rows();
        self.list.select(selected_pid.and_then(|pid| self.rows.iter().position(|r| r.pid == pid)));
    }

    fn sort_rows(&mut self) {
        let selected_pid = self.list.selected().and_then(|i| self.rows.get(i).map(|r| r.pid));
        let (col, desc) = self.sort;
        self.rows.sort_by(|a, b| {
            let o = match col {
                0 => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                1 => a.pid.cmp(&b.pid),
                2 => a.user.cmp(&b.user),
                4 => a.cpu.partial_cmp(&b.cpu).unwrap_or(core::cmp::Ordering::Equal),
                5 => a.memory.cmp(&b.memory),
                6 => a.threads.cmp(&b.threads),
                _ => a.zombie.cmp(&b.zombie),
            };
            if desc {
                o.reverse()
            } else {
                o
            }
        });
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|r| {
                let mut it = ListItem::new(
                    Some(if r.zombie { Icon::Close } else { Icon::Program }),
                    &[
                        &r.name,
                        &format!("{}", r.pid),
                        &r.user,
                        if r.zombie { "Exited" } else { "Running" },
                        &format!("{:.1}%", r.cpu),
                        &human_size(r.memory),
                        &format!("{}", r.threads),
                    ],
                );
                it.dim = r.zombie;
                it
            })
            .collect();
        self.list.set_items(items);
        self.list.select(selected_pid.and_then(|pid| self.rows.iter().position(|r| r.pid == pid)));
    }

    fn rail_items() -> [(Page, &'static str, Icon); 3] {
        [
            (Page::Processes, "Processes", Icon::Apps),
            (Page::Performance, "Performance", Icon::Info),
            (Page::Details, "Details", Icon::Program),
        ]
    }

    fn content(&self) -> Rect {
        let (w, h) = self.size;
        Rect::new(RAIL as i32, 52, w.saturating_sub(RAIL), h.saturating_sub(52))
    }

    fn device_rects(&self) -> Vec<(Device, Rect)> {
        let c = self.content();
        [Device::Cpu, Device::Memory, Device::Disk, Device::Net]
            .iter()
            .enumerate()
            .map(|(i, d)| (*d, Rect::new(c.x + 8, c.y + 8 + i as i32 * 74, 230, 66)))
            .collect()
    }

    /// Area graph with grid, btop colours and an outline.
    fn graph(p: &mut Painter, r: Rect, s: &Series, scale: f32, base: Color, grid: bool) {
        let t = *p.theme;
        p.fill(r, osyx_gfx::rgb(16, 18, 24));
        if grid {
            for i in 1..10 {
                p.vline(r.x + (r.w * i / 10) as i32, r.y, r.h, osyx_gfx::rgb(34, 38, 48));
            }
            for i in 1..5 {
                p.hline(r.x, r.y + (r.h * i / 5) as i32, r.w, osyx_gfx::rgb(34, 38, 48));
            }
        }
        let n = s.v.len().max(2);
        let step = r.w as f32 / (n - 1) as f32;
        let mut prev: Option<(i32, i32)> = None;
        for (i, v) in s.v.iter().enumerate() {
            let f = if scale > 0.0 { (*v / scale).clamp(0.0, 1.0) } else { 0.0 };
            let x = r.x + (i as f32 * step) as i32;
            let y = r.y + r.h as i32 - (f * r.h as f32) as i32;
            if let Some((px, py)) = prev {
                // Fill the columns between samples, coloured by height.
                for cx in px..=x {
                    let k = if x == px { 1.0 } else { (cx - px) as f32 / (x - px) as f32 };
                    let cy = py + ((y - py) as f32 * k) as i32;
                    let h = (r.y + r.h as i32 - cy).max(0) as u32;
                    let col = if base == 0 { heat(1.0 - (cy - r.y) as f32 / r.h as f32) } else { base };
                    p.blend(Rect::new(cx, cy, 1, h), col, 110);
                }
                p.line(px, py, x, y, if base == 0 { heat(f) } else { base });
            }
            prev = Some((x, y));
        }
        p.hline(r.x, r.y, r.w, t.border);
        p.hline(r.x, r.y + r.h as i32 - 1, r.w, t.border);
        p.vline(r.x, r.y, r.h, t.border);
        p.vline(r.x + r.w as i32 - 1, r.y, r.h, t.border);
    }

    fn draw_processes(&mut self, p: &mut Painter, cx: &Ctx, details: bool) {
        let t = cx.theme;
        let c = self.content();
        let cpu = self.cpu.last();
        let mem = self.mem.last();
        // Summary strip like Task Manager's column headers with totals.
        let strip = Rect::new(c.x + 16, c.y + 8, c.w.saturating_sub(32), 40);
        p.text(
            FontKind::Bold,
            strip.x,
            strip.y + 4,
            &format!("{} processes · {} threads", self.rows.len(), self.threads),
            t.text,
        );
        p.text_in(
            FontKind::Ui,
            strip,
            &format!(
                "CPU {:.0}%    Memory {:.0}%    Disk {}    Network {}",
                cpu,
                mem,
                rate(self.disk_r.last() + self.disk_w.last()),
                bits(self.rx.last() + self.tx.last())
            ),
            t.text_dim,
            Align::Right,
        );
        self.list.rect = Rect::new(c.x + 8, c.y + 52, c.w.saturating_sub(16), c.h.saturating_sub(60));
        self.list.columns[3].width = if details { 90 } else { 70 };
        self.list.draw(p, cx.focused);
    }

    fn draw_performance(&mut self, p: &mut Painter, cx: &Ctx) {
        let t = cx.theme;
        let c = self.content();
        // Device list with sparklines.
        for (d, r) in self.device_rects() {
            let sel = d == self.device;
            if sel {
                p.rounded(r, 6, t.selection, 255);
                p.fill(Rect::new(r.x, r.y + 18, 3, 30), t.accent);
            }
            let spark = Rect::new(r.x + 10, r.y + 10, 70, 46);
            let (title, line1, line2, series, scale, color) = match d {
                Device::Cpu => ("CPU", format!("{:.0}%", self.cpu.last()), String::new(), &self.cpu, 100.0, 0),
                Device::Memory => (
                    "Memory",
                    format!(
                        "{}/{} ({:.0}%)",
                        human_size(self.total_mem - self.free_mem),
                        human_size(self.total_mem),
                        self.mem.last()
                    ),
                    String::new(),
                    &self.mem,
                    100.0,
                    osyx_gfx::rgb(150, 110, 230),
                ),
                Device::Disk => (
                    "Disk 0 (C:)",
                    String::from("VirtIO block"),
                    rate(self.disk_r.last() + self.disk_w.last()),
                    &self.disk_r,
                    self.disk_r.max().max(self.disk_w.max()).max(65536.0),
                    osyx_gfx::rgb(90, 200, 120),
                ),
                Device::Net => (
                    "Ethernet",
                    self.iface.clone(),
                    format!("S: {} R: {}", bits(self.tx.last()), bits(self.rx.last())),
                    &self.rx,
                    self.rx.max().max(self.tx.max()).max(16384.0),
                    osyx_gfx::rgb(230, 120, 170),
                ),
            };
            Self::graph(p, spark, series, scale, color, false);
            p.text(FontKind::Large, r.x + 92, r.y + 6, title, t.text);
            p.text(FontKind::Ui, r.x + 92, r.y + 30, &line1, t.text_dim);
            if !line2.is_empty() {
                p.text(FontKind::Ui, r.x + 92, r.y + 46, &line2, t.text_dim);
            }
        }
        // The selected device's large graph and readouts.
        let area = Rect::new(c.x + 256, c.y + 8, c.w.saturating_sub(272), c.h.saturating_sub(16));
        let (title, subtitle, series, scale, color, label) = match self.device {
            Device::Cpu => ("CPU", self.cpu_name.clone(), &self.cpu, 100.0, 0, "% Utilization"),
            Device::Memory => {
                ("Memory", human_size(self.total_mem), &self.mem, 100.0, osyx_gfx::rgb(150, 110, 230), "Memory usage")
            }
            Device::Disk => (
                "Disk 0 (C:)",
                String::from("VirtIO block device"),
                &self.disk_r,
                self.disk_r.max().max(self.disk_w.max()).max(65536.0),
                osyx_gfx::rgb(90, 200, 120),
                "Read throughput",
            ),
            Device::Net => (
                "Ethernet",
                self.iface.clone(),
                &self.rx,
                self.rx.max().max(self.tx.max()).max(16384.0),
                osyx_gfx::rgb(230, 120, 170),
                "Receive",
            ),
        };
        p.text(FontKind::Display, area.x, area.y - 2, title, t.text);
        p.text_in(FontKind::Large, Rect::new(area.x, area.y + 8, area.w, 30), &subtitle, t.text, Align::Right);
        p.text(FontKind::Ui, area.x, area.y + 50, label, t.text_dim);
        let gh = area.h.saturating_sub(220).max(120);
        let g = Rect::new(area.x, area.y + 70, area.w, gh);
        Self::graph(p, g, series, scale, color, true);
        if self.device == Device::Disk || self.device == Device::Net {
            // Second series (writes / sends) as an outline over the first.
            let other = if self.device == Device::Disk { &self.disk_w } else { &self.tx };
            let n = other.v.len().max(2);
            let step = g.w as f32 / (n - 1) as f32;
            for i in 1..other.v.len() {
                let y = |v: f32| g.y + g.h as i32 - ((v / scale).clamp(0.0, 1.0) * g.h as f32) as i32;
                p.line(
                    g.x + ((i - 1) as f32 * step) as i32,
                    y(other.v[i - 1]),
                    g.x + (i as f32 * step) as i32,
                    y(other.v[i]),
                    osyx_gfx::rgb(240, 200, 80),
                );
            }
        }
        p.text(FontKind::Ui, g.x, g.y + g.h as i32 + 4, "60 seconds", t.text_faint);
        p.text_in(FontKind::Ui, Rect::new(g.x, g.y + g.h as i32 + 4, g.w, 20), "0", t.text_faint, Align::Right);
        let sy = g.y + g.h as i32 + 32;
        let stat = |p: &mut Painter, x: i32, y: i32, k: &str, v: &str| {
            p.text(FontKind::Ui, x, y, k, t.text_dim);
            p.text(FontKind::Large, x, y + 18, v, t.text);
        };
        match self.device {
            Device::Cpu => {
                stat(p, g.x, sy, "Utilization", &format!("{:.0}%", self.cpu.last()));
                stat(p, g.x + 140, sy, "Processes", &format!("{}", self.rows.len()));
                stat(p, g.x + 260, sy, "Threads", &format!("{}", self.threads));
                stat(p, g.x, sy + 60, "Up time", &human_duration(self.uptime_s));
                stat(p, g.x + 260, sy + 60, "Logical processors", "1");
            }
            Device::Memory => {
                stat(p, g.x, sy, "In use", &human_size(self.total_mem - self.free_mem));
                stat(p, g.x + 160, sy, "Available", &human_size(self.free_mem));
                stat(p, g.x + 320, sy, "Total", &human_size(self.total_mem));
            }
            Device::Disk => {
                stat(p, g.x, sy, "Read speed", &rate(self.disk_r.last()));
                stat(p, g.x + 180, sy, "Write speed", &rate(self.disk_w.last()));
                p.text(FontKind::Ui, g.x + 360, sy, "green: read, yellow: write", t.text_faint);
            }
            Device::Net => {
                stat(p, g.x, sy, "Send", &bits(self.tx.last()));
                stat(p, g.x + 160, sy, "Receive", &bits(self.rx.last()));
                stat(p, g.x + 320, sy, "IPv4 address", &self.ipv4);
            }
        }
    }
}

impl App for TaskMgr {
    fn layout(&mut self, w: u32, h: u32, _cx: &mut Ctx) {
        self.size = (w, h);
        self.end_task.rect = Rect::new(w as i32 - 128, 10, 112, 32);
    }

    fn event(&mut self, ev: Ui, cx: &mut Ctx) -> bool {
        if let Ui::PointerDown { x, y, .. } = ev {
            if x < RAIL as i32 {
                for (i, (page, _, _)) in Self::rail_items().iter().enumerate() {
                    let r = Rect::new(8, 60 + i as i32 * 44, RAIL - 16, 38);
                    if y >= r.y && y < r.y + r.h as i32 {
                        self.page = *page;
                        return true;
                    }
                }
            }
            if self.page == Page::Performance {
                for (d, r) in self.device_rects() {
                    if x >= r.x && y >= r.y && x < r.x + r.w as i32 && y < r.y + r.h as i32 {
                        self.device = d;
                        return true;
                    }
                }
            }
            // Clicking a column header sorts by it (again: reverses).
            let lr = self.list.rect;
            if self.page != Page::Performance && y >= lr.y && y < lr.y + 30 && x >= lr.x {
                let mut cx0 = lr.x;
                let flex: u32 = lr.w.saturating_sub(self.list.columns.iter().map(|c| c.width).sum());
                for (i, col) in self.list.columns.iter().enumerate() {
                    let w = if col.width == 0 && i == 0 { flex } else { col.width };
                    if x >= cx0 && x < cx0 + w as i32 {
                        self.sort = if self.sort.0 == i { (i, !self.sort.1) } else { (i, i >= 4) };
                        self.sort_rows();
                        return true;
                    }
                    cx0 += w as i32;
                }
            }
        }
        let mut redraw = false;
        if self.page != Page::Performance {
            redraw |= self.list.event(&ev, cx.focused).needs_redraw();
            if self.end_task.event(&ev, cx.focused) == Response::Submitted {
                if let Some(r) = self.list.selected().and_then(|i| self.rows.get(i)) {
                    let (pid, name) = (r.pid, r.name.clone());
                    self.status = match system::kill(pid) {
                        Ok(()) => format!("Ended {name} (PID {pid})"),
                        Err(e) => format!("Cannot end {name}: {e}"),
                    };
                    self.sample();
                }
                redraw = true;
            }
        }
        redraw
    }

    fn tick_ms(&self) -> Option<u64> {
        Some(1000)
    }

    fn tick(&mut self, _cx: &mut Ctx) -> bool {
        self.sample();
        true
    }

    fn draw(&mut self, p: &mut Painter, w: u32, h: u32, cx: &Ctx) {
        let t = cx.theme;
        p.fill(Rect::new(0, 0, w, h), osyx_gfx::rgb(24, 26, 33));
        // Navigation rail.
        p.fill(Rect::new(0, 0, RAIL, h), osyx_gfx::rgb(30, 33, 42));
        p.text(FontKind::Bold, 20, 16, "Task Manager", t.text);
        for (i, (page, label, ic)) in Self::rail_items().iter().enumerate() {
            let r = Rect::new(8, 60 + i as i32 * 44, RAIL - 16, 38);
            if *page == self.page {
                p.rounded(r, 6, osyx_gfx::rgb(44, 48, 60), 255);
                p.rounded(Rect::new(r.x, r.y + 10, 3, 18), 1, t.accent, 255);
            }
            icon::draw(p, *ic, Rect::new(r.x + 14, r.y + 9, 20, 20), t.text_dim);
            p.text(FontKind::Ui, r.x + 46, r.y + 10, label, t.text);
        }
        // Header of the content area.
        let title = match self.page {
            Page::Processes => "Processes",
            Page::Performance => "Performance",
            Page::Details => "Details",
        };
        p.fill(Rect::new(RAIL as i32, 0, w.saturating_sub(RAIL), 52), osyx_gfx::rgb(24, 26, 33));
        p.text(FontKind::Bold, RAIL as i32 + 20, 16, title, t.text);
        p.hline(RAIL as i32, 51, w.saturating_sub(RAIL), t.border);
        if !self.status.is_empty() {
            p.text_in(
                FontKind::Ui,
                Rect::new(RAIL as i32 + 140, 16, w.saturating_sub(RAIL + 300), 20),
                &self.status,
                t.text_dim,
                Align::Left,
            );
        }
        match self.page {
            Page::Processes => {
                self.draw_processes(p, cx, false);
                self.end_task.draw(p, cx.focused);
            }
            Page::Details => {
                self.draw_processes(p, cx, true);
                self.end_task.draw(p, cx.focused);
            }
            Page::Performance => self.draw_performance(p, cx),
        }
    }
}

fn main() -> i32 {
    let mut list = ListView::new(vec![
        Column::new("Name", 0, Align::Left),
        Column::new("PID", 64, Align::Right),
        Column::new("User", 80, Align::Left),
        Column::new("Status", 0, Align::Left),
        Column::new("CPU", 72, Align::Right),
        Column::new("Memory", 96, Align::Right),
        Column::new("Threads", 76, Align::Right),
    ]);
    list.row_height = 30;
    let mut app = TaskMgr {
        page: Page::Processes,
        device: Device::Cpu,
        size: (0, 0),
        cpu_name: cpu_brand(),
        cpu: Series::new(),
        mem: Series::new(),
        disk_r: Series::new(),
        disk_w: Series::new(),
        rx: Series::new(),
        tx: Series::new(),
        prev: None,
        prev_proc: Vec::new(),
        rows: Vec::new(),
        sort: (4, true),
        list,
        end_task: Button::new(1, "End task", ButtonStyle::Secondary).with_icon(Icon::Close),
        total_mem: 0,
        free_mem: 0,
        uptime_s: 0,
        threads: 0,
        iface: String::new(),
        ipv4: String::new(),
        status: String::new(),
    };
    app.sample();
    println!("taskmgr: started; {} processes, CPU {}", app.rows.len(), app.cpu_name);
    let mut spec = WindowSpec::normal("Task Manager", 1180, 760);
    spec.min_width = 860;
    spec.min_height = 560;
    run_app("taskmgr", spec, &mut app)
}
