//! Content-Aware Fill's patch synthesis: the hole is rebuilt from
//! patches of the picture around it, so texture and structure continue
//! into it rather than a blur of its surroundings.
//!
//! The method is the one Photoshop's own fill descends from: PatchMatch
//! (Barnes, Shechtman, Finkelstein & Goldman, "PatchMatch: A Randomized
//! Correspondence Algorithm for Structural Image Editing", SIGGRAPH
//! 2009) inside the coarse-to-fine, vote-and-refine loop of Wexler,
//! Shechtman & Irani ("Space-Time Completion of Video", PAMI 2007). At
//! every level of an image pyramid, each patch overlapping the hole is
//! matched to a patch drawn wholly from the sampling area — by random
//! initialisation, propagation from neighbours, and random search — and
//! every hole pixel takes the average of what the patches covering it
//! propose; the match then improves against the new estimate, and the
//! result seeds the next finer level. Source patches may be mirrored
//! and rotated by quarter turns (Photoshop's Mirror and Rotation
//! Adaptation), and the finished fill can be blended into its border by
//! solving Poisson's equation over the hole (Pérez, Gangnet & Blake,
//! "Poisson Image Editing", SIGGRAPH 2003 — Photoshop's Color
//! Adaptation).

use crate::progress::Progress;

/// Bytes per pixel: RGBA, all four synthesised.
pub const CHANNELS: usize = 4;

/// What no valid source means: not one full patch of unselected,
/// sampling-area pixels exists, so there is nothing to synthesise from.
pub const NO_SOURCE: &str =
    "Content-Aware Fill has nothing to sample from: no patch of unselected pixels fits.";

/// Rotation Adaptation: how source patches may turn. Photoshop's five
/// settings grade a continuous angle; this fill's patches turn by
/// quarter turns, so the settings map onto none, a half turn, or any
/// quarter turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Rotation {
    None,
    Half,
    Quarter,
}

/// The fill's settings.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Patch side, odd; 7 is PatchMatch's own default.
    pub patch: usize,
    /// Propagation-and-search passes per round.
    pub iterations: usize,
    /// Match-then-vote rounds per pyramid level.
    pub rounds: usize,
    /// Mirror: source patches may be flipped left-for-right.
    pub mirror: bool,
    pub rotation: Rotation,
    /// Color Adaptation: blend the fill into its border by Poisson's equation.
    pub color_adaptation: bool,
    pub seed: u64,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            patch: 7,
            iterations: 4,
            rounds: 3,
            mirror: false,
            rotation: Rotation::None,
            color_adaptation: true,
            seed: 1,
        }
    }
}

/// A transform of a source patch: bits 0–1 the quarter turns, bit 2
/// the mirror. `(dx, dy)` in the target patch reads the source at
/// `centre + transform(t, dx, dy)`.
fn transform(t: u8, dx: i32, dy: i32) -> (i32, i32) {
    let x = if t & 4 != 0 { -dx } else { dx };
    match t & 3 {
        0 => (x, dy),
        1 => (-dy, x),
        2 => (-x, -dy),
        _ => (dy, -x),
    }
}

fn allowed_transforms(options: &Options) -> Vec<u8> {
    let turns: &[u8] = match options.rotation {
        Rotation::None => &[0],
        Rotation::Half => &[0, 2],
        Rotation::Quarter => &[0, 1, 2, 3],
    };
    let mut out: Vec<u8> = turns.to_vec();
    if options.mirror {
        out.extend(turns.iter().map(|t| t | 4));
    }
    out
}

/// A match: the source patch's centre and its transform.
type Match = (i32, i32, u8);

/// A small deterministic generator (splitmix64), so a seed reproduces a fill.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
    fn range(&mut self, radius: i32) -> i32 {
        self.below((2 * radius + 1) as usize) as i32 - radius
    }
}

/// One pyramid level.
struct Level {
    w: usize,
    h: usize,
    img: Vec<f32>,
    hole: Vec<bool>,
    /// A pixel any source patch may include: in the sampling area, not in the hole.
    source_ok: Vec<bool>,
    /// A centre whose whole patch is `source_ok` and inside the image.
    valid: Vec<bool>,
    valid_list: Vec<usize>,
    /// Pixels whose patch overlaps the hole: the ones that get a match.
    targets: Vec<usize>,
    is_target: Vec<bool>,
}

fn box_count(mask: &[bool], w: usize, h: usize) -> Vec<u32> {
    // Summed-area table of `mask` as 0/1, (w + 1) × (h + 1).
    let mut sat = vec![0u32; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0u32;
        for x in 0..w {
            row += mask[y * w + x] as u32;
            sat[(y + 1) * (w + 1) + x + 1] = sat[y * (w + 1) + x + 1] + row;
        }
    }
    sat
}

fn window_sum(sat: &[u32], w: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> u32 {
    // Inclusive-exclusive [x0, x1) × [y0, y1).
    let s = w + 1;
    sat[y1 * s + x1] + sat[y0 * s + x0] - sat[y0 * s + x1] - sat[y1 * s + x0]
}

impl Level {
    fn finish(
        w: usize,
        h: usize,
        img: Vec<f32>,
        hole: Vec<bool>,
        source_ok: Vec<bool>,
        r: usize,
    ) -> Level {
        let bad: Vec<bool> = source_ok.iter().map(|ok| !ok).collect();
        let bad_sat = box_count(&bad, w, h);
        let hole_sat = box_count(&hole, w, h);
        let mut valid = vec![false; w * h];
        let mut valid_list = Vec::new();
        let mut is_target = vec![false; w * h];
        let mut targets = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let idx = y * w + x;
                if x >= r && y >= r && x + r < w && y + r < h {
                    let clean = window_sum(&bad_sat, w, x - r, y - r, x + r + 1, y + r + 1) == 0;
                    if clean {
                        valid[idx] = true;
                        valid_list.push(idx);
                    }
                }
                let (x0, y0) = (x.saturating_sub(r), y.saturating_sub(r));
                let (x1, y1) = ((x + r + 1).min(w), (y + r + 1).min(h));
                if window_sum(&hole_sat, w, x0, y0, x1, y1) > 0 {
                    is_target[idx] = true;
                    targets.push(idx);
                }
            }
        }
        Level {
            w,
            h,
            img,
            hole,
            source_ok,
            valid,
            valid_list,
            targets,
            is_target,
        }
    }

    /// The next coarser level: 2×2 boxes, a box that touches the hole
    /// being hole, a box wholly in the sampling area staying in it.
    fn coarser(&self, r: usize) -> Level {
        let (w, h) = (self.w / 2, self.h / 2);
        let mut img = vec![0.0f32; w * h * CHANNELS];
        let mut hole = vec![false; w * h];
        let mut source_ok = vec![true; w * h];
        for y in 0..h {
            for x in 0..w {
                let idx = y * w + x;
                let mut sum = [0.0f32; CHANNELS];
                let mut count = 0.0;
                for (cx, cy) in [
                    (2 * x, 2 * y),
                    (2 * x + 1, 2 * y),
                    (2 * x, 2 * y + 1),
                    (2 * x + 1, 2 * y + 1),
                ] {
                    let child = cy * self.w + cx;
                    if self.hole[child] {
                        hole[idx] = true;
                    } else {
                        for (c, total) in sum.iter_mut().enumerate() {
                            *total += self.img[child * CHANNELS + c];
                        }
                        count += 1.0;
                    }
                    if !self.source_ok[child] {
                        source_ok[idx] = false;
                    }
                }
                if count > 0.0 {
                    for c in 0..CHANNELS {
                        img[idx * CHANNELS + c] = sum[c] / count;
                    }
                }
            }
        }
        Level::finish(w, h, img, hole, source_ok, r)
    }

    /// Fills the hole by diffusion from its border, for the coarsest
    /// level's first estimate.
    fn diffuse(&mut self) {
        let (w, h) = (self.w, self.h);
        let mut filled: Vec<bool> = self.hole.iter().map(|hole| !hole).collect();
        loop {
            let mut next = Vec::new();
            for y in 0..h {
                for x in 0..w {
                    let idx = y * w + x;
                    if filled[idx] {
                        continue;
                    }
                    let mut sum = [0.0f32; CHANNELS];
                    let mut count = 0.0;
                    for (nx, ny) in neighbours(x, y, w, h) {
                        let n = ny * w + nx;
                        if filled[n] {
                            for (c, total) in sum.iter_mut().enumerate() {
                                *total += self.img[n * CHANNELS + c];
                            }
                            count += 1.0;
                        }
                    }
                    if count > 0.0 {
                        next.push((idx, sum.map(|v| v / count)));
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            for (idx, value) in next {
                self.img[idx * CHANNELS..(idx + 1) * CHANNELS].copy_from_slice(&value);
                filled[idx] = true;
            }
        }
    }

    /// Squared difference between the patch at target `p` and the
    /// transformed source patch at `m`, stopping past `cutoff`.
    fn distance(&self, r: i32, p: usize, m: Match, cutoff: f32) -> f32 {
        let (w, h) = (self.w as i32, self.h as i32);
        let (px, py) = ((p % self.w) as i32, (p / self.w) as i32);
        let mut sum = 0.0f32;
        for dy in -r..=r {
            for dx in -r..=r {
                let tx = (px + dx).clamp(0, w - 1);
                let ty = (py + dy).clamp(0, h - 1);
                let (sx, sy) = transform(m.2, dx, dy);
                let s = ((m.1 + sy) * w + m.0 + sx) as usize * CHANNELS;
                let t = (ty * w + tx) as usize * CHANNELS;
                for c in 0..CHANNELS {
                    let d = self.img[t + c] - self.img[s + c];
                    sum += d * d;
                }
            }
            if sum > cutoff {
                return sum;
            }
        }
        sum
    }

    fn is_valid(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < self.w
            && (y as usize) < self.h
            && self.valid[y as usize * self.w + x as usize]
    }

    fn random_match(&self, rng: &mut Rng, transforms: &[u8]) -> Match {
        let idx = self.valid_list[rng.below(self.valid_list.len())];
        (
            (idx % self.w) as i32,
            (idx / self.w) as i32,
            transforms[rng.below(transforms.len())],
        )
    }

    /// One PatchMatch pass: propagation along the scan (forward on even
    /// passes, backward on odd) and random search around the current match.
    fn improve(
        &self,
        r: i32,
        nnf: &mut [Match],
        dist: &mut [f32],
        pass: usize,
        rng: &mut Rng,
        transforms: &[u8],
    ) {
        let forward = pass % 2 == 0;
        let order: Vec<usize> = if forward {
            self.targets.clone()
        } else {
            self.targets.iter().rev().copied().collect()
        };
        let radius0 = self.w.max(self.h) as i32;
        for &p in &order {
            let (px, py) = ((p % self.w) as i32, (p / self.w) as i32);
            let mut best = nnf[p];
            let mut best_d = dist[p];
            let consider = |cand: Match, best: &mut Match, best_d: &mut f32| {
                if !self.is_valid(cand.0, cand.1) {
                    return;
                }
                let d = self.distance(r, p, cand, *best_d);
                if d < *best_d {
                    *best_d = d;
                    *best = cand;
                }
            };
            let step = if forward { -1 } else { 1 };
            for (nx, ny) in [(px + step, py), (px, py + step)] {
                if nx < 0 || ny < 0 || nx as usize >= self.w || ny as usize >= self.h {
                    continue;
                }
                let n = ny as usize * self.w + nx as usize;
                if !self.is_target[n] {
                    continue;
                }
                let (mx, my, t) = nnf[n];
                let (ox, oy) = transform(t, px - nx, py - ny);
                consider((mx + ox, my + oy, t), &mut best, &mut best_d);
            }
            let mut radius = radius0;
            while radius >= 1 {
                let t = if rng.below(4) == 0 {
                    transforms[rng.below(transforms.len())]
                } else {
                    best.2
                };
                let cand = (best.0 + rng.range(radius), best.1 + rng.range(radius), t);
                consider(cand, &mut best, &mut best_d);
                radius /= 2;
            }
            nnf[p] = best;
            dist[p] = best_d;
        }
    }

    /// Every hole pixel becomes the mean of what the patches covering it
    /// propose. Returns the same proposals for every covered pixel, hole
    /// or not — the fill as it would continue past its border, which
    /// Color Adaptation's guidance needs.
    fn vote(&mut self, r: i32, nnf: &[Match]) -> Vec<f32> {
        let (w, h) = (self.w as i32, self.h as i32);
        let mut acc = vec![0.0f32; self.w * self.h * CHANNELS];
        let mut count = vec![0u32; self.w * self.h];
        for &p in &self.targets {
            let (px, py) = ((p % self.w) as i32, (p / self.w) as i32);
            let (mx, my, t) = nnf[p];
            for dy in -r..=r {
                for dx in -r..=r {
                    let (qx, qy) = (px + dx, py + dy);
                    if qx < 0 || qy < 0 || qx >= w || qy >= h {
                        continue;
                    }
                    let q = (qy * w + qx) as usize;
                    let (sx, sy) = transform(t, dx, dy);
                    let s = ((my + sy) * w + mx + sx) as usize * CHANNELS;
                    for c in 0..CHANNELS {
                        acc[q * CHANNELS + c] += self.img[s + c];
                    }
                    count[q] += 1;
                }
            }
        }
        let mut proposals = self.img.clone();
        for q in 0..self.w * self.h {
            if count[q] > 0 {
                for c in 0..CHANNELS {
                    proposals[q * CHANNELS + c] = acc[q * CHANNELS + c] / count[q] as f32;
                }
                if self.hole[q] {
                    self.img[q * CHANNELS..(q + 1) * CHANNELS]
                        .copy_from_slice(&proposals[q * CHANNELS..(q + 1) * CHANNELS]);
                }
            }
        }
        proposals
    }

    /// Color Adaptation: the fill keeps its own gradients — `guide`, the
    /// fill as it continues past its border — but takes the border's
    /// colours: Poisson's equation over the hole, by Gauss–Seidel sweeps.
    fn poisson_blend(&mut self, guide: &[f32]) {
        let (w, h) = (self.w, self.h);
        let hole_pixels: Vec<usize> = (0..w * h).filter(|&i| self.hole[i]).collect();
        for _ in 0..2000 {
            let mut max_delta = 0.0f32;
            for &p in &hole_pixels {
                let (x, y) = (p % w, p / w);
                let ns = neighbours(x, y, w, h);
                if ns.is_empty() {
                    continue;
                }
                for c in 0..CHANNELS {
                    let mut sum = 0.0f32;
                    for &(nx, ny) in &ns {
                        let n = ny * w + nx;
                        sum += self.img[n * CHANNELS + c] + guide[p * CHANNELS + c]
                            - guide[n * CHANNELS + c];
                    }
                    let value = sum / ns.len() as f32;
                    max_delta = max_delta.max((value - self.img[p * CHANNELS + c]).abs());
                    self.img[p * CHANNELS + c] = value;
                }
            }
            if max_delta < 0.05 {
                break;
            }
        }
    }
}

fn neighbours(x: usize, y: usize, w: usize, h: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(4);
    if x > 0 {
        out.push((x - 1, y));
    }
    if x + 1 < w {
        out.push((x + 1, y));
    }
    if y > 0 {
        out.push((x, y - 1));
    }
    if y + 1 < h {
        out.push((x, y + 1));
    }
    out
}

/// Every pixel within Chebyshev distance `radius` of a `mask` pixel —
/// the Sampling Area as a margin around the selection.
pub fn within(mask: &[bool], width: u32, height: u32, radius: u32) -> Vec<bool> {
    let (w, h, r) = (width as usize, height as usize, radius as usize);
    let sat = box_count(mask, w, h);
    (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            let (x0, y0) = (x.saturating_sub(r), y.saturating_sub(r));
            let (x1, y1) = ((x + r + 1).min(w), (y + r + 1).min(h));
            window_sum(&sat, w, x0, y0, x1, y1) > 0
        })
        .collect()
}

/// Fills every `hole` pixel of `pixels` (RGBA, `width × height`) from
/// patches of the pixels `sampling` allows (every non-hole pixel when
/// `None`), reporting each round to `progress` under "Content-Aware
/// Fill". Errors with [`NO_SOURCE`] when no full patch of allowed
/// pixels exists, and for mismatched buffers or an even patch size.
pub fn inpaint(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    hole: &[bool],
    sampling: Option<&[bool]>,
    options: &Options,
    progress: &mut dyn Progress,
) -> Result<(), String> {
    let (w, h) = (width as usize, height as usize);
    if pixels.len() != w * h * CHANNELS || hole.len() != w * h {
        return Err("Content-Aware Fill was given mismatched buffers.".to_string());
    }
    if let Some(sampling) = sampling {
        if sampling.len() != w * h {
            return Err("Content-Aware Fill was given a mismatched sampling area.".to_string());
        }
    }
    if options.patch % 2 == 0 || options.patch < 3 {
        return Err("The patch size is odd and at least 3.".to_string());
    }
    if !hole.iter().any(|&b| b) {
        return Ok(());
    }
    let r = options.patch / 2;
    let img: Vec<f32> = pixels.iter().map(|&b| b as f32).collect();
    let source_ok: Vec<bool> = (0..w * h)
        .map(|i| {
            !hole[i]
                && match sampling {
                    Some(s) => s[i],
                    None => true,
                }
        })
        .collect();
    let base = Level::finish(w, h, img, hole.to_vec(), source_ok, r);
    if base.valid_list.is_empty() {
        return Err(NO_SOURCE.to_string());
    }
    // The pyramid, finest first; a level stops it when it gets too small
    // for a patch or loses every valid source.
    let mut levels = vec![base];
    while levels.len() < 5 {
        let last = levels.last().unwrap();
        if last.w / 2 < 2 * options.patch || last.h / 2 < 2 * options.patch {
            break;
        }
        let next = last.coarser(r);
        if next.valid_list.is_empty() || next.targets.is_empty() {
            break;
        }
        levels.push(next);
    }
    let transforms = allowed_transforms(options);
    let mut rng = Rng(options.seed ^ 0xD1B5_4A32_D192_ED03);
    let total_rounds = levels.len() * options.rounds;
    let mut done = 0;
    let ri = r as i32;
    let mut coarse_nnf: Option<(Vec<Match>, usize, usize)> = None;
    let mut guide = Vec::new();
    for li in (0..levels.len()).rev() {
        let level = &mut levels[li];
        let n = level.w * level.h;
        let mut nnf = vec![(0i32, 0i32, 0u8); n];
        let mut dist = vec![f32::INFINITY; n];
        match &coarse_nnf {
            None => {
                level.diffuse();
                for &p in &level.targets {
                    nnf[p] = level.random_match(&mut rng, &transforms);
                }
            }
            Some((coarse, cw, _)) => {
                for &p in &level.targets {
                    let (px, py) = (p % level.w, p / level.w);
                    let (cx, cy) = (px / 2, py / 2);
                    let (mx, my, t) = coarse[cy * cw + cx];
                    // Which child of the coarse source this child of the
                    // coarse target reads: the transform applied about the
                    // block's centre, so a mirrored or turned block's
                    // children swap places too.
                    let (hx, hy) = transform(
                        t,
                        2 * (px - 2 * cx) as i32 - 1,
                        2 * (py - 2 * cy) as i32 - 1,
                    );
                    let cand = (2 * mx + (hx + 1) / 2, 2 * my + (hy + 1) / 2, t);
                    nnf[p] = if level.is_valid(cand.0, cand.1) {
                        cand
                    } else {
                        level.random_match(&mut rng, &transforms)
                    };
                }
                level.vote(ri, &nnf);
            }
        }
        for _ in 0..options.rounds {
            for p in &level.targets {
                dist[*p] = level.distance(ri, *p, nnf[*p], f32::INFINITY);
            }
            for pass in 0..options.iterations {
                level.improve(ri, &mut nnf, &mut dist, pass, &mut rng, &transforms);
            }
            guide = level.vote(ri, &nnf);
            done += 1;
            progress.report("Content-Aware Fill", done, total_rounds)?;
        }
        coarse_nnf = Some((nnf, level.w, level.h));
    }
    let base = &mut levels[0];
    if options.color_adaptation {
        base.poisson_blend(&guide);
    }
    for q in 0..w * h {
        if hole[q] {
            for c in 0..CHANNELS {
                pixels[q * CHANNELS + c] =
                    base.img[q * CHANNELS + c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::{Recorder, Silent, CANCELLED};

    fn image(w: usize, h: usize, f: impl Fn(usize, usize) -> [u8; 4]) -> Vec<u8> {
        let mut out = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&f(x, y));
            }
        }
        out
    }

    fn block(w: usize, h: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> Vec<bool> {
        (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                x >= x0 && x < x1 && y >= y0 && y < y1
            })
            .collect()
    }

    fn mean_error(a: &[u8], b: &[u8], hole: &[bool]) -> f64 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for (i, &h) in hole.iter().enumerate() {
            if h {
                for c in 0..3 {
                    sum += (a[i * 4 + c] as f64 - b[i * 4 + c] as f64).abs();
                    n += 1.0;
                }
            }
        }
        sum / n
    }

    /// The old fill for comparison: every hole pixel the mean of the
    /// original pixels on the ring two out.
    fn ring_mean_fill(truth: &[u8], w: usize, h: usize, hole: &[bool]) -> Vec<u8> {
        let mut out = truth.to_vec();
        for (i, &is_hole) in hole.iter().enumerate() {
            if !is_hole {
                continue;
            }
            let (x, y) = ((i % w) as i32, (i / w) as i32);
            let mut sum = [0u32; 4];
            let mut n = 0;
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    if dx.abs() != 2 && dy.abs() != 2 {
                        continue;
                    }
                    let sx = (x + dx).clamp(0, w as i32 - 1) as usize;
                    let sy = (y + dy).clamp(0, h as i32 - 1) as usize;
                    for c in 0..4 {
                        sum[c] += truth[(sy * w + sx) * 4 + c] as u32;
                    }
                    n += 1;
                }
            }
            for c in 0..4 {
                out[i * 4 + c] = (sum[c] / n) as u8;
            }
        }
        out
    }

    #[test]
    fn within_is_the_chebyshev_neighbourhood_of_a_mask() {
        let mask = block(6, 5, 2, 2, 3, 3);
        let one = within(&mask, 6, 5, 1);
        let expected = block(6, 5, 1, 1, 4, 4);
        assert_eq!(one, expected);
        assert_eq!(within(&mask, 6, 5, 0), mask);
        assert!(within(&mask, 6, 5, 9).iter().all(|&b| b));
    }

    #[test]
    fn transforms_turn_and_mirror_offsets() {
        assert_eq!(transform(0, 2, 1), (2, 1));
        assert_eq!(transform(1, 2, 1), (-1, 2));
        assert_eq!(transform(2, 2, 1), (-2, -1));
        assert_eq!(transform(3, 2, 1), (1, -2));
        assert_eq!(transform(4, 2, 1), (-2, 1));
        assert_eq!(transform(5, 2, 1), (-1, -2));
        let quarter = Options {
            rotation: Rotation::Quarter,
            mirror: true,
            ..Options::default()
        };
        assert_eq!(allowed_transforms(&quarter), vec![0, 1, 2, 3, 4, 5, 6, 7]);
        let half = Options {
            rotation: Rotation::Half,
            ..Options::default()
        };
        assert_eq!(allowed_transforms(&half), vec![0, 2]);
        assert_eq!(allowed_transforms(&Options::default()), vec![0]);
    }

    #[test]
    fn stripes_continue_through_the_hole_where_a_ring_mean_would_blur_them() {
        let (w, h) = (48, 48);
        let truth = image(w, h, |x, _| {
            if (x / 4) % 2 == 0 {
                [220, 40, 40, 255]
            } else {
                [30, 30, 200, 255]
            }
        });
        let hole = block(w, h, 18, 18, 30, 30);
        let mut filled = truth.clone();
        for (i, &is_hole) in hole.iter().enumerate() {
            if is_hole {
                filled[i * 4..i * 4 + 4].copy_from_slice(&[0, 0, 0, 0]);
            }
        }
        let options = Options {
            color_adaptation: false,
            ..Options::default()
        };
        let mut recorder = Recorder::default();
        inpaint(
            &mut filled,
            w as u32,
            h as u32,
            &hole,
            None,
            &options,
            &mut recorder,
        )
        .unwrap();
        let synth = mean_error(&filled, &truth, &hole);
        let blur = mean_error(&ring_mean_fill(&truth, w, h, &hole), &truth, &hole);
        assert!(synth < 6.0, "synthesis error {synth}");
        assert!(blur > 40.0, "ring mean error {blur}");
        // Two pyramid levels (48 → 24; 12 would be under two patches),
        // three rounds each, reported in order.
        assert_eq!(recorder.reports.len(), 6);
        assert_eq!(
            recorder.reports[0],
            ("Content-Aware Fill".to_string(), 1, 6)
        );
        assert_eq!(
            recorder.reports[5],
            ("Content-Aware Fill".to_string(), 6, 6)
        );
        // Nothing outside the hole moved, alpha included.
        for (i, &is_hole) in hole.iter().enumerate() {
            if !is_hole {
                assert_eq!(filled[i * 4..i * 4 + 4], truth[i * 4..i * 4 + 4]);
            }
        }
        // The same seed reproduces the fill exactly.
        let mut again = truth.clone();
        inpaint(
            &mut again,
            w as u32,
            h as u32,
            &hole,
            None,
            &options,
            &mut Silent,
        )
        .unwrap();
        assert_eq!(again, filled);
    }

    #[test]
    fn the_sampling_area_is_the_only_source() {
        let (w, h) = (40, 24);
        let truth = image(w, h, |x, _| {
            if x < 20 {
                [200, 20, 20, 255]
            } else {
                [20, 20, 200, 255]
            }
        });
        let hole = block(w, h, 26, 8, 34, 16);
        let sampling: Vec<bool> = (0..w * h).map(|i| i % w < 20).collect();
        let mut filled = truth.clone();
        inpaint(
            &mut filled,
            w as u32,
            h as u32,
            &hole,
            Some(&sampling),
            &Options {
                color_adaptation: false,
                ..Options::default()
            },
            &mut Silent,
        )
        .unwrap();
        for (i, &is_hole) in hole.iter().enumerate() {
            if is_hole {
                assert_eq!(filled[i * 4..i * 4 + 4], [200, 20, 20, 255], "pixel {i}");
            }
        }
        // A sampling area with no room for a patch is an error, and so is a
        // layer too small for one.
        let tiny: Vec<bool> = (0..w * h).map(|i| i % w < 3).collect();
        assert_eq!(
            inpaint(
                &mut truth.clone(),
                w as u32,
                h as u32,
                &hole,
                Some(&tiny),
                &Options::default(),
                &mut Silent
            ),
            Err(NO_SOURCE.to_string())
        );
        let small = image(5, 5, |_, _| [1, 2, 3, 255]);
        assert!(inpaint(
            &mut small.clone(),
            5,
            5,
            &block(5, 5, 2, 2, 3, 3),
            None,
            &Options::default(),
            &mut Silent
        )
        .is_err());
        assert!(inpaint(
            &mut small.clone(),
            5,
            5,
            &block(5, 5, 2, 2, 3, 3),
            None,
            &Options {
                patch: 4,
                ..Options::default()
            },
            &mut Silent
        )
        .is_err());
    }

    #[test]
    fn mirror_and_rotation_find_sources_that_only_exist_flipped_or_turned() {
        // Left half: a sawtooth that climbs for seven pixels and drops.
        // Right half: its mirror image. The hole sits in the right half;
        // only the left may be sampled. (A linear ramp would not do: the
        // vote's averaging reproduces any linear function whatever its
        // sources' slope, so only a pattern with a direction tells.)
        let (w, h) = (64, 32);
        let saw = |x: usize| ((x % 8) * 32) as u8;
        let truth = image(w, h, |x, _| {
            let v = if x < 32 { saw(x) } else { saw(63 - x) };
            [v, v, v, 255]
        });
        let hole = block(w, h, 40, 10, 50, 22);
        let sampling: Vec<bool> = (0..w * h).map(|i| i % w < 32).collect();
        let error = |mirror: bool| {
            let mut filled = truth.clone();
            let options = Options {
                mirror,
                color_adaptation: false,
                ..Options::default()
            };
            inpaint(
                &mut filled,
                w as u32,
                h as u32,
                &hole,
                Some(&sampling),
                &options,
                &mut Silent,
            )
            .unwrap();
            mean_error(&filled, &truth, &hole)
        };
        let (plain, mirrored) = (error(false), error(true));
        assert!(mirrored < 4.0, "mirrored error {mirrored}");
        assert!(
            plain > 3.0 * mirrored,
            "plain {plain} vs mirrored {mirrored}"
        );

        // Horizontal stripes on the left, vertical on the right: only a
        // quarter turn makes the left a source for the right.
        let truth = image(w, h, |x, y| {
            let on = if x < 32 {
                (y / 4) % 2 == 0
            } else {
                (x / 4) % 2 == 0
            };
            if on {
                [230, 230, 60, 255]
            } else {
                [40, 60, 90, 255]
            }
        });
        let error = |rotation: Rotation| {
            let mut filled = truth.clone();
            let options = Options {
                rotation,
                color_adaptation: false,
                ..Options::default()
            };
            inpaint(
                &mut filled,
                w as u32,
                h as u32,
                &hole,
                Some(&sampling),
                &options,
                &mut Silent,
            )
            .unwrap();
            mean_error(&filled, &truth, &hole)
        };
        let (none, quarter) = (error(Rotation::None), error(Rotation::Quarter));
        assert!(quarter < 8.0, "quarter-turn error {quarter}");
        assert!(none > 3.0 * quarter, "none {none} vs quarter {quarter}");
    }

    #[test]
    fn color_adaptation_matches_the_border_and_a_refused_report_cancels() {
        // A flat grey field whose only source is a brighter flat region:
        // without Color Adaptation the hole fills bright; with it, the fill
        // takes the border's grey, since a flat fill has no gradient to keep.
        let (w, h) = (48, 32);
        let truth = image(w, h, |x, _| {
            if x < 24 {
                [180, 180, 180, 255]
            } else {
                [90, 90, 90, 255]
            }
        });
        let hole = block(w, h, 30, 10, 40, 20);
        let sampling: Vec<bool> = (0..w * h).map(|i| i % w < 24).collect();
        let mut raw = truth.clone();
        inpaint(
            &mut raw,
            w as u32,
            h as u32,
            &hole,
            Some(&sampling),
            &Options {
                color_adaptation: false,
                ..Options::default()
            },
            &mut Silent,
        )
        .unwrap();
        assert_eq!(raw[(15 * w + 35) * 4], 180);
        let mut adapted = truth.clone();
        inpaint(
            &mut adapted,
            w as u32,
            h as u32,
            &hole,
            Some(&sampling),
            &Options::default(),
            &mut Silent,
        )
        .unwrap();
        assert!((adapted[(15 * w + 35) * 4] as i32 - 90).abs() <= 1);
        assert!(mean_error(&adapted, &truth, &hole) < 1.0);

        let mut cancelled = truth.clone();
        assert_eq!(
            inpaint(
                &mut cancelled,
                w as u32,
                h as u32,
                &hole,
                None,
                &Options::default(),
                &mut Recorder::cancelling_at(2)
            ),
            Err(CANCELLED.to_string())
        );
        assert_eq!(cancelled, truth, "a cancelled fill writes nothing");
    }
}
