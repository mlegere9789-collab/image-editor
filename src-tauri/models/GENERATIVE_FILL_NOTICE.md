# Bundled model: `generative_fill.onnx`

**This model was trained by this project, from scratch.** No pretrained
checkpoint of any kind — context-encoder or otherwise — was loaded at
any point in producing it.

## What this is, and honestly, what it is not

Adobe's own Generative Fill is a real text-conditioned latent diffusion
model. Two facts, cross-checked directly rather than assumed, are why
this project cannot train an equivalent:

- **Training compute.** Stable Diffusion — a comparable latent diffusion
  model, not even Firefly's own larger one — took roughly 150,000-200,000
  A100 GPU-hours to train (about $600,000 at market cloud pricing).
  A modern CPU core, even generously estimated, runs 3-4 orders of
  magnitude slower than an A100 at this kind of convolution-heavy
  workload; a CPU-only sandbox with no GPU at all pushes the real gap
  closer to 4-5 orders of magnitude. That is not "would take a long
  time" — 150,000 GPU-hours at even a (very generous) 1000x CPU slowdown
  is on the order of 17 CPU-*years* of continuous compute for one
  training run, before accounting for the fact this sandbox has no GPU
  to be slower *than* at all. Longer sessions don't close a gap of this
  size; they're the wrong tool for it, the same way more time doesn't
  make a bicycle cross an ocean.
- **Training data.** A text-conditioned model needs paired image/caption
  data at a scale (hundreds of millions of pairs, in Stable Diffusion's
  and Firefly's own case) this project has no reachable source for, on
  top of the compute problem above.

**What a small, from-scratch model genuinely can do — and what this one
does — is different in kind, not just smaller:** context-only
hallucination. [Context Encoders (Pathak, Krahenbuhl, Donahue, Darrell &
Efros, CVPR 2016)](https://openaccess.thecvf.com/content_cvpr_2016/papers/Pathak_Context_Encoders_Feature_CVPR_2016_paper.pdf)
established that an encoder-decoder network, trained self-supervised to
reconstruct a masked region from the pixels around it, learns a real,
useful visual prior — no text, no labels, no captions needed, since the
"label" is just the original unmasked pixels the network never gets to
see. That is genuinely trainable at the scale this project's other four
models already proved out (small architecture, hundreds of real
photographs, CPU-only, one training run).

## Adobe Firefly, audited honestly (not assumed)

What's real and good about it: Adobe trained on Adobe Stock plus openly
licensed and public-domain content specifically to sidestep the
copyright-infringement risk generic web-scraped models carry, and ships
it tightly integrated into Photoshop's own selection/layer/masking
tools — genuinely useful UX, not just a raw model. It has iterated
publicly through three major versions since 2024, each closing the gap
with Midjourney/FLUX.

What's real and not good about it, per Adobe's own published limitations
and user reports, not assumption: **text rendering inside a generated
image is only about 60% accurate**; **roughly 30% of images containing
people still show distorted hands/fingers**; results are reported as
"inconsistent, distorted, or poorly composed" often enough that several
independent reviews call it unsuitable for client-facing work without
manual cleanup. It requires an active internet connection and a Creative
Cloud "generative credits" subscription for every single generation —
no offline mode exists at any tier.

**Where a small, honest, local model can genuinely do better than
Firefly at something real** — not by being a better generator, which it
categorically is not, but on axes Firefly structurally can't compete
on: it runs with zero network round-trip and zero per-image cost or
credit consumption; every pixel stays on the user's own machine, never
uploaded anywhere; the entire training recipe, dataset attribution, and
weights are in this repository, reproducible and auditable end to end,
where Firefly's own training data and architecture details are largely
undisclosed; and it keeps working with no subscription and no internet
connection at all. None of that makes its output *better* — it doesn't,
and this document isn't claiming it does.

## Architecture

A context-encoder-style convolutional encoder-decoder with U-Net-style
skip connections (Ronneberger, Fischer & Brox, MICCAI 2015): three
stride-2 downsampling stages (32→64→128→256 channels), six residual
blocks at the 256-channel bottleneck, three matching
nearest-upsample+convolution stages back up, each concatenating the
matching encoder stage's own higher-resolution features before its
convolution — the same `ConvInstanceReLU`/`ResidualBlock` building
blocks `style_transfer.onnx`/`landscape_mixer.onnx` already use, so it
lowers through ONNX ops `tract` (this project's Rust inference engine)
can run. Input is 4 channels: RGB with the hole zeroed out, plus a
1-channel mask (1 inside the hole, 0 outside) — the standard
context-encoder input recipe, so the network can tell "this pixel is
genuinely black" from "this pixel needs to be invented."

**The skip connections were not the original design.** A first version
— a plain context-encoder bottleneck, no skips — trained cleanly (loss
fell smoothly) but produced a real, diagnosed failure: a small, uniform
hole surrounded by strongly-coloured context (e.g. clear sky next to a
warm sunset) still came back a flat, wrong-coloured patch. Confirmed via
the network's own *unmasked*-region reconstruction (visibly correct,
ruling out a pipeline bug) that the three downsamples-to-a-16×16-bottleneck
were losing the fine local colour cue a good fill needs in low-texture
regions. Skip connections — a real, standard fix for exactly this failure
mode, not a novel invention — let the decoder read each encoder stage's
own full-resolution features directly. Retraining from scratch with
them measurably helped textured content; see Honest limitations below
for where the same weakness still shows up.

**Fixed 128×128 input, on purpose.** The exported ONNX has no dynamic
height/width axes. Two real reasons: `tract` could not prove the skip
connections' own `Concat` node shapes matched under symbolic axes
(confirmed directly — dynamic axes failed to load with an
`InferenceConcat` analysis error); and running the network at any other
size would itself be a real distribution shift this architecture has no
proven robustness to. `generative_fill.rs` resizes its own context-window
crop to exactly 128×128 (bilinear) before inference and the masked
prediction back to the crop's real size afterward, so every crop the
model ever actually sees matches training exactly.

**Not a GAN.** The original Context Encoders paper uses an adversarial
loss to sharpen its output; this project's own Style Transfer training
already needed three attempts to find a stable *non*-adversarial recipe
(see `STYLE_TRANSFER_NOTICE.md`) — a jointly-trained discriminator is a
well-known further source of instability (mode collapse, oscillation)
this project has no proven track record surviving. Instead: a real,
frozen, pretrained VGG16 perceptual loss (the same technique — and the
same VGG16 file — Style Transfer/Landscape Mixer already use) at a
deliberately small weight, plus a heavier L1 reconstruction loss. That
weighting is itself a lesson from this model's own training: an early
attempt with a perceptual weight as high as the style-transfer recipe's
own let the network satisfy the loss with "plausible-looking texture in
the abstract" rather than the colour that actually belonged in the hole.
The honest tradeoff that remains regardless: real, context-aware, but
visibly softer/blurrier fills than a true GAN or diffusion model would
produce — no adversarial or diffusion objective is sharpening
high-frequency texture here.

## Training data

The same 787 real, individually-licensed landscape photographs
`train_landscape/` already fetched and attributed (`attributions.json`
in `train_generative_fill/`, copied from there) — via the
[`ml5js/ml5-data-and-models`](https://github.com/ml5js/ml5-data-and-models)
repository, filtered to only CC BY, CC0, public domain, or "no known
copyright restrictions" images. More scene diversity than the 51-photo
OpenCV set matters more here than it did for style transfer, since this
network has to invent plausible *content* from context, not just re-tone
pixels that are already there.

## Training recipe

128x128 real patches, a random rectangular hole 32-64px on a side at a
random position, self-supervised (the "label" is the same patch before
masking — no external labels needed). L1 loss on the masked region
(normalized by hole area, weighted 3x), a small L1 term on the unmasked
region for boundary consistency, a small-weight VGG16 perceptual loss
(`relu2_2`) on the composited result, gradient-norm clipping at 5.0, and
an explicit non-finite-loss abort (the same divergence guard
`train_style.py` added after its own second training attempt diverged)
so a failed run cannot silently save broken weights. 40 epochs (8,000
real gradient steps) from random initialization, converging smoothly
with no divergence — final loss 0.242 hole-region L1, 0.585 perceptual.

## Honest limitations

- **No text prompt.** This model has no text input at all — Photoshop's
  own "leave the prompt empty" content-aware behavior is the honest
  comparison point, not full text-to-image generation.
- **Local, not global.** Inference runs on a context window around the
  selection (roughly the selection's own size plus 48px of real
  surrounding pixels on each side, not the whole canvas) — comparable to
  what the network actually saw during training. A very large selection
  gets filled from its own local neighborhood, not a scene-level
  understanding of the whole photograph.
- **Soft, not sharp.** No adversarial or diffusion objective — expect
  plausible but visibly softer texture than a GAN/diffusion fill would
  produce, especially for fine, high-frequency detail (individual leaves,
  brick texture, text).
- **Genuinely good on textured content, genuinely weak on large uniform
  sky/gradient regions — verified, not assumed.** A real qualitative
  sweep across several training photographs found a consistent pattern:
  selections over ocean water (with reflection), city buildings at
  night, and mountain silhouettes fill in convincingly, close to
  invisible; selections sitting entirely within a large, smooth sky —
  a clear blue sky over desert rock, a warm gradient sunset — still come
  back as a visibly flat, mismatched patch, feathered at the edges (see
  `src-tauri/src/generative_fill.rs`'s own `FEATHER_PX` blending) but
  not colour-correct in the interior. This tracks with the failure mode
  the skip connections were added to fix: low-texture regions give the
  network the least to latch onto, and it remains the network's weakest
  case after 40 real epochs of training. Selections over detailed,
  textured content are the reliable case; a large clear-sky selection is
  the case to expect a visible patch on.
- **One deterministic result per selection.** No noise input, so
  Generate Similar's own "give me another variation" has no real
  meaning for this model as built — a documented, not silently dropped,
  scope cut.
- **Generative Expand needs a real prerequisite this model doesn't
  provide:** this app's canvas has always been a single fixed size (see
  the Artboard Tool's own documented scope cut), so there is no new
  canvas border to select and fill in the first place, independent of
  this model's own capability.

See `src-tauri/src/generative_fill.rs` for the real integration, and
`src-tauri/models/train_generative_fill/` for the exact scripts (and the
full real attribution list, inherited from `train_landscape/`) this
model was produced with.
