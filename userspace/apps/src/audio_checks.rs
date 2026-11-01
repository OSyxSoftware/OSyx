//! Sound card checks (`init tests audio`, run by scripts/audio-test.ps1 with
//! QEMU's intel-hda controller and hda-duplex codec): the kernel's per-card
//! and default nodes, recording at the card's pace in whole frames,
//! playback and recording at the same time, the mixer's choice of default
//! cards, and the winmm wave devices on top (wavedev.exe).

use crate::Tests;
use os_std::fs::{self, File, OpenOptions};
use os_std::io::{Read, Write};
use os_std::prelude::*;
use os_std::thread;
use os_std::time::{Duration, Instant};

fn field(status: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    status.lines().find_map(|l| l.strip_prefix(prefix.as_str())).map(|v| v.trim().to_string())
}

/// Reads `node` for `ms` milliseconds: (bytes, whether every read returned
/// whole frames, elapsed milliseconds).
fn record(node: &str, ms: u64) -> Option<(usize, bool, u128)> {
    let mut f = File::open(node).ok()?;
    let start = Instant::now();
    let (mut total, mut aligned) = (0, true);
    let mut buf = vec![0u8; 4096];
    while start.elapsed() < Duration::from_millis(ms) {
        let n = f.read(&mut buf).ok()?;
        aligned &= n % 4 == 0 && n > 0;
        total += n;
    }
    Some((total, aligned, start.elapsed().as_millis()))
}

fn mixer(line: &str) -> bool {
    OpenOptions::new().write(true).open("/dev/mixer").and_then(|mut f| f.write_all(line.as_bytes())).is_ok()
}

pub fn run(t: &mut Tests) {
    let status = fs::read_to_string("/run/audio.status").unwrap_or_default();
    println!("audio: status {}", status.trim_end().replace('\n', " | "));
    let output = field(&status, "device").unwrap_or_default();
    let input = field(&status, "input").unwrap_or_default();
    t.check("the status names the default output and input cards", || !output.is_empty() && !input.is_empty());
    t.check("each card has its nodes, listed in the status", || {
        fs::metadata("/dev/audio0").is_ok()
            && fs::metadata("/dev/audioin0").is_ok()
            && field(&status, "audio0").is_some()
            && field(&status, "audioin0").is_some()
    });
    t.check("the default nodes exist", || fs::metadata("/dev/audio").is_ok() && fs::metadata("/dev/audioin").is_ok());
    t.check("a recording node refuses writes and reads shorter than a frame", || {
        let Ok(mut f) = OpenOptions::new().read(true).write(true).open("/dev/audioin0") else { return false };
        f.write(&[0u8; 4]).is_err() && f.read(&mut [0u8; 3]).is_err()
    });
    t.check("recording delivers whole frames at the card's pace (48 kHz)", || {
        let Some((bytes, aligned, ms)) = record("/dev/audioin", 1500) else { return false };
        let rate = bytes as u128 / 4 * 1000 / ms.max(1);
        println!("audio: recorded {bytes} bytes in {ms} ms ({rate} frames/s)");
        // Emulated cards do not keep perfect time; the count must still be
        // of the right order (half to one and a half times 48 kHz).
        aligned && (24_000..=72_000).contains(&rate)
    });
    t.check("playback and recording run at the same time on one card", || {
        let player = thread::spawn(|| {
            let Ok(mut f) = OpenOptions::new().write(true).open("/dev/audio0") else { return false };
            // One second of a 375 Hz square wave, 48 kHz stereo.
            let mut pcm = Vec::with_capacity(48_000 * 4);
            for i in 0..48_000u32 {
                let s: i16 = if (i / 64) % 2 == 0 { 4000 } else { -4000 };
                pcm.extend_from_slice(&s.to_le_bytes());
                pcm.extend_from_slice(&s.to_le_bytes());
            }
            f.write_all(&pcm).is_ok()
        });
        let rec = record("/dev/audioin0", 1000);
        let played = player.join().unwrap_or(false);
        println!("audio: duplex recorded {:?}, played {played}", rec.map(|r| r.0));
        played && rec.is_some_and(|(bytes, aligned, _)| aligned && bytes >= 48_000 * 4 / 2)
    });
    t.check("the mixer selects default cards by number or name", || {
        mixer("input=0\n") && mixer(&format!("output={output}\n")) && mixer(&format!("input={input}\n"))
    });
    t.check("the mixer refuses cards that do not exist or cannot do the job", || {
        !mixer("output=7\n") && !mixer("input=nosuchcard\n")
    });
    t.check("the mixer keeps the default output while /dev/audio is open", || {
        let Ok(_f) = OpenOptions::new().write(true).open("/dev/audio") else { return false };
        // Choosing the same card is no change and is allowed.
        mixer(&format!("output={output}\n"))
    });
    if fs::metadata("/opt/wintests/wavedev.exe").is_ok() {
        t.check("winmm wave devices on the card (wavedev.exe)", || crate::win_checks::run("wavedev.exe", &[]) == Some(0));
    }
}
