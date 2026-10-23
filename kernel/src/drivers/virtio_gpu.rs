//! virtio-gpu 2D (virtio specification 1.x, section 5.7) over the modern
//! PCI transport.
//!
//! Model: the host owns *resources* (2D images) created by the driver;
//! each is backed by guest pages it reads through `TRANSFER_TO_HOST_2D`.
//! A resource is shown on an output (*scanout*) with `SET_SCANOUT` and
//! updated on screen with `RESOURCE_FLUSH`. Resources and scanouts are
//! separate, so the same resource can be shown on several outputs.
//!
//! Commands go through the control queue one at a time (a request chain:
//! the command, then a response buffer the device writes); every command
//! is fenced (`VIRTIO_GPU_FLAG_FENCE`) and its response type and fence id
//! are checked. The cursor queue carries `UPDATE_CURSOR` / `MOVE_CURSOR`
//! (no response). No 3D (virgl), blob resources or contexts are used: this
//! is a 2D display device, nothing is accelerated.
//!
//! DMA ownership: request and response pages belong to the driver and are
//! only touched between commands; resource backing pages belong to the
//! caller and must outlive the attachment (`detach_backing` before they are
//! freed).

use super::virtio::{Buffer, Transport, VirtioError, Virtqueue};
use crate::mem::{alloc_frame, phys_to_virt, PAGE_SIZE};
use crate::pci;
use crate::sync::{IrqSpinLock, KMutex};
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, Ordering};

pub const DEVICE_ID: u16 = 0x1050;

const F_EDID: u64 = 1 << 1;

// Control commands.
const CMD_GET_DISPLAY_INFO: u32 = 0x0100;
const CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
const CMD_RESOURCE_UNREF: u32 = 0x0102;
const CMD_SET_SCANOUT: u32 = 0x0103;
const CMD_RESOURCE_FLUSH: u32 = 0x0104;
const CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
const CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const CMD_RESOURCE_DETACH_BACKING: u32 = 0x0107;
const CMD_GET_EDID: u32 = 0x010A;
// Cursor commands.
const CMD_UPDATE_CURSOR: u32 = 0x0300;
const CMD_MOVE_CURSOR: u32 = 0x0301;
// Responses.
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_OK_EDID: u32 = 0x1104;

const FLAG_FENCE: u32 = 1;
const HDR_LEN: usize = 24;
const MAX_SCANOUTS: usize = 16;
/// `VIRTIO_GPU_EVENT_DISPLAY` in the configuration's `events_read`.
const EVENT_DISPLAY: u32 = 1;

/// Pixel formats (`enum virtio_gpu_formats`).
pub const FORMAT_B8G8R8A8: u32 = 1;
pub const FORMAT_B8G8R8X8: u32 = 2;

pub const CURSOR_SIZE: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuError {
    Transport(VirtioError),
    /// The device answered with an error response (`VIRTIO_GPU_RESP_ERR_*`).
    Device(u32),
    /// Response type or fence did not match the request.
    BadResponse(u32),
    InvalidArgument,
}

impl GpuError {
    pub fn describe(&self) -> &'static str {
        match self {
            GpuError::Transport(VirtioError::Timeout) => "device timeout",
            GpuError::Transport(_) => "transport error",
            GpuError::Device(0x1201) => "out of memory (host)",
            GpuError::Device(0x1202) => "invalid scanout",
            GpuError::Device(0x1203) => "invalid resource",
            GpuError::Device(0x1205) => "invalid parameter",
            GpuError::Device(_) => "device error",
            GpuError::BadResponse(_) => "unexpected response",
            GpuError::InvalidArgument => "invalid argument",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// One output as `GET_DISPLAY_INFO` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Scanout {
    pub rect: Rect,
    pub enabled: bool,
}

/// Command counters (diagnostics and benchmarks).
#[derive(Clone, Copy, Debug, Default)]
pub struct GpuStats {
    pub commands: u64,
    pub transfers: u64,
    pub flushes: u64,
    pub transferred_pixels: u64,
    pub cursor_updates: u64,
    pub cursor_moves: u64,
    pub errors: u64,
    /// Time spent in transfer + flush commands, nanoseconds.
    pub present_ns: u64,
    pub resources_created: u64,
    pub resources_destroyed: u64,
}

struct Control {
    queue: Virtqueue,
    /// Request pages (commands up to `REQ_PAGES` pages long) and the response page.
    req: [u64; REQ_PAGES],
    resp: u64,
    fence: u64,
    failed: bool,
}

struct Cursor {
    queue: Virtqueue,
    req: u64,
}

/// Request pages: page 0 holds commands; `ATTACH_BACKING` puts its
/// entries (256 per page) on the others, so a backing store may consist of
/// up to 33 * 256 physically discontiguous runs (enough for 4K at 32 bpp).
const REQ_PAGES: usize = 34;

pub struct VirtioGpu {
    pub location: pci::Address,
    transport: Transport,
    // Sleeping locks: a command polls its queue and may yield the CPU.
    control: KMutex<Control>,
    cursor: KMutex<Option<Cursor>>,
    edid: bool,
    pub num_scanouts: u32,
    next_resource: AtomicU64,
    stats: IrqSpinLock<GpuStats>,
}

/// Writes plain little-endian words into a request page.
struct Writer {
    base: u64,
    off: usize,
}

impl Writer {
    fn u32(&mut self, v: u32) -> &mut Self {
        // SAFETY: `base` is a driver-owned request page; callers keep `off`
        // within REQ_PAGES pages (checked by `encode`).
        unsafe { write_volatile((self.base + self.off as u64) as *mut u32, v) };
        self.off += 4;
        self
    }

    fn u64(&mut self, v: u64) -> &mut Self {
        self.u32(v as u32).u32((v >> 32) as u32)
    }

    fn rect(&mut self, r: Rect) -> &mut Self {
        self.u32(r.x).u32(r.y).u32(r.w).u32(r.h)
    }
}

fn read32(base: u64, off: usize) -> u32 {
    // SAFETY: the response page belongs to the driver; the device has
    // returned it.
    unsafe { read_volatile((base + off as u64) as *const u32) }
}

/// The first VirtIO-GPU among `devices` that initialises.
pub fn find(devices: &[pci::Device]) -> Option<alloc::sync::Arc<VirtioGpu>> {
    for d in devices.iter().filter(|d| d.vendor == super::virtio::VENDOR && d.device == DEVICE_ID) {
        match VirtioGpu::probe(d) {
            Ok(g) => return Some(alloc::sync::Arc::new(g)),
            Err(e) => crate::kprintln!("virtio-gpu at {}: initialisation failed: {:?}", d.addr, e),
        }
    }
    None
}

impl VirtioGpu {
    pub fn probe(dev: &pci::Device) -> Result<VirtioGpu, VirtioError> {
        let t = Transport::new(dev)?;
        let features = t.negotiate(F_EDID)?;
        let queue = t.setup_queue(0)?;
        let cursor_q = t.setup_queue(1).ok();
        let mut req = [0u64; REQ_PAGES];
        for r in req.iter_mut() {
            *r = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        }
        let resp = alloc_frame().ok_or(VirtioError::OutOfMemory)?;
        let cursor = match cursor_q {
            Some(q) => Some(Cursor { queue: q, req: alloc_frame().ok_or(VirtioError::OutOfMemory)? }),
            None => None,
        };
        t.driver_ok();
        let num_scanouts = t.device_config_u32(8).clamp(1, MAX_SCANOUTS as u32);
        Ok(VirtioGpu {
            location: dev.addr,
            transport: t,
            control: KMutex::new(Control { queue, req, resp, fence: 0, failed: false }),
            cursor: KMutex::new(cursor),
            edid: features & F_EDID != 0,
            num_scanouts,
            next_resource: AtomicU64::new(1),
            stats: IrqSpinLock::new(GpuStats::default()),
        })
    }

    pub fn has_cursor(&self) -> bool {
        self.cursor.lock().is_some()
    }

    pub fn has_edid(&self) -> bool {
        self.edid
    }

    pub fn stats(&self) -> GpuStats {
        *self.stats.lock()
    }

    /// Runs one fenced control command whose body (after the 24-byte
    /// header) `body` writes; the request may continue on `extra` request
    /// pages (`(page index, bytes)`, used by `attach_backing`). The device
    /// must answer `expect` within `resp_len` bytes; `read` then sees the
    /// response page while the queue is still ours.
    fn command<R>(
        &self,
        kind: u32,
        expect: u32,
        resp_len: usize,
        body: impl FnOnce(&mut Writer, &[u64; REQ_PAGES]) -> usize,
        read: impl FnOnce(u64) -> R,
    ) -> Result<R, GpuError> {
        let mut c = self.control.lock();
        if c.failed {
            return Err(GpuError::Transport(VirtioError::Timeout));
        }
        c.fence += 1;
        let fence = c.fence;
        let mut w = Writer { base: phys_to_virt(c.req[0]), off: 0 };
        w.u32(kind).u32(FLAG_FENCE).u64(fence).u32(0).u32(0);
        let extra = body(&mut w, &c.req);
        if w.off > PAGE_SIZE as usize {
            return Err(GpuError::InvalidArgument);
        }
        let resp_virt = phys_to_virt(c.resp);
        // SAFETY: the response page is ours between commands.
        unsafe { write_volatile(resp_virt as *mut u32, 0) };
        let mut chain: Vec<Buffer> = Vec::with_capacity(extra.div_ceil(PAGE_SIZE as usize) + 2);
        chain.push(Buffer { phys: c.req[0], len: w.off as u32, device_writes: false });
        let mut left = extra;
        for &page in &c.req[1..] {
            if left == 0 {
                break;
            }
            let n = left.min(PAGE_SIZE as usize);
            chain.push(Buffer { phys: page, len: n as u32, device_writes: false });
            left -= n;
        }
        chain.push(Buffer { phys: c.resp, len: resp_len as u32, device_writes: true });
        let r = c.queue.run(&chain);
        let mut st = self.stats.lock();
        st.commands += 1;
        if let Err(e) = r {
            st.errors += 1;
            drop(st);
            c.failed = true;
            crate::kprintln!("virtio-gpu: command {:#x} failed: {:?}; device disabled", kind, e);
            return Err(GpuError::Transport(e));
        }
        let got = read32(resp_virt, 0);
        let flags = read32(resp_virt, 4);
        let rfence = read32(resp_virt, 8) as u64 | (read32(resp_virt, 12) as u64) << 32;
        if got != expect {
            st.errors += 1;
            return Err(if got >= 0x1200 { GpuError::Device(got) } else { GpuError::BadResponse(got) });
        }
        if flags & FLAG_FENCE != 0 && rfence != fence {
            st.errors += 1;
            return Err(GpuError::BadResponse(got));
        }
        drop(st);
        Ok(read(resp_virt))
    }

    /// A command without extra request pages or response data.
    fn simple(&self, kind: u32, body: impl FnOnce(&mut Writer)) -> Result<(), GpuError> {
        self.command(
            kind,
            RESP_OK_NODATA,
            HDR_LEN,
            |w, _| {
                body(w);
                0
            },
            |_| (),
        )
    }

    /// Outputs and their preferred geometry (`GET_DISPLAY_INFO`).
    pub fn display_info(&self) -> Result<Vec<Scanout>, GpuError> {
        let n = self.num_scanouts as usize;
        self.command(CMD_GET_DISPLAY_INFO, RESP_OK_DISPLAY_INFO, HDR_LEN + MAX_SCANOUTS * 24, |_, _| 0, |base| {
            (0..n)
                .map(|i| {
                    let o = HDR_LEN + i * 24;
                    Scanout {
                        rect: Rect { x: read32(base, o), y: read32(base, o + 4), w: read32(base, o + 8), h: read32(base, o + 12) },
                        enabled: read32(base, o + 16) != 0,
                    }
                })
                .collect()
        })
    }

    /// The EDID block of an output (`GET_EDID`), if the device offers it.
    pub fn edid(&self, scanout: u32) -> Result<Vec<u8>, GpuError> {
        if !self.edid {
            return Err(GpuError::InvalidArgument);
        }
        self.command(
            CMD_GET_EDID,
            RESP_OK_EDID,
            HDR_LEN + 8 + 1024,
            |w, _| {
                w.u32(scanout).u32(0);
                0
            },
            |base| {
                let size = (read32(base, HDR_LEN) as usize).min(1024);
                let mut v = alloc::vec![0u8; size];
                // SAFETY: the response page holds HDR_LEN + 8 + 1024 bytes.
                unsafe { core::ptr::copy_nonoverlapping((base + HDR_LEN as u64 + 8) as *const u8, v.as_mut_ptr(), size) };
                v
            },
        )
    }

    /// Display-change events pending in the configuration space; clears them.
    pub fn take_display_event(&self) -> bool {
        let ev = self.transport.device_config_u32(0);
        if ev & EVENT_DISPLAY != 0 {
            self.transport.write_device_config_u32(4, EVENT_DISPLAY);
            true
        } else {
            false
        }
    }

    pub fn create_resource(&self, width: u32, height: u32, format: u32) -> Result<u32, GpuError> {
        if width == 0 || height == 0 || width > 16384 || height > 16384 {
            return Err(GpuError::InvalidArgument);
        }
        let id = self.next_resource.fetch_add(1, Ordering::Relaxed) as u32;
        self.simple(CMD_RESOURCE_CREATE_2D, |w| {
            w.u32(id).u32(format).u32(width).u32(height);
        })?;
        self.stats.lock().resources_created += 1;
        Ok(id)
    }

    pub fn unref_resource(&self, id: u32) -> Result<(), GpuError> {
        self.simple(CMD_RESOURCE_UNREF, |w| {
            w.u32(id).u32(0);
        })?;
        self.stats.lock().resources_destroyed += 1;
        Ok(())
    }

    /// Attaches guest pages (in order) as the resource's backing store.
    /// Physically contiguous runs become one entry each.
    pub fn attach_backing(&self, id: u32, frames: &[u64]) -> Result<(), GpuError> {
        let mut runs: Vec<(u64, u64)> = Vec::new();
        for &f in frames {
            match runs.last_mut() {
                Some((start, len)) if *start + *len == f && *len < u32::MAX as u64 - PAGE_SIZE => *len += PAGE_SIZE,
                _ => runs.push((f, PAGE_SIZE)),
            }
        }
        let per_page = PAGE_SIZE as usize / 16;
        if runs.is_empty() || runs.len() > per_page * (REQ_PAGES - 1) {
            return Err(GpuError::InvalidArgument);
        }
        self.command(
            CMD_RESOURCE_ATTACH_BACKING,
            RESP_OK_NODATA,
            HDR_LEN,
            |w, req| {
                w.u32(id).u32(runs.len() as u32);
                for (pi, chunk) in runs.chunks(per_page).enumerate() {
                    let mut e = Writer { base: phys_to_virt(req[1 + pi]), off: 0 };
                    for &(addr, len) in chunk {
                        e.u64(addr).u32(len as u32).u32(0);
                    }
                }
                runs.len() * 16
            },
            |_| (),
        )
    }

    pub fn detach_backing(&self, id: u32) -> Result<(), GpuError> {
        self.simple(CMD_RESOURCE_DETACH_BACKING, |w| {
            w.u32(id).u32(0);
        })
    }

    /// Shows resource `id` (0 disables the output) on `scanout`.
    pub fn set_scanout(&self, scanout: u32, id: u32, r: Rect) -> Result<(), GpuError> {
        self.simple(CMD_SET_SCANOUT, |w| {
            w.rect(r).u32(scanout).u32(id);
        })
    }

    /// No command is in flight (the panic path only submits then).
    pub fn is_idle(&self) -> bool {
        self.control.try_lock().is_some()
    }

    /// Copies `r` of the backing store (row stride `stride` bytes) to the
    /// host resource.
    pub fn transfer(&self, id: u32, r: Rect, stride: u32) -> Result<(), GpuError> {
        let offset = r.y as u64 * stride as u64 + r.x as u64 * 4;
        self.simple(CMD_TRANSFER_TO_HOST_2D, |w| {
            w.rect(r).u64(offset).u32(id).u32(0);
        })
    }

    /// `transfer`, then flushes `r` to every output showing the resource.
    pub fn present(&self, id: u32, r: Rect, stride: u32) -> Result<(), GpuError> {
        let t0 = crate::interrupts::monotonic_ns();
        self.transfer(id, r, stride)?;
        self.simple(CMD_RESOURCE_FLUSH, |w| {
            w.rect(r).u32(id).u32(0);
        })?;
        let mut st = self.stats.lock();
        st.transfers += 1;
        st.flushes += 1;
        st.transferred_pixels += r.w as u64 * r.h as u64;
        st.present_ns += crate::interrupts::monotonic_ns() - t0;
        Ok(())
    }

    fn cursor_command(&self, kind: u32, scanout: u32, x: u32, y: u32, resource: u32, hot: (u32, u32)) -> Result<(), GpuError> {
        let mut g = self.cursor.lock();
        let c = g.as_mut().ok_or(GpuError::InvalidArgument)?;
        let mut w = Writer { base: phys_to_virt(c.req), off: 0 };
        w.u32(kind).u32(0).u64(0).u32(0).u32(0);
        w.u32(scanout).u32(x).u32(y).u32(0);
        w.u32(resource).u32(hot.0).u32(hot.1).u32(0);
        // Cursor commands have no response data; the device returns the chain.
        let r = c.queue.run(&[Buffer { phys: c.req, len: w.off as u32, device_writes: false }]);
        drop(g);
        let mut st = self.stats.lock();
        match r {
            Ok(_) => {
                if kind == CMD_UPDATE_CURSOR {
                    st.cursor_updates += 1;
                } else {
                    st.cursor_moves += 1;
                }
                Ok(())
            }
            Err(e) => {
                st.errors += 1;
                Err(GpuError::Transport(e))
            }
        }
    }

    /// Sets the cursor image (resource 0 hides it) and position.
    pub fn update_cursor(&self, scanout: u32, x: u32, y: u32, resource: u32, hot_x: u32, hot_y: u32) -> Result<(), GpuError> {
        self.cursor_command(CMD_UPDATE_CURSOR, scanout, x, y, resource, (hot_x, hot_y))
    }

    pub fn move_cursor(&self, scanout: u32, x: u32, y: u32) -> Result<(), GpuError> {
        self.cursor_command(CMD_MOVE_CURSOR, scanout, x, y, 0, (0, 0))
    }
}
