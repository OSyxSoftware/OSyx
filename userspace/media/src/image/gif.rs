//! GIF (87a and 89a): LZW image data, global and local colour tables,
//! transparency, interlacing and animation with frame disposal.

use super::{check_size, Frame, Picture, MAX_FRAMES};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

fn le16(d: &[u8], p: usize) -> usize {
    u16::from_le_bytes([d[p], d[p + 1]]) as usize
}

/// Concatenated data sub-blocks starting at `*p`.
fn sub_blocks(d: &[u8], p: &mut usize) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(&n) = d.get(*p) {
        *p += 1;
        if n == 0 {
            break;
        }
        let end = (*p + n as usize).min(d.len());
        out.extend_from_slice(&d[*p..end]);
        *p = end;
    }
    out
}

/// GIF LZW (least significant bit first, codes of min_size+1 to 12 bits).
fn lzw(data: &[u8], min_size: u8, want: usize) -> Vec<u8> {
    let min_size = min_size.clamp(2, 11) as u32;
    let clear = 1u32 << min_size;
    let end = clear + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    for i in 0..clear {
        suffix[i as usize] = i as u8;
        first[i as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(want);
    let mut size = min_size + 1;
    let mut next = end + 1;
    let mut prev: Option<u32> = None;
    let (mut acc, mut nbits) = (0u32, 0u32);
    let mut stack = Vec::with_capacity(4096);
    for &b in data {
        acc |= (b as u32) << nbits;
        nbits += 8;
        while nbits >= size {
            let code = acc & ((1 << size) - 1);
            acc >>= size;
            nbits -= size;
            if code == clear {
                size = min_size + 1;
                next = end + 1;
                prev = None;
                continue;
            }
            if code == end || out.len() >= want {
                return out;
            }
            let Some(p) = prev else {
                if code < clear {
                    out.push(code as u8);
                    prev = Some(code);
                }
                continue;
            };
            let (emit, head) = if code < next {
                (code, first[code as usize])
            } else if code == next {
                (p, first[p as usize])
            } else {
                return out;
            };
            // Output the string for `emit`, then the extra byte for KwKwK.
            stack.clear();
            let mut c = emit;
            loop {
                stack.push(suffix[c as usize]);
                if c < clear {
                    break;
                }
                c = prefix[c as usize] as u32;
            }
            out.extend(stack.iter().rev());
            if code == next {
                out.push(head);
            }
            if next < 4096 {
                prefix[next as usize] = p as u16;
                suffix[next as usize] = head;
                first[next as usize] = first[p as usize];
                next += 1;
                if next == 1 << size && size < 12 {
                    size += 1;
                }
            }
            prev = Some(code);
        }
    }
    out
}

pub fn decode(d: &[u8]) -> Result<Picture, String> {
    if d.len() < 13 {
        return Err(String::from("GIF too short"));
    }
    let (w, h) = (le16(d, 6), le16(d, 8));
    let n = check_size(w, h)?;
    let flags = d[10];
    let bg = d[11] as usize;
    let mut p = 13;
    let mut global: Vec<[u8; 3]> = Vec::new();
    if flags & 0x80 != 0 {
        let count = 2 << (flags & 7);
        let t = d.get(p..p + 3 * count).ok_or("GIF colour table cut short")?;
        global = t.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
        p += 3 * count;
    }
    let _ = bg;
    let mut canvas = vec![[0u8; 4]; n];
    let mut frames: Vec<Frame> = Vec::new();
    let (mut delay, mut transparent, mut disposal) = (0u32, None::<u8>, 0u8);
    while p < d.len() && frames.len() < MAX_FRAMES {
        match d[p] {
            0x21 => {
                let label = *d.get(p + 1).unwrap_or(&0);
                p += 2;
                if label == 0xF9 && d.get(p) == Some(&4) {
                    let g = &d[p + 1..(p + 5).min(d.len())];
                    if g.len() == 4 {
                        disposal = (g[0] >> 2) & 7;
                        delay = le16(g, 1) as u32 * 10;
                        transparent = (g[0] & 1 != 0).then_some(g[3]);
                    }
                }
                sub_blocks(d, &mut p);
            }
            0x2C => {
                let id = d.get(p..p + 10).ok_or("GIF image descriptor cut short")?;
                let (fx, fy, fw, fh) = (le16(id, 1), le16(id, 3), le16(id, 5), le16(id, 7));
                let iflags = id[9];
                p += 10;
                let mut table = &global;
                let local: Vec<[u8; 3]>;
                if iflags & 0x80 != 0 {
                    let count = 2 << (iflags & 7);
                    let t = d.get(p..p + 3 * count).ok_or("GIF colour table cut short")?;
                    local = t.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
                    p += 3 * count;
                    table = &local;
                }
                let min = *d.get(p).ok_or("GIF image data cut short")?;
                p += 1;
                let data = sub_blocks(d, &mut p);
                let idx = lzw(&data, min, fw * fh);
                // The rows of an interlaced frame come in four passes.
                let order: Vec<usize> = if iflags & 0x40 != 0 {
                    (0..fh)
                        .step_by(8)
                        .chain((4..fh).step_by(8))
                        .chain((2..fh).step_by(4))
                        .chain((1..fh).step_by(2))
                        .collect()
                } else {
                    (0..fh).collect()
                };
                let before = (disposal == 3).then(|| canvas.clone());
                for (row_i, &row) in order.iter().enumerate() {
                    let y = fy + row;
                    if y >= h {
                        continue;
                    }
                    for col in 0..fw {
                        let x = fx + col;
                        let Some(&k) = idx.get(row_i * fw + col) else { break };
                        if x >= w || Some(k) == transparent {
                            continue;
                        }
                        let c = table.get(k as usize).copied().unwrap_or([0; 3]);
                        canvas[y * w + x] = [c[0], c[1], c[2], 255];
                    }
                }
                frames.push(Frame { px: canvas.clone(), delay_ms: if delay < 20 { 100 } else { delay } });
                // Disposal before the next frame.
                match disposal {
                    2 => {
                        for y in fy..(fy + fh).min(h) {
                            for x in fx..(fx + fw).min(w) {
                                canvas[y * w + x] = [0; 4];
                            }
                        }
                    }
                    3 => {
                        if let Some(b) = before {
                            canvas = b;
                        }
                    }
                    _ => {}
                }
                delay = 0;
                transparent = None;
                disposal = 0;
            }
            0x3B => break,
            _ => break,
        }
    }
    if frames.is_empty() {
        return Err(String::from("GIF without images"));
    }
    if frames.len() == 1 {
        frames[0].delay_ms = 0;
    }
    let format = format!("GIF{}", if frames.len() > 1 { format!(", {} frames", frames.len()) } else { String::new() });
    Ok(Picture { w, h, frames, format })
}
