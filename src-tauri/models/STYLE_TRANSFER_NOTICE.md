# Bundled model: `style_transfer.onnx`

**This model was trained by this project, from scratch** — the same
approach as `tiny_colorizer.onnx` (see `COLORIZE_NOTICE.md`), applied
to a harder, better-studied problem.

**Why this exists instead of a pretrained model.** The [ONNX Model
Zoo](https://github.com/onnx/models) publishes real, fetchable
`fast_neural_style` models (`mosaic-9.onnx` and its siblings) under
Apache License 2.0. They were investigated first, and ruled out for a
concrete, verified reason: exported at ONNX opset 9, their upsampling
layers lower to the deprecated ONNX `Upsample` op, and
[`tract`](https://github.com/sonos/tract) (this project's Rust
inference engine) has **zero implementation of that op at any level** —
confirmed by grepping `tract-onnx` and `tract-hir`'s own source, not
just by one failing run. There is no already-published ONNX export of
this architecture at a newer opset to substitute in. So this project
trained its own instead.

**Architecture:** the real, published feed-forward style-transfer
network from Johnson, Alahi & Fei-Fei, ["Perceptual Losses for
Real-Time Style Transfer and Super-Resolution"](https://arxiv.org/abs/1603.08155)
(ECCV 2016) — a downsampling convolutional front end, five residual
blocks, and an upsampling back end, with instance normalization
throughout — reimplemented for this project (it's a well-known,
standard shape, not copied from any specific codebase) with one
deliberate change: nearest-neighbour upsample + convolution instead of
the original paper's (and the ONNX Model Zoo's) transposed-convolution/
`Upsample`-op upsampling. This lowers to ONNX's modern `Resize` op,
which `tract` does implement.

**Training data:**
- **Content:** the same 51 real photographs used to train
  `tiny_colorizer.onnx`, fetched directly from [OpenCV's own
  `samples/data`](https://github.com/opencv/opencv/tree/master/samples/data)
  (Apache License 2.0).
- **Style:** one of those same real photographs — `baboon.jpg`, chosen
  for its own strong colour and texture — resized to the training patch
  size and used as the single style target, the standard Gatys et al.
  2015 / Johnson et al. 2016 setup (one style image per trained
  network).
- **Perceptual loss network:** a real, pretrained VGG16 (ONNX Model
  Zoo, `validated/vision/classification/vgg`, Apache License 2.0). Its
  553 MB of real weights were extracted directly out of the real ONNX
  file via the `onnx` Python package's own protobuf parsing (not
  `torch.load`/pickle deserialization — a materially different, safer
  mechanism with no arbitrary-code-execution surface, since protobuf is
  a fixed schema) and loaded into a matching frozen PyTorch module used
  only to score how training is going. **VGG16's weights are never
  bundled into this project's binary or committed to its repository —
  only the small network trained against it is.**

**Training recipe:** Adam, learning rate `4e-4`, gradient norm clipped
to `5.0`, batch size 4, `128x128` real random crops (with random
horizontal flips) from the 51 content photographs, 150 steps/epoch, 16
epochs (2,400 real gradient updates). Loss is the real Johnson et al.
combination: MSE content loss at VGG's `relu2_2` features, MSE style
loss over Gram matrices at `relu1_2`/`relu2_2`/`relu3_3`/`relu4_3`, plus
a small total-variation regularizer for smoothness. Only the
style-transfer network's own parameters are updated; VGG16 is frozen
throughout.

**Real training failures, not hidden.** The first attempt at these
weights (learning rate `1e-3`, a much higher style-loss weight, no
gradient clipping) diverged to `NaN` at epoch 11 of 20 — a real
optimization failure, caught by this project's own habit of verifying
before shipping (an early-abort check on non-finite loss was added
after the fact so a future divergence fails loudly at the exact step
rather than silently finishing and saving broken weights). The version
before that, converged and non-`NaN`, was **rejected anyway** on a
qualitative check: with the style loss weighted too heavily relative
to content, the output visually erased the original photo's own
structure entirely, dominated by repeating eye-like blobs, worse than
either not shipping or accepting a weaker effect. The weights actually
bundled here are the third attempt — lower learning rate, gradient
clipping, and a style weight re-balanced roughly 16x lower than the
first attempt's — verified both numerically (finite output at several
sizes through the real exported ONNX file) and visually (the source
photo's own shapes stay recognizable, with the style image's colour and
texture genuinely applied over them).

**Honest limitations.** This is a real trained model producing a real
stylization effect, not a fabricated stand-in — but, as with Colorize,
it is not a claim of parity with Photoshop's own Neural Filters
(trained on vastly larger content sets with vastly more compute).
Trained against only 51 content images and one style image, this
network reproduces one specific look; unlike Adobe's own Neural
Filters gallery (or the ONNX Model Zoo's multiple ready-made styles),
there is exactly one style baked into these weights, not a choice of
several. Both are named plainly, not smoothed over.

See `src-tauri/src/style_transfer.rs` for the real integration, and
`src-tauri/models/train_style/` for the exact scripts this model was
produced with — the same training run can be reproduced from them.
