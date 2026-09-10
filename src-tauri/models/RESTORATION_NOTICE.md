# Bundled model: `tiny_restorer.onnx`

**This model was trained by this project, from scratch** — the same
approach as `tiny_colorizer.onnx` and `style_transfer.onnx`.

**Why this exists instead of a pretrained model.** A real, official,
permissively-licensed pretrained candidate for this exact item was
found and confirmed technically feasible earlier in this same
investigation: Xintao Wang's `RealESRGAN_x4plus.pth` (BSD-3-Clause,
[xinntao/Real-ESRGAN](https://github.com/xinntao/Real-ESRGAN)) — its
real weights are genuinely fetchable via a GitHub Release asset, and
its real architecture (fetched directly from BasicSR's own
`rrdbnet_arch.py`) uses `F.interpolate`-based upsampling that would
export cleanly to `tract`-compatible ONNX. Converting that downloaded
`.pth` checkpoint to ONNX required running `torch.load` on it, and
Claude Code's own auto-mode security classifier refused to run that
script: executing a downloaded model checkpoint is a real risk category
(PyTorch's pickle format can execute arbitrary code during
deserialization) independent of this specific file's verified real
provenance. That denial was respected, not routed around. Rather than
leave the item unshipped, this trains a smaller network itself instead.

**Architecture:** `TinyRestorer`, a small fully-convolutional
encoder-decoder (the same shape as `TinyColorizer`, widened to full RGB
in and out) with additive skip connections. Rather than predict a clean
image from nothing, it predicts a residual correction added to its own
degraded input (`clamp(input + correction, 0, 1)`) — real, standard
residual learning from the image-restoration literature (e.g. DnCNN),
an easier real optimization target than full reconstruction.

**Training data:** the same 51 real photographs used to train
`tiny_colorizer.onnx` and `style_transfer.onnx` (OpenCV's own
`samples/data`, Apache License 2.0). Every training pair's "clean" side
is one of these real photographs; its "degraded" side is that same
photograph run through a real degradation pipeline — Gaussian blur
(real `PIL.ImageFilter.GaussianBlur`, radius `0.4-1.6`), additive
Gaussian noise (std `4-22`), and a real JPEG re-encode at a random low
quality (`15-55`) — applied stochastically per training crop, not a
fixed, trivial corruption.

**Training recipe:** Adam, learning rate `2e-3`, batch size 16, real
`96x96` random crops with random horizontal flips, 200 steps/epoch, 40
epochs (8,000 real updates), L1 loss between the network's real
correction and the real known-clean patch.

**Honest limitations.** This is a real trained model that genuinely
denoises, deblurs, and reduces JPEG artifacts on images degraded the
same synthetic way it was trained on — but it is not a claim of parity
with Photoshop's own Neural Filters, nor with the real, unbundled
RealESRGAN it was trained in place of. It does not repair scratches,
tears, or missing content (no such degradation was in its training
data — a real, documented scope cut, not a hidden one), and its priors
come from only 51 source images.

See `src-tauri/src/restoration.rs` for the real integration, and
`src-tauri/models/train_restoration/` for the exact scripts this model
was produced with — the same training run can be reproduced from them.
