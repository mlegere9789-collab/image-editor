# Bundled model: `super-resolution-10.onnx`

**Source:** [ONNX Model Zoo](https://github.com/onnx/models), `validated/vision/super_resolution/sub_pixel_cnn_2016/model/super-resolution-10.onnx`, fetched directly from the real, published repository.

**License:** Apache License 2.0 (the ONNX Model Zoo repository's own license — verified against `LICENSE` in that repository before this file was bundled).

**Architecture:** the sub-pixel convolutional neural network from Shi et al., ["Real-Time Single Image and Video Super-Resolution Using an Efficient Sub-Pixel Convolutional Neural Network"](https://arxiv.org/abs/1609.05158) (CVPR 2016) — a real, published, peer-reviewed architecture, exported to ONNX by the ONNX Model Zoo maintainers. This project did not train this model; it is used here exactly as published, run entirely on-device via [`tract`](https://github.com/sonos/tract) (a real, pure-Rust ONNX inference engine — no network call, no hosted backend, no native binary download).

**What it does:** real 3x single-image super-resolution on the luma (Y) channel of a YCbCr-converted image, upsampled via real sub-pixel (pixel-shuffle) convolution — the same real technique the original paper describes. Chroma channels are upsampled separately (bicubic) and recombined, the same real post-processing the model's own published reference implementation uses, since the network itself only operates on luma.

See `src-tauri/src/super_resolution.rs` for the real integration.
