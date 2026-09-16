//! Select Subject -- Cloud Processing: the heavier detector the desktop
//! app's own on-device heuristic cannot afford. The app finds the
//! largest 4-connected region that is not the canvas edge's colour;
//! that is the *initialisation* here, after which the image is
//! segmented the way GrabCut does (Rother, Kolmogorov & Blake, "GrabCut:
//! interactive foreground extraction using iterated graph cuts",
//! SIGGRAPH 2004 -- reimplemented, not copied): colour models for
//! foreground and background are fitted to the current labelling
//! (k-means clusters with per-cluster variance, the paper's GMMs
//! simplified to isotropic components), every pixel's data cost is the
//! negative log-likelihood under each model, neighbouring pixels pay a
//! contrast-sensitive smoothness cost for differing, and the exact
//! minimum cut of that graph is the new labelling; a few rounds
//! converge. The cut is found by Dinic's max-flow on the 4-connected
//! grid (`maxflow`), exact, not approximate.
//!
//! Work is done at up to `WORK_SIZE` pixels on the long side and the
//! mask is resampled back to the image's size, so a 4000-pixel photo
//! takes about as long as a 320-pixel one.

use std::collections::VecDeque;

/// The long side the segmentation works at.
pub const WORK_SIZE: u32 = 320;
const CLUSTERS: usize = 5;
const ROUNDS: usize = 4;
const KMEANS_ITERATIONS: usize = 8;
const GAMMA: f64 = 50.0;

/// Dinic's maximum flow on a small directed graph with a source and a
/// sink; `min_cut_source_side` then says which nodes stay reachable
/// from the source in the residual graph -- the source side of a
/// minimum cut.
pub struct MaxFlow {
    head: Vec<u32>,
    to: Vec<u32>,
    cap: Vec<f64>,
    next: Vec<u32>,
    level: Vec<i32>,
    iter: Vec<u32>,
}

const NONE: u32 = u32::MAX;

impl MaxFlow {
    pub fn new(nodes: usize) -> Self {
        MaxFlow {
            head: vec![NONE; nodes],
            to: Vec::new(),
            cap: Vec::new(),
            next: Vec::new(),
            level: vec![0; nodes],
            iter: vec![NONE; nodes],
        }
    }

    /// An edge `a -> b` with capacity `forward` and its reverse with
    /// capacity `backward` (0 for a one-way edge).
    pub fn add_edge(&mut self, a: usize, b: usize, forward: f64, backward: f64) {
        for (from, to, c) in [(a, b, forward), (b, a, backward)] {
            self.to.push(to as u32);
            self.cap.push(c);
            self.next.push(self.head[from]);
            self.head[from] = (self.to.len() - 1) as u32;
        }
    }

    fn bfs(&mut self, source: usize, sink: usize) -> bool {
        self.level.iter_mut().for_each(|l| *l = -1);
        self.level[source] = 0;
        let mut queue = VecDeque::from([source]);
        while let Some(node) = queue.pop_front() {
            let mut e = self.head[node];
            while e != NONE {
                let to = self.to[e as usize] as usize;
                if self.cap[e as usize] > 1e-12 && self.level[to] < 0 {
                    self.level[to] = self.level[node] + 1;
                    queue.push_back(to);
                }
                e = self.next[e as usize];
            }
        }
        self.level[sink] >= 0
    }

    /// One blocking-flow phase, iterative (a DFS with an explicit stack,
    /// so a long grid path cannot overflow the call stack).
    fn blocking_flow(&mut self, source: usize, sink: usize) -> f64 {
        let mut total = 0.0;
        self.iter.copy_from_slice(&self.head);
        loop {
            // Walk from the source along admissible edges until the sink
            // or a dead end.
            let mut path: Vec<u32> = Vec::new();
            let mut node = source;
            let mut reached = false;
            loop {
                if node == sink {
                    reached = true;
                    break;
                }
                let mut e = self.iter[node];
                let mut advanced = false;
                while e != NONE {
                    let to = self.to[e as usize] as usize;
                    if self.cap[e as usize] > 1e-12 && self.level[to] == self.level[node] + 1 {
                        path.push(e);
                        node = to;
                        advanced = true;
                        break;
                    }
                    e = self.next[e as usize];
                    self.iter[node] = e;
                }
                if !advanced {
                    // Dead end: retreat one edge, and skip it from now on.
                    self.level[node] = -1;
                    match path.pop() {
                        Some(back) => {
                            node = self.to[(back ^ 1) as usize] as usize;
                            self.iter[node] = self.next[back as usize];
                        }
                        None => break,
                    }
                }
            }
            if !reached {
                return total;
            }
            let bottleneck = path
                .iter()
                .map(|&e| self.cap[e as usize])
                .fold(f64::INFINITY, f64::min);
            for &e in &path {
                self.cap[e as usize] -= bottleneck;
                self.cap[(e ^ 1) as usize] += bottleneck;
            }
            total += bottleneck;
        }
    }

    pub fn max_flow(&mut self, source: usize, sink: usize) -> f64 {
        let mut flow = 0.0;
        while self.bfs(source, sink) {
            let pushed = self.blocking_flow(source, sink);
            if pushed <= 1e-12 {
                break;
            }
            flow += pushed;
        }
        flow
    }

    /// After `max_flow`: the nodes on the source side of the minimum cut.
    pub fn min_cut_source_side(&self, source: usize) -> Vec<bool> {
        let mut side = vec![false; self.head.len()];
        side[source] = true;
        let mut queue = VecDeque::from([source]);
        while let Some(node) = queue.pop_front() {
            let mut e = self.head[node];
            while e != NONE {
                let to = self.to[e as usize] as usize;
                if self.cap[e as usize] > 1e-12 && !side[to] {
                    side[to] = true;
                    queue.push_back(to);
                }
                e = self.next[e as usize];
            }
        }
        side
    }
}

/// A colour model: `CLUSTERS` isotropic Gaussian components fitted by
/// k-means, each with a weight (its share of the pixels) and a variance.
struct ColourModel {
    centres: Vec<[f64; 3]>,
    variances: Vec<f64>,
    weights: Vec<f64>,
}

fn sq_dist(a: [f64; 3], b: [f64; 3]) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

impl ColourModel {
    fn fit(pixels: &[[f64; 3]]) -> Self {
        assert!(!pixels.is_empty());
        let k = CLUSTERS.min(pixels.len());
        // Deterministic seeding: evenly spaced pixels in scan order.
        let mut centres: Vec<[f64; 3]> = (0..k).map(|i| pixels[i * pixels.len() / k]).collect();
        let mut assignment = vec![0usize; pixels.len()];
        for _ in 0..KMEANS_ITERATIONS {
            for (i, p) in pixels.iter().enumerate() {
                assignment[i] = (0..k)
                    .min_by(|&a, &b| sq_dist(*p, centres[a]).total_cmp(&sq_dist(*p, centres[b])))
                    .expect("k >= 1");
            }
            let mut sums = vec![[0.0f64; 3]; k];
            let mut counts = vec![0usize; k];
            for (i, p) in pixels.iter().enumerate() {
                let c = assignment[i];
                sums[c][0] += p[0];
                sums[c][1] += p[1];
                sums[c][2] += p[2];
                counts[c] += 1;
            }
            for c in 0..k {
                if counts[c] > 0 {
                    centres[c] = [
                        sums[c][0] / counts[c] as f64,
                        sums[c][1] / counts[c] as f64,
                        sums[c][2] / counts[c] as f64,
                    ];
                }
            }
        }
        let mut variances = vec![0.0f64; k];
        let mut counts = vec![0usize; k];
        for (i, p) in pixels.iter().enumerate() {
            let c = assignment[i];
            variances[c] += sq_dist(*p, centres[c]) / 3.0;
            counts[c] += 1;
        }
        let weights = counts
            .iter()
            .map(|&n| (n as f64 / pixels.len() as f64).max(1e-6))
            .collect();
        let variances = variances
            .iter()
            .zip(&counts)
            .map(|(v, &n)| if n > 0 { (v / n as f64).max(4.0) } else { 4.0 })
            .collect();
        ColourModel {
            centres,
            variances,
            weights,
        }
    }

    /// Negative log-likelihood of `p` under the mixture.
    fn cost(&self, p: [f64; 3]) -> f64 {
        let mut likelihood = 0.0;
        for ((c, v), w) in self.centres.iter().zip(&self.variances).zip(&self.weights) {
            let d = sq_dist(p, *c);
            likelihood += w * (-d / (2.0 * v)).exp() / (2.0 * std::f64::consts::PI * v).powf(1.5);
        }
        -(likelihood.max(1e-300)).ln()
    }
}

/// The canvas edge is background, as the app's own finder assumes: the
/// two-pixel ring's colours are a background model, and a pixel starts
/// as subject when the ring model finds it less likely than the ring's
/// own 98th-percentile pixel by a margin that grows with `tolerance`
/// (so noise in the background does not seed the subject; a high
/// tolerance asks for a subject that stands out more).
fn ring_indices(width: usize, height: usize) -> Vec<usize> {
    (0..width * height)
        .filter(|&i| {
            let (x, y) = (i % width, i / width);
            x < 2 || y < 2 || x + 2 >= width || y + 2 >= height
        })
        .collect()
}

fn initial_labels(width: usize, height: usize, pixels: &[[f64; 3]], tolerance: u8) -> Vec<bool> {
    let ring = ring_indices(width, height);
    let ring_pixels: Vec<[f64; 3]> = ring.iter().map(|&i| pixels[i]).collect();
    let model = ColourModel::fit(&ring_pixels);
    let mut ring_costs: Vec<f64> = ring_pixels.iter().map(|p| model.cost(*p)).collect();
    ring_costs.sort_by(f64::total_cmp);
    let percentile = ring_costs[(ring_costs.len() * 98 / 100).min(ring_costs.len() - 1)];
    let threshold = percentile + tolerance as f64 / 8.0;
    pixels.iter().map(|p| model.cost(*p) > threshold).collect()
}

/// The largest 4-connected `true` region of `mask`, alone -- Select
/// Subject is one subject, as in the app.
fn largest_component(width: usize, height: usize, mask: &[bool]) -> Vec<bool> {
    let mut seen = vec![false; mask.len()];
    let mut best: Vec<usize> = Vec::new();
    for start in 0..mask.len() {
        if !mask[start] || seen[start] {
            continue;
        }
        let mut component = vec![start];
        let mut stack = vec![start];
        seen[start] = true;
        while let Some(i) = stack.pop() {
            let (x, y) = (i % width, i / width);
            let neighbours = [
                (x > 0).then(|| i - 1),
                (x + 1 < width).then(|| i + 1),
                (y > 0).then(|| i - width),
                (y + 1 < height).then(|| i + width),
            ];
            for n in neighbours.into_iter().flatten() {
                if mask[n] && !seen[n] {
                    seen[n] = true;
                    component.push(n);
                    stack.push(n);
                }
            }
        }
        if component.len() > best.len() {
            best = component;
        }
    }
    let mut out = vec![false; mask.len()];
    for i in best {
        out[i] = true;
    }
    out
}

/// Segments an RGB image into subject (`true`) and background. Returns
/// `None` when no subject at all is found.
pub fn segment(width: usize, height: usize, rgb: &[[u8; 3]], tolerance: u8) -> Option<Vec<bool>> {
    assert_eq!(rgb.len(), width * height);
    if width < 5 || height < 5 {
        return None;
    }
    let pixels: Vec<[f64; 3]> = rgb
        .iter()
        .map(|p| [p[0] as f64, p[1] as f64, p[2] as f64])
        .collect();
    let ring = ring_indices(width, height);
    let mut labels = initial_labels(width, height, &pixels, tolerance);
    for &i in &ring {
        labels[i] = false;
    }
    if !labels.iter().any(|&l| l) {
        return None;
    }
    // Contrast term: beta from the mean squared difference between
    // neighbours (the paper's choice), so smoothness is cheap to break
    // across real edges and expensive across flat colour.
    let mut diff_sum = 0.0;
    let mut diff_count = 0usize;
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            if x + 1 < width {
                diff_sum += sq_dist(pixels[i], pixels[i + 1]);
                diff_count += 1;
            }
            if y + 1 < height {
                diff_sum += sq_dist(pixels[i], pixels[i + width]);
                diff_count += 1;
            }
        }
    }
    let beta = if diff_sum > 0.0 {
        diff_count as f64 / (2.0 * diff_sum)
    } else {
        0.0
    };
    let smooth = |a: usize, b: usize| GAMMA * (-beta * sq_dist(pixels[a], pixels[b])).exp();
    let n = width * height;
    let (source, sink) = (n, n + 1);
    for _ in 0..ROUNDS {
        let fg: Vec<[f64; 3]> = pixels
            .iter()
            .zip(&labels)
            .filter(|(_, &l)| l)
            .map(|(p, _)| *p)
            .collect();
        let bg: Vec<[f64; 3]> = pixels
            .iter()
            .zip(&labels)
            .filter(|(_, &l)| !l)
            .map(|(p, _)| *p)
            .collect();
        if fg.is_empty() || bg.is_empty() {
            break;
        }
        let fg_model = ColourModel::fit(&fg);
        let bg_model = ColourModel::fit(&bg);
        let mut graph = MaxFlow::new(n + 2);
        for (i, p) in pixels.iter().enumerate() {
            // Source = subject: the edge from the source carries the cost
            // of calling the pixel background, and vice versa.
            graph.add_edge(source, i, bg_model.cost(*p), 0.0);
            graph.add_edge(i, sink, fg_model.cost(*p), 0.0);
        }
        // The canvas edge stays background, whatever the models say.
        for &i in &ring {
            graph.add_edge(i, sink, 1e9, 0.0);
        }
        for y in 0..height {
            for x in 0..width {
                let i = y * width + x;
                if x + 1 < width {
                    let w = smooth(i, i + 1);
                    graph.add_edge(i, i + 1, w, w);
                }
                if y + 1 < height {
                    let w = smooth(i, i + width);
                    graph.add_edge(i, i + width, w, w);
                }
            }
        }
        graph.max_flow(source, sink);
        let side = graph.min_cut_source_side(source);
        let next: Vec<bool> = side[..n].to_vec();
        let changed = next.iter().zip(&labels).filter(|(a, b)| a != b).count();
        labels = next;
        if changed == 0 {
            break;
        }
    }
    if !labels.iter().any(|&l| l) {
        return None;
    }
    Some(largest_component(width, height, &labels))
}

/// Box-averages an RGBA image down so its long side is at most `max`;
/// returns the RGB samples and the working size.
pub fn downscale(width: u32, height: u32, rgba: &[u8], max: u32) -> (usize, usize, Vec<[u8; 3]>) {
    let scale = (width.max(height) as f64 / max as f64).max(1.0);
    let (w, h) = (
        ((width as f64 / scale).round() as usize).max(1),
        ((height as f64 / scale).round() as usize).max(1),
    );
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let y0 = (y as f64 * scale) as u32;
        let y1 = (((y + 1) as f64 * scale) as u32).clamp(y0 + 1, height);
        for x in 0..w {
            let x0 = (x as f64 * scale) as u32;
            let x1 = (((x + 1) as f64 * scale) as u32).clamp(x0 + 1, width);
            let mut sum = [0u64; 3];
            let mut count = 0u64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let base = ((sy * width + sx) * 4) as usize;
                    // Transparent pixels count as black, as a flattened
                    // export would show them over nothing.
                    let a = rgba[base + 3] as u64;
                    sum[0] += rgba[base] as u64 * a / 255;
                    sum[1] += rgba[base + 1] as u64 * a / 255;
                    sum[2] += rgba[base + 2] as u64 * a / 255;
                    count += 1;
                }
            }
            out.push([
                (sum[0] / count) as u8,
                (sum[1] / count) as u8,
                (sum[2] / count) as u8,
            ]);
        }
    }
    (w, h, out)
}

/// Nearest-neighbour resampling of a working-size mask back to the
/// image's size.
pub fn upscale(mask: &[bool], w: usize, h: usize, width: u32, height: u32) -> Vec<bool> {
    let mut out = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        let sy = (y * h / height as usize).min(h - 1);
        for x in 0..width as usize {
            let sx = (x * w / width as usize).min(w - 1);
            out.push(mask[sy * w + sx]);
        }
    }
    out
}

/// The whole cloud path: PNG in, a PNG mask out (white and opaque where
/// the subject is, transparent elsewhere, the image's own size).
pub fn select_subject_png(png: &[u8], tolerance: u8) -> Result<Vec<u8>, String> {
    let decoded =
        image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png)
            .decode()
            .map_err(|e| format!("not a readable PNG: {e}"))?
            .to_rgba8();
    let (width, height) = (decoded.width(), decoded.height());
    if width == 0 || height == 0 {
        return Err("an empty image has no subject".into());
    }
    let (w, h, rgb) = downscale(width, height, decoded.as_raw(), WORK_SIZE);
    let mask = segment(w, h, &rgb, tolerance).ok_or_else(|| "no subject was found".to_string())?;
    let full = upscale(&mask, w, h, width, height);
    let mut out = Vec::with_capacity(full.len() * 4);
    for &on in &full {
        out.extend_from_slice(if on {
            &[255, 255, 255, 255]
        } else {
            &[0, 0, 0, 0]
        });
    }
    let mut buffer = Vec::new();
    use image::ImageEncoder;
    image::codecs::png::PngEncoder::new(&mut buffer)
        .write_image(&out, width, height, image::ExtendedColorType::Rgba8)
        .map_err(|e| format!("could not encode the mask: {e}"))?;
    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_flow_matches_hand_computed_cuts() {
        // s -> a (3), s -> b (2), a -> b (1), a -> t (2), b -> t (3): max flow 5,
        // min cut {s} | {a, b, t}? s->a 3 + s->b 2 = 5; a->t 2 + b->t 3 = 5.
        // The source side after saturation is just {s} (both cuts tie; the
        // residual reachability from s stops at s since s->a and s->b are
        // both saturated).
        let (s, a, b, t) = (0, 1, 2, 3);
        let mut g = MaxFlow::new(4);
        g.add_edge(s, a, 3.0, 0.0);
        g.add_edge(s, b, 2.0, 0.0);
        g.add_edge(a, b, 1.0, 0.0);
        g.add_edge(a, t, 2.0, 0.0);
        g.add_edge(b, t, 3.0, 0.0);
        assert!((g.max_flow(s, t) - 5.0).abs() < 1e-9);
        assert_eq!(g.min_cut_source_side(s), vec![true, false, false, false]);

        // A bottleneck in the middle: s -> a (10), a -> b (1), b -> t (10).
        let mut g = MaxFlow::new(4);
        g.add_edge(s, a, 10.0, 0.0);
        g.add_edge(a, b, 1.0, 0.0);
        g.add_edge(b, t, 10.0, 0.0);
        assert!((g.max_flow(s, t) - 1.0).abs() < 1e-9);
        assert_eq!(g.min_cut_source_side(s), vec![true, true, false, false]);

        // Undirected edge (both ways) and a node the sink cannot be reached from.
        let mut g = MaxFlow::new(5);
        g.add_edge(s, a, 4.0, 0.0);
        g.add_edge(a, b, 2.0, 2.0);
        g.add_edge(b, t, 4.0, 0.0);
        g.add_edge(a, 4, 7.0, 0.0);
        assert!((g.max_flow(s, t) - 2.0).abs() < 1e-9);
        assert_eq!(
            g.min_cut_source_side(s),
            vec![true, true, false, false, true]
        );
        // No path at all.
        let mut g = MaxFlow::new(3);
        g.add_edge(0, 1, 1.0, 0.0);
        assert_eq!(g.max_flow(0, 2), 0.0);
    }

    #[test]
    fn colour_model_separates_two_colours() {
        let pixels: Vec<[f64; 3]> = (0..100)
            .map(|i| {
                if i % 2 == 0 {
                    [250.0, 10.0, 10.0]
                } else {
                    [10.0, 10.0, 250.0]
                }
            })
            .collect();
        let model = ColourModel::fit(&pixels);
        assert!(model.cost([250.0, 10.0, 10.0]) < model.cost([128.0, 128.0, 128.0]));
        assert!(model.cost([10.0, 10.0, 250.0]) < model.cost([10.0, 250.0, 10.0]));
    }

    /// A noisy blob on a noisy background: the app's heuristic at a small
    /// tolerance would fragment on the noise; the cut recovers the blob.
    fn scene(width: usize, height: usize) -> (Vec<[u8; 3]>, Vec<bool>) {
        let mut seed = 12345u32;
        let mut noise = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed % 41) as i32 - 20
        };
        let mut rgb = Vec::new();
        let mut truth = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let inside = (x as i32 - width as i32 / 2).pow(2)
                    + (y as i32 - height as i32 / 2).pow(2)
                    < (width.min(height) as i32 / 3).pow(2);
                let base: [i32; 3] = if inside { [200, 60, 40] } else { [40, 90, 180] };
                rgb.push([
                    (base[0] + noise()).clamp(0, 255) as u8,
                    (base[1] + noise()).clamp(0, 255) as u8,
                    (base[2] + noise()).clamp(0, 255) as u8,
                ]);
                truth.push(inside);
            }
        }
        (rgb, truth)
    }

    #[test]
    fn a_noisy_blob_is_recovered_almost_exactly() {
        let (w, h) = (48, 40);
        let (rgb, truth) = scene(w, h);
        let mask = segment(w, h, &rgb, 12).unwrap();
        let wrong = mask.iter().zip(&truth).filter(|(a, b)| a != b).count();
        let intersection = mask.iter().zip(&truth).filter(|(a, b)| **a && **b).count();
        let union = mask.iter().zip(&truth).filter(|(a, b)| **a || **b).count();
        assert!(wrong < 15, "{wrong} pixels wrong of {}", w * h);
        assert!(intersection as f64 / union as f64 > 0.97);
        // Two blobs: only the larger is the subject.
        let mut two = rgb.clone();
        for y in 2..6 {
            for x in 2..6 {
                two[y * w + x] = [200, 60, 40];
            }
        }
        let mask = segment(w, h, &two, 12).unwrap();
        assert!(!mask[3 * w + 3]);
        assert!(mask[(h / 2) * w + w / 2]);
        // A flat image has no subject; a 4-pixel-high one cannot be cut.
        assert!(segment(w, h, &vec![[7, 7, 7]; w * h], 0).is_none());
        assert!(segment(w, 4, &vec![[7, 7, 7]; w * 4], 0).is_none());
    }

    #[test]
    fn scaling_round_trips_and_the_png_path_masks_the_subject() {
        let (w, h) = (60, 44);
        let (rgb, truth) = scene(w, h);
        let mut rgba = Vec::new();
        for p in &rgb {
            rgba.extend_from_slice(&[p[0], p[1], p[2], 255]);
        }
        // No downscale needed below the limit: identity.
        let (dw, dh, small) = downscale(w as u32, h as u32, &rgba, WORK_SIZE);
        assert_eq!((dw, dh), (w, h));
        assert_eq!(small, rgb);
        // Halving then nearest-upscaling keeps the blob's shape within a pixel ring.
        let (hw, hh, half) = downscale(w as u32, h as u32, &rgba, 30);
        assert_eq!((hw, hh), (30, 22));
        let mask = segment(hw, hh, &half, 12).unwrap();
        let full = upscale(&mask, hw, hh, w as u32, h as u32);
        let wrong = full.iter().zip(&truth).filter(|(a, b)| a != b).count();
        assert!(wrong < 200, "{wrong} wrong after a 2x round trip");
        // The PNG path.
        let mut png = Vec::new();
        use image::ImageEncoder;
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8)
            .unwrap();
        let out = select_subject_png(&png, 12).unwrap();
        let decoded =
            image::ImageReader::with_format(std::io::Cursor::new(&out), image::ImageFormat::Png)
                .decode()
                .unwrap()
                .to_rgba8();
        assert_eq!((decoded.width(), decoded.height()), (w as u32, h as u32));
        let centre = decoded.get_pixel(w as u32 / 2, h as u32 / 2).0;
        let corner = decoded.get_pixel(0, 0).0;
        assert_eq!(centre, [255, 255, 255, 255]);
        assert_eq!(corner, [0, 0, 0, 0]);
        assert!(select_subject_png(b"nope", 12).is_err());
    }
}
