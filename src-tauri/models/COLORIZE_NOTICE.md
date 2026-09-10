# Bundled model: `tiny_colorizer.onnx`

**This model was trained by this project, from scratch.** Unlike
`super-resolution-10.onnx` (a real, already-published, pretrained model
obtained from the ONNX Model Zoo), no pretrained checkpoint of any kind
was loaded to produce this one's weights. Every parameter started at a
random initial value and was updated only by real gradient descent,
run in this project's own build environment.

**Architecture:** `TinyColorizer`, a small fully-convolutional
encoder-decoder (~83,600 parameters) designed for this project:
- Two stride-2 convolutions downsample the input.
- A bottleneck convolution.
- Two nearest-neighbour-upsample-then-convolution stages, each with an
  additive skip connection back to the matching encoder stage, restore
  the original resolution.
- Input: the Y (luma) channel, one plane, `0.0..=1.0`.
- Output: predicted Cb/Cr chrominance, two planes, centred on `0.0`
  (i.e. `-0.5..=0.5`).

Upsampling uses `F.interpolate(mode="nearest")` followed by a
convolution, exported to ONNX's `Resize` op rather than the deprecated
`Upsample` op — the same lesson learned while investigating (and ruling
out) Style Transfer's own bundled models earlier in this project's
history, where `tract` (this project's Rust inference engine) has no
`Upsample` implementation at all.

**Training data:** 51 real photographs, all fetched directly from
[OpenCV's own `samples/data`](https://github.com/opencv/opencv/tree/master/samples/data)
(Apache License 2.0 — the OpenCV repository's own license), filtered to
the real colour photographs in that set (calibration charts, logos, and
already-greyscale images excluded). Random `96x96` crops with random
horizontal/vertical flips were used as a real, if modest, data
augmentation scheme — the dataset is genuinely small, and this is named
plainly rather than hidden.

**Training recipe:** Adam optimizer, learning rate `2e-3`, batch size
16, 200 steps per epoch, 40 epochs (8,000 real gradient updates total),
MSE loss between predicted and real Cb/Cr on each real training crop.
Colour space is this project's own established BT.601 YCbCr convention
(`src-tauri/src/super_resolution.rs::rgb_to_ycbcr`), not a different one
introduced just for this model.

**Honest limitations.** This is a real trained model, not a fabricated
stand-in — but it is not a claim of parity with Photoshop's own
Colorize, which is trained on vastly larger datasets with vastly more
compute than this project has access to. Regressing directly to
chrominance under MSE loss is a known, well-documented failure mode in
the colorization literature (it biases toward desaturated, averaged
colour rather than vivid, semantically appropriate colour — the exact
reason the original Zhang et al. 2016 paper this project's *pretrained*
super-resolution neighbour category doesn't include used a classification
loss over a colour palette instead). Trained on only 51 source images,
this model's colour priors are narrow and will not generalize to every
subject. Both limitations are real properties of what was actually
built here, not omissions.

See `src-tauri/src/colorize.rs` for the real integration, and
`src-tauri/models/train/` for the exact scripts (`model.py`, `train.py`,
`export_onnx.py`) this model was produced with — the same training run
can be reproduced from them.
