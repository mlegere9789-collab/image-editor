# Reproducing `tiny_colorizer.onnx`

These are the exact scripts used to train and export the bundled
Colorize model (see `../COLORIZE_NOTICE.md` for the full recipe and its
honest limitations). To reproduce the training run:

1. Fetch the real training photographs (51 real, permissively-licensed
   photos from OpenCV's own `samples/data`, Apache License 2.0) into a
   `raw/` directory next to these scripts — this project does not
   commit those source images here to keep the repository small; they
   are fetched fresh, e.g.:

   ```sh
   mkdir raw && cd raw
   for f in baboon.jpg fruits.jpg building.jpg apple.jpg orange.jpg \
            home.jpg butterfly.jpg squirrel_cls.jpg HappyFish.jpg \
            blox.jpg board.jpg aero1.jpg aero3.jpg messi5.jpg \
            leuvenA.jpg leuvenB.jpg cards.png smarties.png \
            chicky_512.png basketball1.png basketball2.png \
            rubberwhale1.png rubberwhale2.png aloeL.jpg aloeR.jpg \
            left01.jpg left02.jpg left03.jpg left04.jpg left05.jpg \
            left06.jpg left07.jpg left08.jpg left09.jpg left11.jpg \
            left12.jpg left13.jpg left14.jpg right01.jpg right02.jpg \
            right03.jpg right04.jpg right05.jpg right06.jpg right07.jpg \
            right08.jpg right09.jpg right11.jpg right12.jpg right13.jpg \
            right14.jpg; do
     curl -sL -o "$f" "https://raw.githubusercontent.com/opencv/opencv/master/samples/data/$f"
   done
   cd ..
   ```

2. `pip install torch pillow numpy`
3. `python3 train.py` — trains `TinyColorizer` from random initial
   weights (no pretrained checkpoint of any kind is loaded), saves
   `tiny_colorizer.pt`.
4. `python3 export_onnx.py` — exports the just-trained weights to
   `tiny_colorizer.onnx` (ONNX opset 13), the file bundled at
   `../tiny_colorizer.onnx` and embedded into this project's Rust binary
   via `include_bytes!` in `src-tauri/src/colorize.rs`.
