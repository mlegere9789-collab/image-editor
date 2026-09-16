//! Generate Image: this project's own text-conditioned diffusion model,
//! trained from scratch here on 787 real, individually-licensed
//! landscape photographs (`models/train_generate/`, attributions in
//! `attributions.json`) and run on the CPU through `tract`. A denoising
//! diffusion model (Ho, Jain & Abbeel 2020) with a cosine schedule
//! (Nichol & Dhariwal 2021), sampled by DDIM (Song, Meng & Ermon 2021)
//! under classifier-free guidance (Ho & Salimans 2022): the network
//! predicts the noise in a noisy 64×64 image given the step, a category
//! and a bag-of-words caption; the exported graph evaluates the
//! conditioned and unconditioned passes as one batch of two, so one
//! model run per sampling step.
//!
//! Every function that turns a prompt into the model's inputs
//! (`tokenize`, `caption_vector`, `category_of_prompt`) and the
//! schedule (`cosine_alphas_cumprod`) is written to match the Python
//! trainer exactly, so a prompt means the same thing at training and
//! at generation time. `models/generate_check.json` pins the model's
//! output on a fixed input to PyTorch's, and a test checks tract's
//! evaluation against it.

use std::io::Cursor;
use std::sync::OnceLock;

use crate::progress::{Progress, Silent, Span};

use tract_onnx::prelude::*;

pub const IMAGE_SIZE: usize = 64;
pub const CATEGORIES: [&str; 7] = [
    "city", "field", "forest", "lake", "mountain", "ocean", "road",
];
pub const NULL_CATEGORY: usize = 7;
pub const TIMESTEPS: usize = 1000;
/// DDIM steps by default: 25 network runs, each a batch of two passes.
pub const DEFAULT_STEPS: usize = 25;
pub const DEFAULT_GUIDANCE: f32 = 2.0;
const VOCAB_JSON: &str = include_str!("../models/train_generate/vocab.json");
const MODEL_BYTES: &[u8] = include_bytes!("../models/generate.onnx");
const STOPWORDS: [&str; 62] = [
    "the", "and", "with", "from", "over", "into", "near", "for", "this", "that", "are", "was",
    "you", "your", "its", "our", "off", "out", "one", "two", "three", "img", "dsc", "jpg", "photo",
    "picture", "image", "view", "shot", "day", "trip", "some", "all", "not", "but", "has", "have",
    "had", "his", "her", "they", "them", "their", "there", "here", "where", "when", "what", "than",
    "then", "also", "just", "very", "more", "most", "such", "only", "a", "an", "of", "on", "in",
];

/// The trainer's tokens: runs of ASCII letters in the lower-cased text,
/// three letters or more, that are not stopwords.
pub fn tokenize(text: &str) -> Vec<String> {
    let lower = text.to_lowercase();
    lower
        .split(|c: char| !c.is_ascii_lowercase())
        .filter(|t| t.len() >= 3 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

pub fn vocab() -> &'static Vec<String> {
    static VOCAB: OnceLock<Vec<String>> = OnceLock::new();
    VOCAB.get_or_init(|| serde_json::from_str(VOCAB_JSON).expect("the bundled vocabulary parses"))
}

/// The caption as the model sees it: one per vocabulary word, 1 where
/// the prompt uses it. Words outside the vocabulary say nothing.
pub fn caption_vector(text: &str) -> Vec<f32> {
    let vocab = vocab();
    let mut v = vec![0.0f32; vocab.len()];
    for token in tokenize(text) {
        if let Some(i) = vocab.iter().position(|w| *w == token) {
            v[i] = 1.0;
        }
    }
    v
}

/// The first category word the prompt mentions, else the null category.
pub fn category_of_prompt(text: &str) -> usize {
    tokenize(text)
        .iter()
        .find_map(|t| CATEGORIES.iter().position(|c| c == t))
        .unwrap_or(NULL_CATEGORY)
}

/// Which vocabulary words a prompt actually reaches -- what the model
/// will hear -- so the UI can say so.
pub fn understood_words(text: &str) -> Vec<String> {
    let vocab = vocab();
    tokenize(text)
        .into_iter()
        .filter(|t| vocab.iter().any(|w| w == t))
        .collect()
}

/// The cosine schedule's cumulative alphas, computed in f64 as the
/// trainer does, then cast.
pub fn cosine_alphas_cumprod(timesteps: usize) -> Vec<f32> {
    let s = 0.008f64;
    let f = |step: f64| {
        ((step / timesteps as f64 + s) / (1.0 + s) * std::f64::consts::FRAC_PI_2)
            .cos()
            .powi(2)
    };
    let f0 = f(0.0);
    let mut out = Vec::with_capacity(timesteps);
    let mut prod = 1.0f64;
    for i in 0..timesteps {
        let beta = (1.0 - (f((i + 1) as f64) / f0) / (f(i as f64) / f0)).min(0.999);
        prod *= 1.0 - beta;
        out.push(prod as f32);
    }
    out
}

type Model = SimplePlan<TypedFact, Box<dyn TypedOp>, Graph<TypedFact, Box<dyn TypedOp>>>;

fn model() -> Result<&'static Model, String> {
    static MODEL: OnceLock<Result<Model, String>> = OnceLock::new();
    MODEL
        .get_or_init(|| {
            tract_onnx::onnx()
                .model_for_read(&mut Cursor::new(MODEL_BYTES))
                .and_then(|m| m.into_optimized())
                .and_then(|m| m.into_runnable())
                .map_err(|err| format!("Could not load the bundled Generate Image model: {err}"))
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// The width of the sinusoidal timestep embedding the graph takes.
pub const EMBEDDING: usize = 128;

/// The trainer's `timestep_embedding(t, 128)`: cosines then sines of
/// `t · exp(−ln 10000 · i / 64)`, computed here in f64 and cast, so the
/// graph never evaluates cos/sin of arguments up to 1000 itself.
pub fn timestep_embedding(t: usize) -> Vec<f32> {
    let half = EMBEDDING / 2;
    let freqs: Vec<f64> = (0..half)
        .map(|i| (-(10000f64.ln()) * i as f64 / half as f64).exp())
        .collect();
    let mut out = Vec::with_capacity(EMBEDDING);
    out.extend(
        freqs
            .iter()
            .map(|f| ((t as f64 * f) as f32 as f64).cos() as f32),
    );
    out.extend(
        freqs
            .iter()
            .map(|f| ((t as f64 * f) as f32 as f64).sin() as f32),
    );
    out
}

/// One evaluation: `x` is the batch of two (conditioned, unconditioned)
/// noisy images, `t` the step; returns the two predicted noises.
fn predict_noise(
    x: &[f32],
    t: usize,
    category: usize,
    caption: &[f32],
) -> Result<Vec<f32>, String> {
    let model = model()?;
    let n = 3 * IMAGE_SIZE * IMAGE_SIZE;
    let x_tensor: Tensor =
        tract_ndarray::Array4::from_shape_vec((2, 3, IMAGE_SIZE, IMAGE_SIZE), x.to_vec())
            .map_err(|e| format!("Could not shape the image: {e}"))?
            .into();
    let temb_row = timestep_embedding(t);
    let mut temb = temb_row.clone();
    temb.extend_from_slice(&temb_row);
    let t_tensor: Tensor = tract_ndarray::Array2::from_shape_vec((2, EMBEDDING), temb)
        .map_err(|e| format!("Could not shape the timestep: {e}"))?
        .into();
    let category_tensor: Tensor =
        tract_ndarray::Array1::from_vec(vec![category as i64, NULL_CATEGORY as i64]).into();
    let mut caption_rows = caption.to_vec();
    caption_rows.extend(std::iter::repeat(0.0f32).take(caption.len()));
    let caption_tensor: Tensor =
        tract_ndarray::Array2::from_shape_vec((2, caption.len()), caption_rows)
            .map_err(|e| format!("Could not shape the caption: {e}"))?
            .into();
    let result = model
        .run(tvec!(
            x_tensor.into(),
            t_tensor.into(),
            category_tensor.into(),
            caption_tensor.into()
        ))
        .map_err(|e| format!("Generate Image's model failed to run: {e}"))?;
    let out = result[0]
        .to_array_view::<f32>()
        .map_err(|e| format!("Generate Image's model returned an unreadable result: {e}"))?;
    let flat: Vec<f32> = out.iter().copied().collect();
    if flat.len() != 2 * n {
        return Err(format!(
            "Generate Image's model returned {} values, not {}.",
            flat.len(),
            2 * n
        ));
    }
    Ok(flat)
}

/// Standard-normal samples from a seed: splitmix64 then Box-Muller, the
/// same construction Generative Fill's noise plane uses.
pub fn gaussian_noise(seed: u64, count: usize) -> Vec<f32> {
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut out = Vec::with_capacity(count);
    while out.len() < count {
        let u1 = ((next() >> 11) as f64 + 1.0) / ((1u64 << 53) as f64 + 1.0);
        let u2 = (next() >> 11) as f64 / (1u64 << 53) as f64;
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * std::f64::consts::PI * u2;
        out.push((r * theta.cos()) as f32);
        if out.len() < count {
            out.push((r * theta.sin()) as f32);
        }
    }
    out
}

/// The DDIM step schedule: `steps` timesteps from `T - 1` down to `0`,
/// evenly spaced and rounded, as the trainer samples its grids.
pub fn ddim_timesteps(steps: usize) -> Vec<usize> {
    (0..steps)
        .map(|i| {
            let t = (TIMESTEPS - 1) as f64 * (1.0 - i as f64 / (steps - 1).max(1) as f64);
            t.round() as usize
        })
        .collect()
}

/// The DDIM loop from `x` (CHW, batch of one) down `ts` to a clean
/// image in [-1, 1]: one batched model run per step, guidance mixing the
/// conditioned and unconditioned predictions, the DDIM update between.
/// Each step is reported to `progress` as one unit of `span` under
/// `stage`; a refused report stops the loop with the cancelled error.
#[allow(clippy::too_many_arguments)]
fn ddim(
    mut x: Vec<f32>,
    ts: &[usize],
    category: usize,
    caption: &[f32],
    guidance: f32,
    progress: &mut dyn Progress,
    span: Span,
    stage: &str,
) -> Result<Vec<f32>, String> {
    let alphas = cosine_alphas_cumprod(TIMESTEPS);
    let n = 3 * IMAGE_SIZE * IMAGE_SIZE;
    for (i, &t) in ts.iter().enumerate() {
        let mut batch = x.clone();
        batch.extend_from_slice(&x);
        let eps = predict_noise(&batch, t, category, caption)?;
        span.report(progress, stage, i + 1)?;
        let a_t = alphas[t];
        let next = if i + 1 < ts.len() {
            Some(alphas[ts[i + 1]])
        } else {
            None
        };
        for j in 0..n {
            let e = eps[n + j] + guidance * (eps[j] - eps[n + j]);
            let x0 = ((x[j] - (1.0 - a_t).sqrt() * e) / a_t.sqrt()).clamp(-1.0, 1.0);
            x[j] = match next {
                Some(a_prev) => a_prev.sqrt() * x0 + (1.0 - a_prev).sqrt() * e,
                None => x0,
            };
        }
    }
    Ok(x)
}

/// CHW in [-1, 1] to interleaved RGB bytes.
fn rgb_from_chw(x: &[f32]) -> Vec<u8> {
    let plane = IMAGE_SIZE * IMAGE_SIZE;
    let mut rgb = Vec::with_capacity(plane * 3);
    for i in 0..plane {
        for c in 0..3 {
            rgb.push(((x[c * plane + i] + 1.0) * 127.5).round().clamp(0.0, 255.0) as u8);
        }
    }
    rgb
}

/// Interleaved RGB bytes to CHW in [-1, 1].
fn chw_from_rgb(rgb: &[u8]) -> Vec<f32> {
    let plane = IMAGE_SIZE * IMAGE_SIZE;
    let mut x = vec![0.0f32; plane * 3];
    for i in 0..plane {
        for c in 0..3 {
            x[c * plane + i] = rgb[i * 3 + c] as f32 / 127.5 - 1.0;
        }
    }
    x
}

/// SDEdit's step schedule: `steps` timesteps from `start` down to `0`.
pub fn sdedit_timesteps(start: usize, steps: usize) -> Vec<usize> {
    (0..steps)
        .map(|i| (start as f64 * (1.0 - i as f64 / (steps - 1).max(1) as f64)).round() as usize)
        .collect()
}

/// SDEdit (Meng et al., "SDEdit: Guided Image Synthesis and Editing with
/// Stochastic Differential Equations", ICLR 2022): a 64×64 RGB
/// `reference` is noised to timestep `strength · (T − 1)` and denoised
/// from there under `prompt`, so the result keeps the reference's
/// layout and colour in proportion to how little noise was added and
/// takes the prompt's character in proportion to how much. `strength`
/// 0 returns the reference; 1 is nearly a fresh generation. An empty
/// prompt denoises unconditionally. Errors for a reference of the
/// wrong size, a strength outside 0..=1, or as `generate_rgb` does.
pub fn sdedit_rgb(
    reference: &[u8],
    prompt: &str,
    seed: u64,
    strength: f32,
    steps: usize,
    guidance: f32,
) -> Result<Vec<u8>, String> {
    sdedit_rgb_with(
        reference,
        prompt,
        seed,
        strength,
        steps,
        guidance,
        &mut Silent,
        Span::whole(steps),
    )
}

/// [`sdedit_rgb`], reporting each denoising step as one unit of `span`
/// to `progress` under the stage "Editing" and stopping when refused.
#[allow(clippy::too_many_arguments)]
pub fn sdedit_rgb_with(
    reference: &[u8],
    prompt: &str,
    seed: u64,
    strength: f32,
    steps: usize,
    guidance: f32,
    progress: &mut dyn Progress,
    span: Span,
) -> Result<Vec<u8>, String> {
    if reference.len() != IMAGE_SIZE * IMAGE_SIZE * 3 {
        return Err(format!(
            "SDEdit takes a {IMAGE_SIZE}x{IMAGE_SIZE} RGB reference, not {} bytes.",
            reference.len()
        ));
    }
    if !(0.0..=1.0).contains(&strength) {
        return Err("Strength is between 0 and 1.".to_string());
    }
    if !(2..=TIMESTEPS).contains(&steps) {
        return Err(format!("Generate Image runs 2 to {TIMESTEPS} steps."));
    }
    if !guidance.is_finite() || guidance < 0.0 {
        return Err("Guidance must be a non-negative number.".to_string());
    }
    let start = (strength * (TIMESTEPS - 1) as f32).round() as usize;
    if start == 0 {
        return Ok(reference.to_vec());
    }
    let (category, caption) = if prompt.trim().is_empty() {
        (NULL_CATEGORY, vec![0.0f32; vocab().len()])
    } else {
        (category_of_prompt(prompt), caption_vector(prompt))
    };
    let alphas = cosine_alphas_cumprod(TIMESTEPS);
    let a = alphas[start];
    let noise = gaussian_noise(seed, 3 * IMAGE_SIZE * IMAGE_SIZE);
    let x: Vec<f32> = chw_from_rgb(reference)
        .iter()
        .zip(&noise)
        .map(|(r, n)| a.sqrt() * r + (1.0 - a).sqrt() * n)
        .collect();
    let x0 = ddim(
        x,
        &sdedit_timesteps(start, steps),
        category,
        &caption,
        guidance,
        progress,
        span,
        "Editing",
    )?;
    Ok(rgb_from_chw(&x0))
}

/// Generates a 64×64 RGB image for `prompt` from `seed`: DDIM over
/// `steps` steps with classifier-free guidance `guidance` (1 = the
/// conditioned prediction alone; 2 = the trainer's own sample grids).
/// Returns `IMAGE_SIZE * IMAGE_SIZE * 3` bytes. Errors for a blank
/// prompt, fewer than 2 steps, or a model failure.
pub fn generate_rgb(
    prompt: &str,
    seed: u64,
    steps: usize,
    guidance: f32,
) -> Result<Vec<u8>, String> {
    generate_rgb_with(
        prompt,
        seed,
        steps,
        guidance,
        &mut Silent,
        Span::whole(steps),
    )
}

/// [`generate_rgb`], reporting each denoising step as one unit of `span`
/// to `progress` under the stage "Generating" and stopping when refused.
pub fn generate_rgb_with(
    prompt: &str,
    seed: u64,
    steps: usize,
    guidance: f32,
    progress: &mut dyn Progress,
    span: Span,
) -> Result<Vec<u8>, String> {
    if prompt.trim().is_empty() {
        return Err("Generate Image needs a prompt.".to_string());
    }
    if !(2..=TIMESTEPS).contains(&steps) {
        return Err(format!("Generate Image runs 2 to {TIMESTEPS} steps."));
    }
    if !guidance.is_finite() || guidance < 0.0 {
        return Err("Guidance must be a non-negative number.".to_string());
    }
    let category = category_of_prompt(prompt);
    let caption = caption_vector(prompt);
    let x = gaussian_noise(seed, 3 * IMAGE_SIZE * IMAGE_SIZE);
    let x0 = ddim(
        x,
        &ddim_timesteps(steps),
        category,
        &caption,
        guidance,
        progress,
        span,
        "Generating",
    )?;
    Ok(rgb_from_chw(&x0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_is_read_as_the_trainer_reads_it() {
        assert_eq!(
            tokenize("A Misty lake at Dawn, the IMG_2023!"),
            vec!["misty", "lake", "dawn"]
        );
        assert_eq!(tokenize("of on in"), Vec::<String>::new());
        assert_eq!(category_of_prompt("snowy mountain road"), 4);
        assert_eq!(category_of_prompt("a red barn"), NULL_CATEGORY);
        let v = caption_vector("lake");
        assert_eq!(v.len(), vocab().len());
        assert_eq!(v[3], 1.0, "the categories lead the vocabulary");
        assert_eq!(v.iter().filter(|&&x| x == 1.0).count(), 1);
        assert!(understood_words("a lake nobody photographed").contains(&"lake".to_string()));
        assert_eq!(&vocab()[..7], &CATEGORIES.map(str::to_string));
    }

    #[test]
    fn the_schedule_matches_the_trainer() {
        let alphas = cosine_alphas_cumprod(TIMESTEPS);
        assert_eq!(alphas.len(), TIMESTEPS);
        // torch: cosine_alphas_cumprod(1000)[0] = 0.99999, [500] ≈ 0.4917, [999] ≈ 6.0e-05.
        assert!((alphas[0] - 0.99999).abs() < 1e-4, "{}", alphas[0]);
        assert!((alphas[500] - 0.4917).abs() < 2e-3, "{}", alphas[500]);
        assert!(alphas[999] < 1e-3 && alphas[999] > 0.0);
        assert!(alphas.windows(2).all(|w| w[1] < w[0]));
        assert_eq!(ddim_timesteps(25)[0], 999);
        assert_eq!(*ddim_timesteps(25).last().unwrap(), 0);
        assert_eq!(ddim_timesteps(3), vec![999, 500, 0]);
        let noise = gaussian_noise(7, 20_000);
        let mean = noise.iter().sum::<f32>() / noise.len() as f32;
        let var = noise.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / noise.len() as f32;
        assert!(mean.abs() < 0.03 && (var - 1.0).abs() < 0.05);
        assert_eq!(gaussian_noise(7, 5), gaussian_noise(7, 5));
        assert_ne!(gaussian_noise(7, 5), gaussian_noise(8, 5));
    }

    #[test]
    fn tract_evaluates_the_model_as_pytorch_did() {
        #[derive(serde::Deserialize)]
        struct Check {
            first: Vec<f32>,
            mean_abs: f32,
        }
        let check: Check =
            serde_json::from_str(include_str!("../models/generate_check.json")).unwrap();
        let n = 2 * 3 * IMAGE_SIZE * IMAGE_SIZE;
        let x: Vec<f32> = (0..n).map(|i| ((i as f64) / 7.0).sin() as f32).collect();
        let caption = vec![0.0f32; vocab().len()];
        let eps = predict_noise(&x, 500, 3, &caption).unwrap();
        let mean_abs = eps.iter().map(|v| v.abs()).sum::<f32>() / eps.len() as f32;
        assert!(
            (mean_abs - check.mean_abs).abs() < 2e-3,
            "{mean_abs} vs {}",
            check.mean_abs
        );
        for (i, (a, b)) in eps.iter().zip(&check.first).enumerate() {
            assert!((a - b).abs() < 2e-3, "eps[{i}] {a} vs {b}");
        }
    }

    #[test]
    fn sdedit_keeps_the_reference_in_proportion_to_its_strength() {
        // A reference: a vertical gradient, dark top to light bottom.
        let reference: Vec<u8> = (0..IMAGE_SIZE * IMAGE_SIZE)
            .flat_map(|i| {
                let v = (i / IMAGE_SIZE * 4) as u8;
                [v, v, v]
            })
            .collect();
        assert_eq!(
            sdedit_rgb(&reference, "lake", 1, 0.0, 2, 2.0).unwrap(),
            reference
        );
        let l1 = |a: &[u8], b: &[u8]| {
            a.iter()
                .zip(b)
                .map(|(x, y)| (*x as i32 - *y as i32).abs())
                .sum::<i32>()
        };
        let low = sdedit_rgb(&reference, "lake", 1, 0.2, 2, 2.0).unwrap();
        let high = sdedit_rgb(&reference, "lake", 1, 0.9, 2, 2.0).unwrap();
        assert!(
            l1(&low, &reference) < l1(&high, &reference),
            "more noise, further from the reference"
        );
        assert_eq!(low, sdedit_rgb(&reference, "lake", 1, 0.2, 2, 2.0).unwrap());
        assert_eq!(sdedit_timesteps(500, 3), vec![500, 250, 0]);
        assert!(sdedit_rgb(&reference[..30], "lake", 1, 0.5, 2, 2.0).is_err());
        assert!(sdedit_rgb(&reference, "lake", 1, 1.5, 2, 2.0).is_err());
        assert!(sdedit_rgb(&reference, "", 1, 0.2, 1, 2.0).is_err());
        // An empty prompt is allowed: the unconditional model.
        assert!(sdedit_rgb(&reference, "", 1, 0.2, 2, 2.0).is_ok());
    }

    #[test]
    fn generate_image_is_deterministic_per_seed_and_in_range() {
        // Two steps, so the (debug-build) test stays affordable: the
        // sampler's arithmetic is the same at 2 steps as at 25.
        let a = generate_rgb("a lake at dawn", 1, 2, 2.0).unwrap();
        assert_eq!(a.len(), IMAGE_SIZE * IMAGE_SIZE * 3);
        assert_ne!(a, generate_rgb("a lake at dawn", 2, 2, 2.0).unwrap());
        assert_eq!(
            gaussian_noise(1, 8),
            gaussian_noise(1, 8),
            "same seed, same start, same image"
        );
        let spread = a.iter().max().unwrap() - a.iter().min().unwrap();
        assert!(spread > 32, "an image, not a flat colour: spread {spread}");
        // Every step reports, and a refused report stops the loop.
        let mut recorder = crate::progress::Recorder::default();
        let b =
            generate_rgb_with("a lake at dawn", 1, 2, 2.0, &mut recorder, Span::whole(2)).unwrap();
        assert_eq!(a, b);
        assert_eq!(
            recorder.reports,
            vec![
                ("Generating".to_string(), 1, 2),
                ("Generating".to_string(), 2, 2)
            ]
        );
        let mut cancelling = crate::progress::Recorder::cancelling_at(1);
        assert_eq!(
            generate_rgb_with(
                "a lake at dawn",
                1,
                2,
                2.0,
                &mut cancelling,
                Span { base: 4, total: 8 }
            ),
            Err(crate::progress::CANCELLED.to_string())
        );
        assert_eq!(cancelling.reports, vec![("Generating".to_string(), 5, 8)]);
        assert!(generate_rgb("   ", 1, 3, 2.0).is_err());
        assert!(generate_rgb("lake", 1, 1, 2.0).is_err());
        assert!(generate_rgb("lake", 1, 3, -1.0).is_err());
    }
}
