# Reproducing `style_transfer.onnx`

These are the exact scripts used to train and export the bundled Style
Transfer model (see `../STYLE_TRANSFER_NOTICE.md` for the full recipe
and its honest limitations). To reproduce the training run:

1. Fetch the real training photographs — the same 51 real,
   permissively-licensed photos from OpenCV's own `samples/data` used
   to train the Colorize model — into a `raw/` directory next to these
   scripts. See `../train/README.md` for the exact file list and
   fetch command; it's the same set.

2. Fetch the real, official VGG16 ONNX Model Zoo file used as the
   frozen perceptual-loss network (Apache License 2.0, **553 MB** —
   used only transiently during training, never bundled or committed):

   ```sh
   curl -L -o vgg16.onnx \
     "https://media.githubusercontent.com/media/onnx/models/main/validated/vision/classification/vgg/model/vgg16-7.onnx"
   ```

3. `pip install torch onnx pillow numpy`
4. `python3 train_style.py` — trains `TransformNet` from random initial
   weights (no pretrained checkpoint of the transform network itself is
   loaded; VGG16's real weights are loaded read-only, frozen, and used
   only to score training progress), saves `style_net.pt`.
5. `python3 export_style_onnx.py` — exports the just-trained weights to
   `style_transfer.onnx` (ONNX opset 13), the file bundled at
   `../style_transfer.onnx` and embedded into this project's Rust
   binary via `include_bytes!` in `src-tauri/src/style_transfer.rs`.
