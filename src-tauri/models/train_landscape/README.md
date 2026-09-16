# Reproducing `landscape_mixer.onnx`

These are the exact scripts used to train and export the bundled
Landscape Mixer model (see `../LANDSCAPE_MIXER_NOTICE.md` for the full
recipe and its honest limitations). To reproduce the training run:

1. Fetch the real training photographs: 787 real landscape photos
   sourced from Flickr via the `ml5js/ml5-data-and-models` repository's
   "landscapes" dataset (MIT-licensed repository — the dataset's own
   README states each image carries its own Flickr license, and
   explicitly calls out that images under licenses 3 and 6 (no
   derivatives) must be excluded for generative ML use). `attributions.json`
   in this directory lists exactly which 816 images (of the ~4,000 in
   the full set) this project kept — every one individually marked CC
   BY 2.0, CC0, "no known copyright restrictions", or a US Government
   Work — with each image's real title, photographer, and source URL.

   ```sh
   curl -L -o landscapes_small.zip \
     "https://raw.githubusercontent.com/ml5js/ml5-data-and-models/master/datasets/images/landscapes/landscapes_small.zip"
   python3 - <<'PY'
   import json, zipfile, shutil, os
   attrs = json.load(open("attributions.json"))
   os.makedirs("raw", exist_ok=True)
   z = zipfile.ZipFile("landscapes_small.zip")
   for a in attrs:
       member = f"{a['category']}/{a['file'].split('__', 1)[1]}"
       try:
           with z.open(member) as src, open(f"raw/{a['file']}", "wb") as dst:
               shutil.copyfileobj(src, dst)
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
4. `python3 train_landscape.py` — trains `TransformNet` from random
   initial weights (no pretrained checkpoint of the transform network
   itself is loaded) against the real landscape photographs and the
   real mountain-sunset style target, saves `style_net.pt`.
5. `python3 export_landscape_onnx.py` — exports the just-trained
   weights to `landscape_mixer.onnx` (ONNX opset 13), the file bundled
   at `../landscape_mixer.onnx` and embedded into this project's Rust
   binary via `include_bytes!` in `src-tauri/src/landscape_mixer.rs`.
