# Reproducing `tiny_restorer.onnx`

These are the exact scripts used to train and export the bundled Photo
Restoration model (see `../RESTORATION_NOTICE.md` for the full recipe
and its honest limitations). To reproduce the training run:

1. Fetch the real training photographs — the same 51 real,
   permissively-licensed photos from OpenCV's own `samples/data` used
   to train the Colorize and Style Transfer models — into a `raw/`
   directory next to these scripts. See `../train/README.md` for the
   exact file list and fetch command; it's the same set.

2. `pip install torch pillow numpy`
3. `python3 train.py` — trains `TinyRestorer` from random initial
   weights against real (degraded, clean) pairs built from those real
   photographs (a real degradation pipeline: Gaussian blur, additive
   Gaussian noise, real JPEG re-encoding — no pretrained checkpoint of
   any kind is loaded), saves `tiny_restorer.pt`.
4. `python3 export_onnx.py` — exports the just-trained weights to
   `tiny_restorer.onnx` (ONNX opset 13), the file bundled at
   `../tiny_restorer.onnx` and embedded into this project's Rust binary
   via `include_bytes!` in `src-tauri/src/restoration.rs`.
