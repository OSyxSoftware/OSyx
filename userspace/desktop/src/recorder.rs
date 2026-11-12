//! Screen recording for PRISM: frames of the composed screen become a WebM
//! file with VP8 video (`osyx_media::vp8::encode`, `video::webm`).
//!
//! The compositor's loop only copies the screen, at most ten times a
//! second and only when something changed; a thread of its own encodes and
//! writes. When the encoder falls behind, frames are dropped, not queued:
//! each frame carries its time, so the film keeps its pace.

use os_std::fs::{self, File};
use os_std::io::SeekFrom;
use os_std::prelude::*;
use os_std::thread;
use os_std::time::Duration;
use osyx_media::video::webm;
use osyx_media::vp8::encode::Encoder;

/// The shortest time between two frames (ms).
pub const FRAME_MS: u64 = 100;
/// A still screen is recorded this often, so that players see time pass.
pub const STILL_MS: u64 = 1000;
/// A key frame at least this often, for seeking.
const KEY_FRAMES: u64 = 50;
/// VP8's quantizer index: fine enough that panels a few levels apart in a
/// dark theme stay apart (luma within 2 of the screen).
const QUALITY: u8 = 4;
/// Frames waiting for the encoder; more are dropped.
const QUEUE: usize = 2;

enum Job {
    Frame { ms: u64, pixels: Vec<u32> },
    Stop { ms: u64 },
}

/// How a recording ended.
pub struct Summary {
    pub path: String,
    pub frames: u64,
    pub bytes: u64,
    pub ms: u64,
}

#[derive(Default)]
struct Shared {
    jobs: Vec<Job>,
    result: Option<Result<Summary, String>>,
}

pub struct Recorder {
    shared: Arc<Mutex<Shared>>,
    /// When the recording started and when the last frame was taken (ms
    /// of the compositor's clock).
    started: u64,
    last: u64,
    stopping: bool,
    pub dropped: u64,
}

impl Recorder {
    /// Starts recording `width` x `height` frames into `path`.
    pub fn start(path: String, width: u32, height: u32, now: u64) -> Result<Recorder, String> {
        let encoder = Encoder::new(width as usize, height as usize, QUALITY).ok_or("the screen is too large to record")?;
        let (writer, head) = webm::Writer::start(width, height);
        let mut file = File::create(&path).map_err(|e| format!("{e}"))?;
        file.write_all(&head).map_err(|e| format!("{e}"))?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker = shared.clone();
        // Without a thread there is no recording, but PRISM lives on.
        let made = thread::Builder::new().spawn({
            let path = path.clone();
            move || {
                // The compositor's thread runs above the others so that
                // input and frames come first; encoding must not share
                // that place.
                let _ = os_std::system::thread_priority(0, os_std::system::priority::NORMAL);
                let result = work(&worker, encoder, writer, file, head.len() as u64, width as usize, &path);
                // Half a file is of no use to anyone.
                if result.is_err() {
                    let _ = fs::remove_file(&path);
                }
                worker.lock().unwrap().result = Some(result);
            }
        });
        if let Err(e) = made {
            let _ = fs::remove_file(&path);
            return Err(format!("no thread for the encoder: {e}"));
        }
        Ok(Recorder { shared, started: now, last: 0, stopping: false, dropped: 0 })
    }

    /// Whether a frame should be taken now: `changed` says that the screen
    /// changed since the last one.
    pub fn due(&self, now: u64, changed: bool) -> bool {
        let since = now.saturating_sub(self.started).saturating_sub(self.last);
        !self.stopping && (self.last == 0 || (changed && since >= FRAME_MS) || since >= STILL_MS)
    }

    /// Hands a copy of the screen to the encoder; `false` when the encoder
    /// is still busy with earlier frames and this one was dropped.
    pub fn frame(&mut self, now: u64, pixels: impl FnOnce() -> Vec<u32>) -> bool {
        let ms = now.saturating_sub(self.started).max(1);
        self.last = ms;
        if self.shared.lock().unwrap().jobs.len() >= QUEUE {
            self.dropped += 1;
            return false;
        }
        let pixels = pixels();
        self.shared.lock().unwrap().jobs.push(Job::Frame { ms, pixels });
        true
    }

    /// Asks the encoder to finish the file.
    pub fn stop(&mut self, now: u64) {
        if !self.stopping {
            self.stopping = true;
            self.shared.lock().unwrap().jobs.push(Job::Stop { ms: now.saturating_sub(self.started) });
        }
    }

    /// The outcome, once the file is finished or writing failed.
    pub fn finished(&mut self) -> Option<Result<Summary, String>> {
        self.shared.lock().unwrap().result.take()
    }
}

fn work(
    shared: &Mutex<Shared>,
    mut encoder: Encoder,
    mut writer: webm::Writer,
    mut file: File,
    mut bytes: u64,
    stride: usize,
    path: &str,
) -> Result<Summary, String> {
    let io = |e: Error| format!("{e}");
    loop {
        let job = {
            let mut s = shared.lock().unwrap();
            if s.jobs.is_empty() {
                None
            } else {
                Some(s.jobs.remove(0))
            }
        };
        match job {
            None => thread::sleep(Duration::from_millis(5)),
            Some(Job::Frame { ms, pixels }) => {
                let key = writer.frames() % KEY_FRAMES == 0;
                let packet = encoder.encode(&pixels, stride, key);
                drop(pixels);
                let out = writer.frame(ms, &packet.data, packet.key);
                file.write_all(&out).map_err(io)?;
                bytes += out.len() as u64;
            }
            Some(Job::Stop { ms }) => {
                let frames = writer.frames();
                let (tail, patches) = writer.finish(ms);
                file.write_all(&tail).map_err(io)?;
                bytes += tail.len() as u64;
                for p in patches {
                    file.seek(SeekFrom::Start(p.at)).map_err(io)?;
                    file.write_all(&p.bytes).map_err(io)?;
                }
                drop(file);
                if frames == 0 {
                    let _ = fs::remove_file(path);
                    return Err(String::from("nothing was recorded"));
                }
                return Ok(Summary { path: String::from(path), frames, bytes, ms });
            }
        }
    }
}
