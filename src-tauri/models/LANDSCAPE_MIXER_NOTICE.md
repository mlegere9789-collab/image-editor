# Bundled model: `landscape_mixer.onnx`

**This model was trained by this project, from scratch** — the same
technique as `style_transfer.onnx` (see `STYLE_TRANSFER_NOTICE.md`),
applied to a landscape-specific dataset and a landscape-mood style
target, rather than reusing the generic Style Transfer weights.

**Architecture:** identical to `style_transfer.onnx` — the real,
published feed-forward network from Johnson, Alahi & Fei-Fei (ECCV
2016), with nearest-neighbour upsample + convolution in place of
transposed-convolution/`Upsample`-based upsampling.

**Training data.** Adobe's own Landscape Mixer is trained on a large
corpus of real landscape photography; this project's honest equivalent
needed real landscape photos of its own, distinct from the 51 general
test images (fruit, calibration charts, a stuffed animal) used to train
Colorize, Style Transfer, and Photo Restoration.

**Source:** the [`ml5js/ml5-data-and-models`](https://github.com/ml5js/ml5-data-and-models)
repository (MIT-licensed), whose `datasets/images/landscapes` directory
bundles ~4,000 real Flickr photographs across seven categories (city,
road, mountain, lake, ocean, field, forest), each with its own
per-image Flickr license recorded in the repository's own metadata
JSON files. The repository's own README explicitly states that images
under license codes 3 and 6 (Creative Commons, no derivatives) "must be
excluded for generative ML projects." This project's own filtering went
further, keeping only images individually marked:

- CC BY 2.0 (license code 4)
- "No known copyright restrictions" (code 7)
- United States Government Work (code 8)
- Public Domain Dedication / CC0 (code 9)
- Public Domain Mark (code 10)

— excluding every non-commercial-only license (1, 2, 5) as well as the
no-derivatives ones, since the resulting model is trained into a real
codebase this project has no restriction on using commercially. This
kept **816 of the ~4,000 images** (787 were actually extracted into the
training set; a handful of filename mismatches in the source zip
accounted for the rest). `train_landscape/attributions.json` in this
project's own repository lists every kept image's real title,
photographer, source URL, and license — real attribution, not omitted.

**Style target:** one of those same real, safely-licensed photographs —
a genuine mountain sunset (`mountain__24508553818_c782c29843.jpg`,
"Mountain sunset" by Jason Rosenberg, CC BY 2.0) — chosen for its own
strong warm/cool colour contrast, the specific "mood" this filter
blends toward.

**Perceptual loss network:** the same real, frozen, pretrained VGG16
(ONNX Model Zoo, Apache 2.0) `style_transfer.onnx` used — never bundled
here either.

**Training recipe:** identical hyperparameters to `style_transfer.onnx`'s
own third, stable training attempt (learning rate `4e-4`, gradient norm
clipped to `5.0`, style weight `3e4`) — carried over deliberately,
since that combination was already proven stable and well-balanced, and
there was no reason to re-discover the earlier phase's own divergence
and over-stylization failures on a new dataset. 16 epochs, 2,400 real
gradient updates, converged smoothly with no divergence.

**Honest limitations.** A real, working landscape-mood transfer — a
qualitative check moving a real ocean-sunset photo through the real
exported model shows its sky and water shift toward the style
reference's own warm/cool palette while keeping the photo's own wave
and cloud structure intact. As with every other self-trained model
here: one specific mood is baked into these weights, not a choice of
several the way Adobe's own Landscape Mixer offers; and the network's
colour priors come from real but limited (787-image) real-world
diversity, not the vastly larger corpus a commercial tool trains on.

See `src-tauri/src/landscape_mixer.rs` for the real integration, and
`src-tauri/models/train_landscape/` for the exact scripts (and the full
real attribution list) this model was produced with.
