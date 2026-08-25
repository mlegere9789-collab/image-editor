# image-editor

Desktop image editor, Tauri + Rust + React.

## Phase 0

Open a PNG and look at it. That is the whole scope of this milestone — it exists to
prove the Tauri ↔ Rust ↔ React wiring end to end before any editing features land.

- **Open PNG…** in the toolbar opens a native file picker.
- Dragging a `.png` onto the window does the same thing.
- The image is drawn over a checkerboard so transparency reads correctly.
- The status bar shows file name, pixel dimensions, and file size.
- Bad input (a missing file, a non-PNG with a `.png` name) shows an inline error
  instead of a blank window.

`samples/sample.png` is a 640×400 test image with a gradient, a grid, and soft
transparent edges — handy for confirming both scaling and alpha are right.

## Prerequisites

- **Node.js** 18+ and npm — https://nodejs.org
- **Rust** (stable) — https://rustup.rs
- **Platform toolchain** for Tauri v2:
  - **macOS** — Xcode Command Line Tools: `xcode-select --install`
  - **Windows** — [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
    and [WebView2](https://developer.microsoft.com/microsoft-edge/webview2/)
    (preinstalled on Windows 11)
  - **Linux** — `libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev patchelf libxdo-dev libssl-dev`

  The full per-platform list lives at https://tauri.app/start/prerequisites/

## Run it

```bash
npm install        # once
npm run tauri:dev
```

The first launch compiles the Rust dependency tree and takes a few minutes; later
launches start in seconds. A desktop window opens — click **Open PNG…** and pick
`samples/sample.png`.

Editing anything under `src/` hot-reloads the window. Editing anything under
`src-tauri/src/` recompiles and restarts the app automatically.

## Build a distributable

```bash
npm run tauri:build
```

Installers are written to `src-tauri/target/release/bundle/` (`.dmg` on macOS,
`.msi`/`.exe` on Windows, `.deb`/`.AppImage` on Linux).

## Tests

```bash
cd src-tauri && cargo test    # Rust: image loading and its error paths
npm run build                 # frontend: typecheck + production build
```

## Layout

```
index.html            Vite entry point
src/
  App.tsx             UI: toolbar, drop target, canvas, status bar
  main.tsx            React root
  styles.css
src-tauri/
  src/lib.rs          `load_image` command + tests
  src/main.rs         desktop entry point
  tauri.conf.json     window, bundle, and CSP config
  capabilities/       per-window permission grants
samples/sample.png    test image
```

## How an image gets on screen

1. React calls the `dialog` plugin's `open()`, or receives a path from a drag-drop
   event, and hands the path to the Rust `load_image` command.
2. `load_image` stats the file, reads it, decodes the PNG header with the `image`
   crate to get dimensions and to confirm it really is a PNG, then returns the
   file's **original bytes** as a base64 `data:` URL.
3. React puts that URL in an `<img>`.

Sending the pixels as a data URL keeps Phase 0 to one round trip with no custom
protocol to configure, at the cost of holding a base64 copy in the webview — which
is why `load_image` refuses files over 64 MB. Swapping this for Tauri's asset
protocol is the natural move when the editor starts handling large files.
