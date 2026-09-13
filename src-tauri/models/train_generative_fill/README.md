# Reproducing `generative_fill.onnx`

These are the exact scripts used to train and export the bundled
Generative Fill model (see `../GENERATIVE_FILL_NOTICE.md` for the full
recipe and its honest limitations). To reproduce the training run:

1. Fetch the real training photographs: the same 787 real landscape
   photos `../train_landscape` already fetched (`attributions.json` in
   this directory is copied from there — the same 816 kept images, same
   real per-image title/photographer/source URL/license).

   ```sh
   curl -L -o landscapes_small.zip \
     "https://raw.githubusercontent.com/ml5js/ml5-data-and-models/master/datasets/images/landscapes/landscapes_small.zip"
   python3 - <<'PY'
   import json, zipfile, os
   attrs = json.load(open("attributions.json"))
   os.makedirs("raw", exist_ok=True)
   z = zipfile.ZipFile("landscapes_small.zip")
   for a in attrs:
       member = f"{a['category']}/{a['file'].split('__', 1)[1]}"
       try:
           with z.open(member) as src, open(f"raw/{a['file']}", "wb") as dst:
               dst.write(src.read())
       except KeyError:
           pass
   PY
   ```

2. Fetch the real, official VGG16 ONNX Model Zoo file used as the
   frozen perceptual-loss network (Apache License 2.0, 553 MB — used
   only transiently during training, never bundled or committed):

   ```sh
   curl -L -o vgg16.onnx \
     "https://media.githubusercontent.com/media/onnx/models/main/validated/vision/classification/vgg/model/vgg16-7.onnx"
   ```

3. `pip install torch onnx pillow numpy`
4. `python3 train_generative_fill.py` — trains `InpaintNet` from random
   initial weights (no pretrained checkpoint of any kind is loaded)
   against real (photo, random-hole-mask) pairs built entirely from
   those real photographs, self-supervised, saves `inpaint_net.pt`.
5. `python3 export_generative_fill_onnx.py` — exports the just-trained
   weights to `generative_fill.onnx` (ONNX opset 13), the file bundled
   at `../generative_fill.onnx` and embedded into this project's Rust
   binary via `include_bytes!` in `src-tauri/src/generative_fill.rs`.
