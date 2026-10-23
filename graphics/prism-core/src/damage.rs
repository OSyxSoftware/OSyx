//! Damage accumulation: the screen areas that must be recomposed.
//!
//! A short list of rectangles: a new rectangle that overlaps or nearly
//! touches an existing one is merged when the union wastes little area;
//! past [`MAX_RECTS`] everything collapses into the bounding box.

use alloc::vec::Vec;
use osyx_gfx::Rect;

pub const MAX_RECTS: usize = 16;

#[derive(Clone, Debug, Default)]
pub struct Damage {
    rects: Vec<Rect>,
}

fn area(r: &Rect) -> u64 {
    r.w as u64 * r.h as u64
}

impl Damage {
    pub fn new() -> Damage {
        Damage::default()
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    pub fn rects(&self) -> &[Rect] {
        &self.rects
    }

    /// Adds `r` clipped to `bounds`.
    pub fn add(&mut self, r: Rect, bounds: Rect) {
        let mut r = r.intersect(&bounds);
        if r.is_empty() {
            return;
        }
        // Absorb existing rectangles while the union stays cheap.
        loop {
            let mut merged = false;
            let mut i = 0;
            while i < self.rects.len() {
                let o = self.rects[i];
                let u = r.union(&o);
                if o.intersect(&r) == r {
                    return; // already covered
                }
                if area(&u) <= area(&r) + area(&o) + area(&u) / 8 {
                    r = u;
                    self.rects.swap_remove(i);
                    merged = true;
                } else {
                    i += 1;
                }
            }
            if !merged {
                break;
            }
        }
        self.rects.push(r);
        if self.rects.len() > MAX_RECTS {
            let all = self.bounding_box();
            self.rects.clear();
            self.rects.push(all);
        }
    }

    pub fn bounding_box(&self) -> Rect {
        self.rects.iter().fold(Rect::EMPTY, |a, r| a.union(r))
    }

    pub fn take(&mut self) -> Vec<Rect> {
        core::mem::take(&mut self.rects)
    }

    /// Total damaged pixels (rectangles never overlap after merging
    /// heuristics, so this is an upper bound when they do).
    pub fn pixels(&self) -> u64 {
        self.rects.iter().map(area).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 1000, 800);

    #[test]
    fn clips_and_ignores_empty() {
        let mut d = Damage::new();
        d.add(Rect::new(-50, -50, 100, 100), SCREEN);
        assert_eq!(d.rects(), &[Rect::new(0, 0, 50, 50)]);
        d.add(Rect::new(2000, 0, 10, 10), SCREEN);
        d.add(Rect::new(10, 10, 0, 5), SCREEN);
        assert_eq!(d.rects().len(), 1);
    }

    #[test]
    fn merges_overlapping_and_contained() {
        let mut d = Damage::new();
        d.add(Rect::new(0, 0, 100, 100), SCREEN);
        d.add(Rect::new(10, 10, 20, 20), SCREEN);
        assert_eq!(d.rects(), &[Rect::new(0, 0, 100, 100)], "contained rect adds nothing");
        d.add(Rect::new(50, 0, 100, 100), SCREEN);
        assert_eq!(d.rects(), &[Rect::new(0, 0, 150, 100)], "heavy overlap merges");
        // Far apart: kept separate.
        d.add(Rect::new(800, 700, 10, 10), SCREEN);
        assert_eq!(d.rects().len(), 2);
        assert_eq!(d.pixels(), 150 * 100 + 100);
        // A big rect covering both swallows them.
        d.add(SCREEN, SCREEN);
        assert_eq!(d.rects(), &[SCREEN]);
    }

    #[test]
    fn collapses_beyond_the_limit() {
        let mut d = Damage::new();
        for i in 0..(MAX_RECTS as i32 + 4) {
            d.add(Rect::new(i * 60, (i % 2) * 400, 5, 5), SCREEN);
        }
        assert_eq!(d.rects().len(), 1);
        let b = d.bounding_box();
        assert!(b.w >= 900 && b.h >= 400);
        assert_eq!(d.take().len(), 1);
        assert!(d.is_empty());
    }
}
