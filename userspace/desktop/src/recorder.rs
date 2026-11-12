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

use crate::user_save;

/// Where a recording is written while it is made: a folder only root can
/// reach. The finished file is handed to the desktop user (`user_save`);
/// root writes nothing in the user's home.
const SPOOL: &str = "/var/cache/osyx-prism";
/// A finished recording that could not be handed over yet is kept in the
/// spool under this prefix and tried again after every later recording.
const KEPT: &str = "kept ";

/// Hands the kept recordings to the user (`dir` is their Videos folder).
/// Returns the error for `name` if that one is still kept.
fn deliver_kept(dir: &str, name: &str) -> Result<(), String> {
    let mut mine = Ok(());
    let Ok(entries) = fs::read_dir(SPOOL) else { return mine };
    for entry in entries {
        let Some(as_name) = entry.file_name().strip_prefix(KEPT).map(String::from) else { continue };
        let path = format!("{SPOOL}/{}", entry.file_name());
        match user_save::save(dir, &as_name, user_save::Source::File(&path)) {
            Ok(()) => {
                let _ = fs::remove_file(&path);
                if as_name != name {
                    println!("prism: an earlier recording was saved to {dir}/{as_name}");
                }
            }
            Err(e) if as_name == name => mine = Err(format!("{e}. The recording is kept and will be saved after the next recording that can be")),
            Err(_) => {}
        }
    }
    mine
}

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
    /// Starts recording `width` x `height` frames; the finished film
    /// becomes the desktop user's file `dir/name`.
    pub fn start(dir: String, name: String, width: u32, height: u32, now: u64) -> Result<Recorder, String> {
        let encoder = Encoder::new(width as usize, height as usize, QUALITY).ok_or("the screen is too large to record")?;
        let (writer, head) = webm::Writer::start(width, height);
        let io = |e: Error| format!("{SPOOL}: {e}");
        fs::create_dir_all(SPOOL).map_err(io)?;
        fs::set_permissions(SPOOL, fs::Permissions::from_mode(0o700)).map_err(io)?;
        // What an earlier PRISM left half written when it died; finished
        // recordings that wait for their folder stay.
        for old in fs::read_dir(SPOOL).map_err(io)? {
            if !old.file_name().starts_with(KEPT) {
                let _ = fs::remove_file(old.path());
            }
        }
        let spool = format!("{SPOOL}/{name}");
        let mut file = File::options().read(true).write(true).create_new(true).mode(0o600).open(&spool).map_err(io)?;
        if let Err(e) = file.write_all(&head) {
            let _ = fs::remove_file(&spool);
            return Err(io(e));
        }
        let shared = Arc::new(Mutex::new(Shared::default()));
        let worker = shared.clone();
        // Without a thread there is no recording, but PRISM lives on.
        let made = thread::Builder::new().spawn({
            let spool = spool.clone();
            move || {
                // The compositor's thread runs above the others so that
                // input and frames come first; encoding must not share
                // that place.
                let _ = os_std::system::thread_priority(0, os_std::system::priority::NORMAL);
                let result = work(&worker, encoder, writer, file, head.len() as u64, width as usize).and_then(|mut summary| {
                    // Finished: kept until the user has it, also when
                    // their folder cannot be written today.
                    fs::rename(&spool, format!("{SPOOL}/{KEPT}{name}")).map_err(|e| format!("{SPOOL}: {e}"))?;
                    deliver_kept(&dir, &name)?;
                    summary.path = format!("{dir}/{name}");
                    Ok(summary)
                });
                // Half a file is of no use to anyone (a finished one has
                // another name by now).
                let _ = fs::remove_file(&spool);
                worker.lock().unwrap().result = Some(result);
            }
        });
        if let Err(e) = made {
            let _ = fs::remove_file(&spool);
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
                file.sync_all().map_err(io)?;
                drop(file);
                if frames == 0 {
                    return Err(String::from("nothing was recorded"));
                }
                return Ok(Summary { path: String::new(), frames, bytes, ms });
            }
        }
    }
}
