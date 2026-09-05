# image-editor

Desktop image editor, Tauri + Rust + React.

## Status

- **Phase 0** — Tauri + Rust + React shell that opens and displays a PNG. *Done.*
- **Phase 1** — the document model and compositor. *Done, described below.*
- **Phase 2** — composite delivery over a custom protocol instead of base64 IPC.
  *Done, described below.*
- **Phase 3** — brush and eraser tools: the first per-pixel edits. *Done,
  described below.*
- **Phase 4** — **Export PNG…**: the app can finally save what you made.
  *Done, described below.*
- **Phase 5** — undo/redo. *Done, described below.*
- **Phase 6** — dirty-region recompositing: a stroke only recomposites the
  pixels it touched. *Done, described below.*
- **Phase 7** — **Save Project… / Open Project…**: a layered project file
  format that round-trips the full editable document, not just a flattened
  PNG. *Done, described below.*
- **Phase 8** — **New…**: start a blank document at a chosen size instead of
  needing to open a file first. *Done, described below.*
- **Phase 9** — **Rect Select / Ellipse Select / Select All / Invert /
  Reselect**: the first selection tools — paint/erase strokes are now
  confined to the active selection, which can cover the whole canvas, be
  inverted, or be restored after deselecting. *Done, described below. Part
  of a much larger [full-parity roadmap](docs/PHOTOSHOP_PARITY.md) — see
  that file for what's next.*
- **Phase 10** — **Lock / Merge Visible / Flatten Image / Merge Down /
  Eyedropper / Paint Bucket / Gradient**: a per-layer toggle that blocks
  paint/erase strokes onto that layer's pixels, three ways to collapse the
  layer stack, a tool that picks up the color under the pointer, one that
  flood-fills a connected region with it, and one that blends between two
  colors along a dragged line. *Done, described below.*

## Phase 1: document model and compositor

A document is a stack of layers. Each layer owns document-sized RGBA8 pixel data
plus **opacity**, a **blend mode**, and a **visibility** flag. The compositor
flattens the stack, bottom to top, into the single image on screen.

- **Open PNG…** starts a new document with that image as its only layer.
- **Add layer…** (or dropping a second file) stacks another image on top. The
  document keeps its original size: smaller images are pasted at the origin,
  larger ones are clipped.
- The layers panel lists the stack top-first. Per layer you can toggle
  visibility, set opacity, choose a blend mode, reorder, and delete.
- Every edit re-flattens in Rust and returns the new composite, so what you see
  is always the compositor's output rather than anything the browser stacked.

### Blend modes

The twelve **separable** modes from the W3C compositing spec: Normal, Multiply,
Screen, Overlay, Darken, Lighten, Color Dodge, Color Burn, Hard Light, Soft
Light, Difference, Exclusion.

The four non-separable modes (Hue, Saturation, Color, Luminosity) need all three
channels at once and are not implemented.

### Compositing math

For a source with alpha `as` over a backdrop with alpha `ab`, per channel:

```text
Cs' = (1 - ab) * Cs + ab * B(Cb, Cs)          // blend against the backdrop
ao  = as + ab * (1 - as)                      // source-over alpha
Co  = (as * Cs' + ab * Cb * (1 - as)) / ao    // back to non-premultiplied
```

`as` is the layer's own per-pixel alpha multiplied by its opacity. Accumulation
runs in `f32` with non-premultiplied alpha and quantizes to `u8` once at the end,
so a tall stack does not accumulate rounding error.

Two consequences worth knowing:

- Over a fully transparent backdrop every blend mode shows the source unchanged —
  there is nothing to blend against.
- Pixels that end up fully transparent are emitted as `[0, 0, 0, 0]`. Colour under
  zero alpha is invisible, so it is not carried into the composite even when the
  source layer stored something there.

### Samples

`samples/sample.png` (640×400) is a gradient with a grid and soft transparent
edges. `samples/rings.png` is a matching-size ring pattern on a transparent
surround. Open the first and add the second to see blend modes at work.

## Phase 2: composite delivery

Phase 1 shipped the flattened composite as a `data:image/png;base64,…` string
inside every command's JSON response — simple, but base64 inflates the bytes
by a third and the whole thing rides the same IPC channel as everything else.

Phase 2 replaces that with a `composite://` custom protocol registered on the
Tauri app. Each edit still re-flattens and PNG-encodes in Rust, but the raw
bytes are cached in `AppState` behind a generation counter instead of encoded
into the response; the command now returns just that counter. The frontend
points its `<img>` at `composite://composite.png?g=<generation>` and lets the
webview fetch the bytes directly as a normal image request — no base64, no
JSON string, no size limit from what IPC can carry as text.

This is the transport half of the "worth flagging" note from Phase 1.
Recompositing only the dirty region instead of the whole document on every
edit is still future work here — there's no per-pixel edit tool yet to make a
"dirty region" mean anything narrower than "the whole layer" (Phase 6 adds
that once Phase 3 gives it something to be dirty about).

## Phase 3: brush and eraser

Phases 1 and 2 only ever replaced or recomposited whole layers; nothing let you
touch an individual pixel. Phase 3 adds that: a **Brush** and an **Eraser**,
selected from the toolbar, that paint or erase on the selected layer wherever
you drag across the canvas.

- `Document::stroke` (`src-tauri/src/document.rs`) applies a tool along a
  polyline of document-pixel coordinates, onto one layer's own pixels — not
  the composite. Consecutive points are joined into capsule-shaped segments
  (a point-to-segment distance test per pixel in the stroke's bounding box),
  so a fast drag has no gaps between samples, with a soft 1px edge rather
  than a hard-aliased circle.
- Coverage from segments that overlap **within one call** is taken as a
  maximum, not summed — a stroke that briefly doubles back on itself (a tight
  curve, a corner) does not paint or erase that overlap twice as hard as the
  rest of the stroke.
- The **Brush** paints an RGBA colour with normal, `source-over` blending —
  the same math `composite.rs` uses to stack layers, applied here to a
  layer's own pixels instead of the accumulated backdrop.
- The **Eraser** multiplies existing alpha down toward zero rather than
  painting; colour is left alone, since a fully transparent pixel's colour is
  invisible and not otherwise meaningful.
- The frontend (`App.tsx`) tracks a pointer drag across the canvas `<img>`,
  converts each event to document-pixel coordinates from the element's
  bounding rect, and sends just the segment since the last point — the
  `paint_stroke` / `erase_stroke` commands — once per pointer move. Each
  call's own bounding box (and the coverage work behind it) stays small
  regardless of how long the drag has run; the stroke is many small edits,
  not one command holding a growing point list.

One thing this does *not* do yet: recomposite only the dirty region instead
of the whole document on every stroke segment (Phase 2's deferred note — now
that there's an actual per-pixel edit tool, this is the next natural
candidate). Phase 6 adds it.

## Phase 4: exporting

Every prior phase could open, edit, and preview a document, but nothing wrote
the result back to disk — editing something with no way to save it is not
yet an editor. **Export PNG…** closes that gap: it flattens the open document
and writes it to a `.png` file at a path chosen through the OS save dialog.

- `export()` (`src-tauri/src/lib.rs`) is the same `flatten` + `png::encode`
  pipeline every edit already runs to refresh the on-screen composite, just
  written to a file instead of cached for the `composite://` protocol. It
  reads the open document; it does not touch it, so unlike every other
  command there is no new `Snapshot` — success or an error string is all the
  frontend gets back.
- `export_png` needed a new capability, `dialog:allow-save`, alongside the
  `dialog:allow-open` Phase 0 already granted for **Open PNG…**.

Exporting is deliberately a flattened PNG, not a save of the editable
document (layers, blend modes, opacity): the app's only file format so far
is PNG, on both the read and write side, and a project format able to round
trip the full layer stack is a bigger, separate piece of scope than "make
the button that writes a file exist." Phase 7 adds that format.

## Phase 5: undo and redo

Every edit so far was one-way — a mistake meant reopening the file. Phase 5
adds **Undo** and **Redo** (toolbar buttons, and Ctrl/Cmd+Z /
Ctrl/Cmd+Shift+Z / Ctrl+Y), backed by whole-document snapshots kept in Rust.

- `AppState` gained a `history: Mutex<History>` — two `VecDeque<Document>`
  stacks, `undo` and `redo`, each bounded at 50 entries (`MAX_HISTORY`; the
  oldest entry drops off rather than growing forever). Every mutating
  command's `Snapshot` now also carries `canUndo`/`canRedo`, so the toolbar
  buttons enable and disable themselves without a separate query.
- **Checkpointing is gesture-granular, not call-granular.** A brush stroke or
  an opacity drag sends many small IPC calls (one per pointer move); auto-
  checkpointing each one would fragment a single stroke into dozens of undo
  steps. Instead the frontend calls a dedicated `checkpoint` command once, at
  the *start* of a gesture, and the gesture's own edit commands
  (`paint_stroke`, `erase_stroke`, `set_layer_opacity`) use a plain,
  non-checkpointing `edit()` helper. Discrete one-shot commands (add a layer,
  toggle visibility, change blend mode, reorder, delete) checkpoint
  themselves via `edit_checkpointed()`.
- A new checkpoint clears the redo stack — standard editor semantics: you
  cannot redo past a new edit. Opening a new document resets history
  entirely, rather than letting you undo into whatever was open before.
- **A real async-ordering bug, found only by live testing.** The original
  `handlePointerDown` fired `checkpoint()` and `applyStroke(...)` back to
  back with no `await` between them. Two `invoke()` calls issued in the same
  synchronous tick have no guaranteed processing order on the Rust side —
  each becomes an independent async task racing for the same
  `std::sync::Mutex` — so the paint command's response sometimes overwrote
  the frontend's undo-state before the checkpoint's own response landed,
  leaving Undo visibly stuck disabled after a stroke. Rust unit tests could
  never catch this (they call `perform_undo`/`push_checkpoint` synchronously,
  with no IPC involved); only interactive testing under Xvfb surfaced it. The
  fix makes the ordering explicit: `checkpoint().then(() => applyStroke(...))`.

## Phase 6: dirty-region recompositing

Every edit through Phase 5 re-flattened the *entire* document, every time —
including once per pointer-move during a brush stroke, dozens of times over
one drag. Deferred at the end of Phase 2 as future work, and again at the end
of Phase 3 once there was finally a per-pixel edit tool to make "dirty
region" mean something narrower than "the whole layer."

- `composite::flatten` is unchanged in signature and behaviour — same input,
  same output — but its blend math was factored into a new `composite_pixel`
  function that composites exactly one pixel. A new `composite::recomposite_region`
  reuses that same function over just a `Rect` (a bounding box, already
  clamped to the document) instead of the whole image, writing into an
  existing full-size buffer rather than allocating a fresh one. `flatten` and
  `recomposite_region` sharing one blend implementation means there is
  exactly one place for that math to be correct, not two copies that could
  quietly drift apart.
- `Document::stroke` already computed its own touched bounding box internally
  (to size its coverage buffer); it now returns that box (`Option<Rect>`,
  `None` for an empty or entirely-off-canvas stroke) instead of discarding it.
- `AppState`'s composite cache now holds the raw RGBA pixel buffer, not just
  the PNG-encoded bytes. `snapshot()` takes an `Option<Rect>`: given one, and
  a cached buffer whose dimensions match the current document, it patches
  just that rect via `recomposite_region` instead of calling `flatten`.
  Every edit that is *not* a stroke (opacity, visibility, blend mode, adding/
  removing/reordering a layer) can change any pixel in the composite, so
  those still pass `None` and get a full flatten — as does undo/redo, and
  opening a document (which also replaces the cache outright, so a
  differently-sized new image can never be patched against a stale buffer).
- The PNG is still re-encoded from the full buffer on every edit either way —
  encoding was never the expensive part. What a dirty stroke segment now
  skips is the O(width × height × layers) blend loop over pixels nothing
  touched; for a small brush radius on a normal-sized canvas, a stroke
  segment's rect is a small fraction of the total pixel count.

**Verified two ways.** `composite.rs` gained tests asserting a region
recomposite matches a full flatten *inside* the rect and leaves pixels
*outside* it untouched (with a sentinel value nothing real could produce, so
any stray write is unmistakable); `lib.rs` gained tests for `snapshot`'s
three paths — a region patch, the no-cache-yet fallback, and the
dimension-mismatch fallback. Every one of Phase 1-5's existing tests also
still passes unchanged, which is what confirms the `flatten` refactor
(reordering the blend loop from layer-outer/pixel-inner to
pixel-outer/layer-inner, so it could share `composite_pixel` with the region
path) produces identical output to before — not just similar, bit-identical,
pixel for pixel. Live under Xvfb: painted two separate strokes across an
open document and confirmed both rendered correctly with the background
gradient untouched around them, then undid both and confirmed the canvas
returned to its pristine state — the full-flatten fallback undo/redo already
used stays correct alongside the new region path.

## Phase 7: project files

**Export PNG…** (Phase 4) only ever wrote the *flattened* composite —
opening that file back up gives you a single fresh layer, not the document
you actually built. **Save Project…** / **Open Project…** close that gap: a
project file round-trips the full editable document — layer order, name,
visibility, opacity, blend mode, and each layer's own pixels — so closing
the app mid-edit and reopening the project picks up exactly where you left
off.

- `src-tauri/src/project.rs` is a small custom format rather than a second
  pixel codec or a pulled-in archive library: a 5-byte magic
  (`b"IEDP1"`), a length-prefixed JSON manifest (document size, and each
  layer's name/visibility/opacity/blend mode/PNG byte length, in stack
  order), followed by each layer's own pixels — PNG-encoded independently
  and concatenated in that same order. Reusing the PNG codec already in
  `png.rs` keeps a project file a similar order of magnitude to the images
  it's built from, instead of a document-sized raw RGBA8 buffer per layer.
- `png.rs` gained `decode_bytes`/`encode_pixels` — the parts of `read`/
  `encode` that work on in-memory bytes rather than a filesystem path,
  needed because a project file's layers are embedded, not one-PNG-per-file.
  `read` and `encode` now just call them, unchanged in behaviour.
- `save_project` / `open_project` (`src-tauri/src/lib.rs`) mirror
  `export_png` / `open_document`: saving reads the open document without
  mutating it (no `Snapshot` to return, like `export_png`); opening replaces
  whatever document was open and starts fresh undo/redo history (like
  `open_document`) — factored into a shared `replace_open_document` helper
  rather than duplicated between the two.
- The frontend adds **Open Project…** / **Save Project…** toolbar buttons,
  filtered to a new `.iep` extension, alongside the existing PNG open/export
  pair.

**Verified two ways.** `project.rs` gained tests for a full round trip
(multiple layers, reordered, with non-default opacity/blend-mode/visibility
all preserved), an empty document, and every truncation/corruption path
(wrong magic, a manifest or a layer's PNG bytes cut short, a layer whose
decoded size doesn't match the document) each producing a clear error rather
than a panic or silent data loss. Live under Xvfb: opened a document, added
a second layer, set it to 60% opacity and Multiply, saved a project file,
then reloaded it — the reloaded document showed exactly two layers (not
duplicated — an early version of the verification probe raced under React
StrictMode's double-effect in dev and *did* duplicate them, caught before
this ever reached real code) with the opacity, blend mode, and composite all
matching what was saved.

## Phase 8: new document

Every prior phase needed a file to already exist — **Open PNG…** or
**Open Project…**. **New…** starts a blank document at a size you choose, the
same size a canvas app on any platform lets you start at, with one
fully-transparent layer ready to paint on immediately.

- `create_new_document` (`src-tauri/src/lib.rs`) builds a `Document` of the
  requested size with a single blank `"Layer 1"`, then hands it to
  `replace_open_document` — the same helper `open_document` and
  `open_project` already use — so **New…** replaces whatever was open and
  resets undo/redo history exactly like opening a file does. Kept as a plain
  function separate from its `#[tauri::command]` wrapper `new_document` so it
  can be unit-tested directly, the same pattern as `export`.
- A blank canvas has no file size to bound it, so it needed its own limit:
  `MAX_NEW_DOCUMENT_BYTES` (64 MB) rejects a request before allocating a
  buffer that large, the same order of magnitude as `png::MAX_FILE_BYTES`
  already bounds an opened PNG to.
- The frontend adds a **New…** toolbar button (first in the row, disabled
  while `busy` like every other action) that opens a small modal — Width and
  Height number inputs (1–8000, matching the backend's practical range),
  Cancel, and Create. Create is disabled until both fields hold a positive
  number, and closing the modal (Cancel, or clicking the overlay) discards
  the values without calling the backend.

**Verified two ways.** `lib.rs` gained tests for a normal blank document (one
layer, correct size, all-zero pixels), rejecting zero width or height,
rejecting a canvas over the 64 MB limit, and — the one that matters most —
that creating a new document while one is already open replaces it and
clears undo/redo (checkpoint a document, confirm `can_undo`, create a new
one, confirm both `can_undo` and `can_redo` come back false). Live under
Xvfb: opened the bundled sample image, confirmed it painted and undid
normally, then used **New…** to create a 100×100 document — the sample's
layer was gone, replaced by a single blank "Layer 1" at the new size, and
Ctrl+Z (undo) was a no-op, matching the unit test's behavior in the real
running app.

## Phase 9: selection tools

Every stroke through Phase 8 touched the whole layer — there was no way to
say "only paint in this part of the canvas." **Rect Select** and **Ellipse
Select** add that: a selection confines every subsequent brush/eraser
stroke to its bounds, the same way Photoshop's marquee tools do. This is
the first item off the [full-parity roadmap](docs/PHOTOSHOP_PARITY.md) —
see that file for the ~590-item backlog this phase and every one after it
draws from.

- `Selection` (`src-tauri/src/document.rs`) is a shape (`Rectangle` or
  `Ellipse`) plus a bounding `Rect`, not a document-sized mask — cheap to
  copy out of `Document` on every `stroke()` call, and exact for these two
  shapes. `Document::select_rectangle`/`select_ellipse` normalize the two
  drag corners (sorted, clamped to the canvas, rejecting a zero-area drag)
  into that bounds rect; `deselect` clears it. `None` means no selection —
  the same as Photoshop's "nothing selected" state — and every stroke stays
  unrestricted.
- `Document::stroke` copies the selection out before borrowing the target
  layer mutably, then zeroes a pixel's coverage whenever
  `Selection::contains` says that pixel center falls outside the bounds (or
  outside the inscribed ellipse, for the ellipse shape) — one extra check in
  the same per-pixel loop the brush/eraser coverage math already runs, no
  separate confinement pass.
- Three new commands — `select_rectangle`, `select_ellipse`, `deselect` —
  are thin `edit_checkpointed` wrappers, the same pattern every other
  one-shot command in `lib.rs` already uses; `DocumentView` gained a
  `selection` field so the frontend can draw the outline.
- The frontend adds a **Selection tool** toolbar group (Rect Select, Ellipse
  Select, Deselect — also bound to Ctrl/Cmd+D) alongside the existing Paint
  tool group. A marquee drag tracks a live local `{start, current}` preview
  (no IPC per pointer move, unlike a brush stroke) and commits with a single
  `select_rectangle`/`select_ellipse` call on release; a click with no drag
  is silently a no-op rather than round-tripping to the backend just to
  surface its "must cover at least one pixel" error. The outline itself is
  a `mix-blend-mode: difference` dashed overlay animated into a marching-ants
  pattern, so it stays visible over any canvas content in either theme.

**A real regression, caught only by testing.** Wrapping the canvas `<img>`
in a positioning `<div>` for the selection overlay caused WebKitGTK to
render its own native "image selected" highlight — a solid color tint over
the *entire* image — on any click-drag, unrelated to this app's own
selection state entirely. `user-select: none` / `-webkit-user-drag: none`
on the wrapper and image fixed it. Caught by live interaction, not unit
tests, since nothing about the pixel data was wrong — the composite itself
was correct underneath the browser-native overlay.

**Verified two ways.** `document.rs` gained tests for both selection shapes
(clamping/sorting a drag's corners, rejecting a zero-area selection,
rejecting non-finite coordinates, `deselect` clearing it) and for
confinement itself: a rectangle selection confines a brush stroke to its
bounds, an ellipse selection excludes its own bounding-box corners while
accepting its center, an eraser stroke is confined the same way a brush
stroke is, and — the control case — a stroke with no active selection stays
completely unrestricted. Live: driving the marquee drag and the confined
paint through real xdotool pointer events under Xvfb turned out to be
unreliable in this sandbox (no window manager, and a multi-step drag
sequence occasionally left stuck ref state between commands) rather than
revealing an actual bug — confirmed by bypassing pointer simulation
entirely with a direct `invoke()` sequence (`new_document` →
`select_rectangle` → `checkpoint` → `paint_stroke`) through the real running
app: the resulting screenshot showed a stroke drawn across the full canvas
width but visibly painted *only* inside the selection's bounds, pixel-exact
with what the Rust confinement tests already predicted.

**Select All / Invert.** `Selection` gained one field, `inverted: bool`
(`Selection::contains` XORs shape-membership with it), rather than a new
representation — "the whole canvas minus a shape" is still exactly
expressible by flipping one boolean, no mask needed. `select_all` sets a
rectangle spanning the whole canvas; `invert_selection` flips `inverted` on
whatever selection is already active and errors ("Nothing is selected.") if
there isn't one, matching Photoshop's own Select > Inverse, which is
disabled rather than a no-op when nothing is selected. Both are
`edit_checkpointed` commands, bound to Ctrl/Cmd+A and Ctrl/Cmd+Shift+I. The
frontend draws a second marching-ants outline around the full canvas
whenever the active selection is inverted, alongside the shape's own
outline, so an inverted selection reads visually as "everywhere but this."

Verified the same two ways as the rest of this phase: `document.rs` gained
tests for `select_all`, for inverting with nothing selected being an error,
for a double-invert returning to the original selection, and for an
inverted selection confining a stroke to *outside* its bounds. Live,
through the real running app under Xvfb: single-click UI verification
(New…, Select All, Invert) confirmed the buttons enable/disable correctly
and the full-canvas outline appears; a direct `invoke()` trace of
`invert_selection` alone, added and removed as a temporary debug probe,
confirmed the Tauri command layer flips `inverted` correctly on a single
call — the apparent failure on the first attempt at this trace turned out
to be React StrictMode invoking the same effect twice in dev, calling
`invert_selection` twice and cancelling itself out, not a real bug.

**Reselect.** `Document` gained a second field, `last_selection`, kept
separate from the active `selection` rather than folded into it: `deselect`
moves whatever was active into `last_selection` before clearing it, and
`reselect` (Select > Reselect) restores it, erroring ("Nothing to
reselect.") if there isn't one — again matching Photoshop's own disabled
menu item rather than a no-op. Deliberately narrow: `last_selection` only
updates on `deselect`, not on every selection change, so reselecting after
replacing one selection with another (without deselecting first) is not
supported — the common case this serves is "I deselected and want that
exact selection back," not a full selection-history stack. `DocumentView`
exposes this as a plain `canReselect: bool` rather than leaking
`last_selection` itself, since the frontend only ever needs to know whether
the button should be enabled. Bound to Ctrl/Cmd+Shift+D — which meant
fixing the existing Deselect handler, which matched Ctrl/Cmd+D regardless
of Shift and would otherwise have eaten this shortcut too.

Verified the same two ways: `document.rs` gained tests for reselecting
with nothing ever deselected being an error (including right after making
a selection, without a deselect in between — reselect restores what
`deselect` cleared, not "whatever was ever selected"), for a deselect/
reselect round trip restoring the exact prior selection, and for
reselect being available again after a second deselect/reselect cycle.
Live, through the real running app under Xvfb: New… → Select All →
Deselect (outline disappears, Reselect changes from disabled to enabled)
→ Reselect (outline reappears, Deselect/Invert re-enable) — all four
single clicks, screenshotted at each step.

## Phase 10 — Lock / Merge Visible / Flatten Image / Merge Down / Eyedropper / Paint Bucket / Gradient / Single Row & Column Marquee / Expand & Contract Selection

`Layer` gains a `locked: bool` (Photoshop's "Lock image pixels" — the one
lock sub-mode that actually protects against the edits this app can make).
`Document::stroke` checks it right after resolving the target layer and
errors (`Layer "<name>" is locked.`) before doing any coverage math at all,
so a locked layer's pixels are provably untouched, not just visually
unchanged. Compositing — visibility, opacity, blend mode, stacking order —
is deliberately untouched by the flag: those aren't edits to the layer's
own pixel data, so locking a layer still lets you hide it, retime it, or
move it in the stack.

`set_layer_locked` is a new `edit_checkpointed` command, the same pattern
every other one-shot layer command uses. The frontend adds a lock checkbox
to each row in the layers panel, right next to the existing visibility
checkbox — checked state mirrors `LayerView.locked`, and a paint/erase
attempt against a locked layer surfaces the backend's error through the
same generic error banner every other command failure already uses, with
no special-casing needed.

Project files (`.iep`, Phase 7) round-trip `locked` too, alongside
visibility/opacity/blend mode — `LayerManifest.locked` is
`#[serde(default)]` so a project file saved before this phase, with no key
for it at all in its manifest JSON, still loads as unlocked rather than
failing to parse.

**Verified two ways.** `document.rs` gained tests for the default
(unlocked), for a locked layer rejecting a stroke outright (pixels
provably untouched, not just unchanged), and for unlocking restoring
normal painting. `project.rs` gained a round-trip test with a locked
layer, plus a dedicated test that hand-rewrites a saved project file's
manifest JSON to strip the `locked` key entirely (recomputing the u32
length prefix that precedes it) and confirms it still loads, unlocked —
proving the backward-compatibility path actually engages rather than just
trusting `#[serde(default)]` to do the right thing untested. Live under
Xvfb: New… → paint a dot (single click; `handlePointerDown` fires one
`paint_stroke` per click even with no drag) → check the lock checkbox →
paint again (blocked, `Layer "Layer 1" is locked.` banner, no new pixels)
→ uncheck lock → paint again (a second dot appears) — five single clicks,
screenshotted at each step.

**Merge Visible.** Collapses every visible layer into one, in place of the
layers it replaces — hidden layers stay exactly where they were, in their
original relative order. `composite.rs` gained `flatten_subset`, which
flattens an arbitrary set of layer indices rather than every contributing
layer; getting there meant factoring the blend accumulation itself out of
`flatten`/`recomposite_region` into a shared `composite_layers_pixel` that
just takes an iterator of layers, so `flatten`, `recomposite_region`, and
`flatten_subset` all still share the one place that math lives, the same
principle Phase 6 established for the first two. `Document::merge_visible`
computes that flattened subset, then rebuilds the layer stack: the first
visible layer's slot (bottom-to-top) gets the new merged layer, every other
visible layer is dropped, and hidden layers pass through untouched. Errors
with fewer than two visible layers — there is nothing meaningful to merge,
matching Photoshop's own menu item being disabled. The new layer is fully
opaque, Normal blend, and already-baked, so it reproduces the exact same
composite the merged layers did, just as one layer instead of several.

**Verified two ways.** `composite.rs`'s existing recompositing tests all
still pass unchanged, confirming the `composite_layers_pixel` refactor
didn't alter `flatten`'s or `recomposite_region`'s output. `document.rs`
gained tests for the two-visible-layer minimum (including a lone visible
layer plus any number of hidden ones still not qualifying), for merging
combining exactly the visible layers with the exact source-over blend
result `flatten` would have produced, and for the merged layer landing at
the bottommost merged layer's position with a hidden layer in between kept
in place. Live under Xvfb: New… → paint a dot on Layer 1 → add a second
image layer (`rings.png`, via a direct `add_layer` call — no native file
dialog needed since the path is just an argument) → Merge Visible — the
layers panel dropped from two layers to one named "Merged", and the
composite was visually unchanged, confirming the merge reproduced the
same appearance rather than just replacing it with something that looked
close.

**121 Rust tests total** (118 → 121). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Flatten Image.** The same `flatten_subset`/`composite_layers_pixel`
groundwork Merge Visible needed made this the smaller of the two: unlike
Merge Visible, Flatten Image composites *every* layer regardless of
visibility (so it's just `composite::flatten`, not a computed subset) and
discards every layer afterward rather than sparing hidden ones — the whole
stack becomes one new layer named `"Background"`, matching what Photoshop
calls the result of its own Flatten Image. Errors only when the document
has no layers at all to flatten. `Document::flatten_image` is a handful of
lines given `flatten` already existed; the actual design cost was already
paid by Merge Visible's refactor just above.

**Verified two ways.** `document.rs` gained tests for flattening an empty
document being an error, for a hidden layer's pixels being discarded
entirely rather than merely staying invisible (only the visible layer's
colour survives in the flattened result, matching what `flatten()` itself
would produce), and for flattening a single-layer document being a visual
no-op. Live under Xvfb: with the same two-layer document Merge Visible was
verified on, hid the `rings.png` layer first (composite fell back to just
the painted dot, `Merge Visible` correctly disabled at one visible layer)
then clicked **Flatten Image** — one layer left, named "Background", and
the composite still showed only the dot, confirming the hidden layer's
content was discarded rather than silently merged back in.

**124 Rust tests total** (121 → 124). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Merge Down.** The narrowest of the three collapse operations: combines
one specific layer with the one directly below it in the stack, replacing
both with a single new layer at that position — everything else in the
stack is untouched. Unlike `merge_visible`'s `flatten_subset` (which
deliberately ignores each included layer's own `visible` flag, since its
caller already filtered to exactly the visible ones), `merge_down` filters
its two candidate layers through `contributes()` first, so a hidden or
zero-opacity layer among the two contributes nothing — the same rule
`flatten` itself applies, rather than a special case just for this
command. The merged layer takes the name of the layer it merged *into*
(the one below), matching Photoshop's own Merge Down. `merge_down` is a
new `edit_checkpointed` command taking the layer id to merge, alongside a
**Merge Down** button in the layers panel's per-layer controls, disabled
whenever the selected layer is already the bottom of the stack (the same
condition **Move down** already used).

**Verified two ways.** `document.rs` gained tests for merging the bottom
layer being an error (nothing below it), for merging combining exactly two
layers with the same source-over blend result the equivalent
`merge_visible` case produces, and for a hidden layer among the two
contributing nothing to the merge rather than blending in regardless. Live
under Xvfb: the same two-layer test document as Merge Visible and Flatten
Image — selected the top layer (`rings.png`), which enabled **Merge
Down** (disabled while the bottom `Layer 1` was selected, since it has
nothing below it), clicked it, and the layers panel dropped to one layer
named "Layer 1" — the name of the layer merged into — with the composite
visually unchanged.

**127 Rust tests total** (124 → 127). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Eyedropper.** Samples from the same source Photoshop's own eyedropper
defaults to: the merged image, not one specific layer. `sample_pixel_color`
reads straight out of `AppState`'s already-cached composite pixels (the
same raw RGBA8 buffer the `composite://` protocol serves as PNG bytes) —
no re-flatten needed, since every edit already keeps that cache current.
Split into a plain function and a thin `sample_color` command around it,
the same pattern `export`/`export_png` established, so it's directly unit
testable. Errors if nothing has been composited yet (no document open) or
the point falls outside the canvas. The frontend adds an **Eyedropper**
toolbar button; clicking the canvas with it active samples that pixel and
sets it as the brush color (`rgbToHex`, the inverse of the existing
`hexToRgb`), without needing a layer selected — sampling reads the
composite, not a specific layer's own pixels, so `Eyedropper` is enabled
whenever a document is open rather than gated behind `canPaint`.

**Verified two ways.** New `lib.rs` tests cover sampling before anything
is composited being an error, sampling outside the canvas being an error
in both dimensions, and sampling reading back the exact colour at a given
pixel from a two-tone test image. Live under Xvfb: New… → switched to
Eyedropper → clicked an untouched (transparent) part of the canvas — the
color swatch changed from its default white to black, correctly reading
back the `[0, 0, 0, 0]` a transparent pixel decodes to — → switched to
Brush → painted a new dot, which came out black, proving the sampled
color was actually picked up and not just displayed.

**130 Rust tests total** (127 → 130). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Paint Bucket.** `Document::flood_fill` spreads from a seed pixel to its
4-connected neighbours whose colour is within `tolerance` (per channel,
`0..=255`) of the seed's own colour — the default "Contiguous" fill
Photoshop's own Paint Bucket starts from — filling each with `color` via
the same normal `source-over` blend `Stroke::Brush` already uses.
Confined to the active selection and blocked by a locked layer, exactly
like `stroke`; a seed pixel excluded by the selection fills nothing
(`None`, not an error — the same as a stroke entirely outside a
selection). Implemented as an explicit stack-based flood fill with a
document-sized `visited` buffer, rather than recursion, to stay
stack-safe on a large contiguous region. `flood_fill` is a new
`edit_checkpointed` command — a whole discrete action on its own, not one
step of a longer gesture the way a brush stroke is. The frontend adds a
**Paint Bucket** toolbar button; Tolerance is fixed at a reasonable
middle value (32) rather than exposing a second numeric control next to
Flow — a deliberate scope cut, not an oversight, left for a later pass if
it turns out to matter.

**Verified two ways.** New `document.rs` tests cover a fill stopping at a
differently-coloured pixel (proving 4-connectivity, not "any matching
pixel in the document"), tolerance controlling how close a match must be
(the same two-pixel case both excluded at zero tolerance and included at
a wider one), confinement to the active selection even when the matching
region continues beyond it, a seed outside the selection filling nothing,
a locked layer rejecting the fill, and an out-of-bounds seed being an
error. Live under Xvfb: New… → dragged a rectangular selection over part
of the (uniformly transparent, and so — without the selection —
otherwise entirely contiguous) canvas → Paint Bucket → clicked inside
it — only the selected rectangle filled white, the rest of the
otherwise-identical-colour canvas around it untouched, proving the fill
actually stopped at the selection boundary rather than spreading through
the whole contiguous region it would have reached unconfined.

**136 Rust tests total** (130 → 136). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Gradient.** `Document::gradient_fill` blends a linear interpolation
between two colors along a dragged line, over every pixel of a layer — or,
with an active selection, just the pixels it includes. Each pixel's centre
is projected onto the line (`((cx - x0) * dx + (cy - y0) * dy) / len_sq`,
clamped to `0.0..=1.0`) to pick its place in the interpolation, then
composited with the same normal `source-over` blend `Stroke::Brush` uses
— so a pixel past either endpoint clamps to that endpoint's colour rather
than extrapolating. Confined to the active selection and blocked by a
locked layer, exactly like every other paint command; unlike Paint
Bucket's bounded stroke/flood-fill region, a gradient can touch the whole
canvas, so it iterates every pixel rather than a bounding box, skipping
whatever a selection excludes. Errors if the two drag points coincide — a
gradient needs a direction. `gradient_fill` is a new `edit_checkpointed`
command, taking eight flat arguments rather than the two-tuple pairs the
Rust API itself uses (`clippy::too_many_arguments` allowed at that one
IPC boundary, since Tauri commands need JSON-flat parameters). The
frontend adds a **Gradient** toolbar button and a second color picker for
the end color, drag-to-commit like the marquee tools but with no live
preview while dragging — a deliberate scope cut.

**Verified two ways.** New `document.rs` tests cover the interpolation
itself with hand-computed exact byte values (a 2-pixel gradient spanning
the whole canvas, checking both pixel centres' exact projected `t` and
resulting colour), clamping past either endpoint, confinement to an
active selection, rejecting coincident points, a locked layer, and an
unknown layer id. Live under Xvfb: New… → Gradient (white to black) →
dragged corner to corner — a smooth, correctly-ordered white-to-black
gradient filled the whole canvas. Undid it, drew a rectangular selection,
repeated the same drag — only the selected rectangle showed the gradient
(a lighter-to-darker grey slice of the same white-to-black line, matching
where that rectangle falls along it), the canvas around it left
untouched, confirming confinement without breaking the per-pixel
projection math.

**142 Rust tests total** (136 → 142). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Single Row Marquee / Single Column Marquee.** Photoshop's two
one-pixel-thick marquee variants — a full-width, 1px-tall row and a
full-height, 1px-wide column, each placed at the click point rather than
dragged out. Both reuse the already-existing `select_rectangle` command
outright: `selectLineAt` computes a `Rect` that spans the whole canvas in
one axis and pins a single pixel in the other (`{x0: 0, y0: floor(y), x1:
width, y1: floor(y) + 1}` for a row; the transposed shape for a column) and
calls `select_rectangle` with it, so there is no new Rust code at all —
the selection model, `contains()`, stroke/fill confinement, and Reselect
all already work correctly for a 1px-tall or 1px-wide rectangle without
any change. The frontend adds `isLineSelect` (`tool === "selectRow" ||
tool === "selectColumn"`) as a third `handlePointerDown` mode alongside
the existing eyedropper/paint-bucket single-click tools — it fires a
single `select_rectangle` call on pointerdown and returns, with no
drag/pointerup handling needed since the position is exactly the click
point. **Single Row** and **Single Column** toolbar buttons sit next to
Rect/Ellipse Select in the existing Selection tool group, and the
canvas's cursor-gating (`hasDocument`, not `canPaint` — like the other
selection tools, no layer needs to be selected to draw a selection) and
per-tool cursor CSS (`row-resize` / `col-resize`, distinguishing them at a
glance from the marquee tools' `crosshair`) follow the same pattern as
every other tool.

**Verified two ways.** No new Rust surface exists to unit-test — this
increment is a pure frontend composition of an already-thoroughly-tested
command, so its correctness rests entirely on `select_rectangle`'s
existing coverage plus live behavior. Live under Xvfb: New… (800×600) →
**Single Row** → clicked mid-canvas — a hairline marching-ants outline
appeared spanning the full canvas width at exactly the clicked row, with
**Reselect** newly enabled. Switched to **Single Column** → clicked
elsewhere on the canvas — the previous row selection was replaced by a
hairline outline spanning the full canvas height at exactly the clicked
column. Both screenshotted and visually confirmed pixel-precise against
the click position.

**142 Rust tests total** (unchanged — frontend-only increment). `cargo fmt`,
`clippy`, `cargo test`, and `npm run build` all clean.

**Expand Selection / Contract Selection.** Select > Modify > Expand and
Contract grow or shrink the selected region by a pixel amount on every
side. Both share one new private helper, `resize_selection_bounds(delta)`
on `Document`, that grows the shape's bounding box by `delta` pixels per
side (negative shrinks it) and clamps each edge to the canvas —
`expand_selection(amount)` and `contract_selection(amount)` are just
`resize_selection_bounds(amount as i64)` and `resize_selection_bounds
(-(amount as i64))`. Errors if nothing is selected, if `amount` is zero
(Photoshop's own dialog requires a positive pixel count), or if
contracting that far would collapse the selection to zero width or
height — leaving the selection untouched rather than silently clearing
it. The existing `Selection { shape, bounds, inverted }` representation
(no mask) turned out to be exactly the right fit for this one: neither
command needed any new state.

The one subtlety worth calling out: for an *inverted* selection —
everywhere except the shape — growing the *selected* area means shrinking
the excluded shape, the opposite of what growing a normal selection's
bounds does. `resize_selection_bounds` flips `delta`'s sign against the
shape's own bounds whenever `selection.inverted` is set, so Expand and
Contract read correctly to the user regardless of whether Select > Inverse
was used beforehand, without needing a mask to express "grow everywhere
except a shrinking hole."

The frontend adds **Expand…** and **Contract…** buttons next to Invert in
the Selection tool group, both disabled without an active selection. They
share one small modal (the same `modal-overlay`/`modal` pattern the New
Document dialog established) with a single pixel-amount number input,
defaulting to 4; clicking Apply sends `expand_selection` or
`contract_selection` with that amount and closes the dialog.

**Verified two ways.** New `document.rs` tests cover both commands
erroring with nothing selected, both erroring at a zero amount, expand
growing bounds on every side, expand clamping at the canvas edge,
contract shrinking bounds on every side, contracting past the selection's
own size erroring and leaving the original bounds untouched, and — the
inverted case specifically — expanding an inverted selection shrinking
the excluded shape's bounds and contracting one growing it, each checked
against hand-computed exact bounds. Live under Xvfb: New… (800×600) →
Rect Select → dragged out a selection → **Expand…** → Apply at the
default 4px — the marching-ants outline grew outward by a few pixels on
every side, screenshotted before and after for a direct visual diff.
Drew a fresh, larger rectangle → **Contract…** → set 30px → Apply — the
outline shrank to a visibly smaller box centred on the same spot,
confirming the shrink was symmetric rather than anchored to one corner.

**152 Rust tests total** (142 → 152). `cargo fmt`, `clippy`, and
`npm run build` all clean.

## Phase 11 — Invert / Threshold / Posterize / Brightness-Contrast / Hue-Saturation / Black & White / Vibrance / Photo Filter / Exposure / Gradient Map / Channel Mixer / Levels / Curves / Color Balance (adjustments)

The first entry from PART V of the parity checklist — Image >
Adjustments > Invert flips every RGB channel of a layer's pixels
(`255 - channel`), leaving alpha untouched. `Document::invert_colors`
follows the same per-pixel-iteration shape Gradient established: no
bounded region to flood-fill or stroke outward from, so it walks every
pixel the canvas has, skipping whatever the active selection excludes,
and is blocked outright by a locked layer — the same two guards every
other in-place pixel edit already respects. Unlike Gradient's blend math,
Invert needed no alpha compositing at all: it operates directly on the
layer's own stored channel bytes, which is also why it still flips a
fully transparent pixel's RGB (mathematically correct, if invisible until
that pixel's alpha changes) rather than special-casing alpha out of the
loop.

`invert_colors` is a new `edit_checkpointed` command, taking just the
layer id — no color, no drag, nothing else to configure, matching
Photoshop's own menu item having no dialog. The frontend adds an
**Invert Colors** toolbar button next to Gradient, enabled whenever a
layer is selected (`canPaint`, the same gate Brush/Eraser use) rather
than needing an active tool at all — clicking it fires once and is done.

**Verified two ways.** New `document.rs` tests cover the core channel
flip against hand-picked byte values (including a partially-transparent
pixel, to confirm alpha itself is untouched), inverting twice restoring
the original colours exactly, confinement to an active selection (pixels
outside it provably untouched), a fully-transparent pixel's RGB flipping
even though nothing is visibly different yet, a locked layer rejecting
the call, and an unknown layer id erroring. Live under Xvfb: New…
(800×600) → painted a white L-shaped stroke → **Invert Colors** — the
stroke turned solid black, screenshotted before and after — → **Undo**
— the white stroke came back exactly, confirming the command checkpoints
itself correctly like every other one-shot action in this project.

**158 Rust tests total** (152 → 158). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Threshold.** Image > Adjustments > Threshold converts a layer to pure
black or white per pixel, based on standard ITU-R BT.601 luma (`0.299R +
0.587G + 0.114B`, the same weights Photoshop's own Threshold uses)
against a `level` (`1..=255`): at or above it, a pixel becomes white;
below it, black. Alpha untouched, same as Invert.

Landing right after Invert made the shared shape between the two obvious
enough to pull out: both are whole-canvas, per-pixel, selection-confined,
lock-respecting transforms that differ only in what they do to each
pixel's four bytes. `Document::invert_colors` and the new
`Document::threshold` now both delegate to a new private
`adjust_layer_pixels(id, f)` helper that owns the iteration, the
selection/lock guards, and the touched-region bookkeeping once, taking a
closure that maps one `[u8; 4]` pixel to its replacement — `invert_colors`
is now a one-line closure, and `threshold` just adds the luma computation
on top. Any future single-pixel adjustment (Posterize, Brightness/
Contrast, Hue/Saturation, …) can reuse the same helper rather than
re-deriving this loop a third time.

`threshold` is a new `edit_checkpointed` command taking the layer id and
`level`. The frontend adds a **Threshold…** toolbar button next to Invert
Colors, opening a small modal (same `modal-overlay`/`modal` pattern as
Expand/Contract) with a single `level` slider (`1..=255`, defaulting to
128) and a live numeric readout, styled like the brush Size/Flow sliders.

**Verified two ways.** New `document.rs` tests cover the core
above/below-level split against hand-picked luma values, confirming the
BT.601 weights are actually applied (pure green crosses a threshold pure
red doesn't, despite both being a single channel maxed out — a flat
per-channel average would get this wrong), alpha staying untouched,
confinement to an active selection, a zero level being rejected (matching
Photoshop's own 1–255 range), a locked layer, and an unknown layer id.
Live under Xvfb: added a colourful sample-image layer (a diagonal
hue/lightness gradient) via a temporary probe button (removed before
committing, `grep -n "TEMP\|PROBE"` returning nothing) → **Threshold…**
→ Apply at the default level 128 — the gradient split cleanly into a
crisp black/white diagonal boundary following the image's own luma
contour, exactly where the darker and lighter halves of the gradient
met, screenshotted before and after.

**165 Rust tests total** (158 → 165). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Posterize.** Image > Adjustments > Posterize quantizes each RGB channel
independently down to a given number of evenly spaced tones (Photoshop's
own dialog defaults to 4), leaving alpha untouched — the third and, for
now, last entry in this batch of `adjust_layer_pixels`-based adjustments.
Each channel value snaps to the nearest of `levels` steps spanning
`0..=255`: `step = 255 / (levels - 1)`, `output = round(round(value /
step) * step)`. `levels` must be at least 2 (one level would collapse
every channel to a single flat value, which isn't a meaningful posterize,
and isn't what Photoshop's own dialog — minimum 2 — allows either).

`Document::posterize` is the third caller of the `adjust_layer_pixels`
helper Threshold introduced, and needed nothing new from it — just its
own per-channel quantization closure, the same shape `invert_colors` and
`threshold` already established. `posterize` is a new `edit_checkpointed`
command taking the layer id and `levels`. The frontend adds a
**Posterize…** toolbar button next to Threshold…, opening the same kind
of small modal with a single `levels` slider — capped at 64 in the UI
(Photoshop's own dialog technically allows up to 255, but the visually
useful range for a genuine posterize effect is a small handful of levels;
the backend command itself still accepts the full `2..=255` range,
matching every other place in this project where the UI narrows a control
without narrowing the underlying API — Paint Bucket's fixed tolerance is
the same pattern).

**Verified two ways.** New `document.rs` tests cover the core
quantization against hand-computed exact byte values (including a
partially-transparent pixel, confirming alpha stays untouched), 2-level
posterize collapsing a channel to pure black or white, a 1-level request
being rejected (matching Photoshop's own 2-level minimum), confinement to
an active selection, a locked layer, and an unknown layer id. Live under
Xvfb: added the same colourful sample-image gradient layer used to verify
Threshold, via the same temporary probe button (removed before
committing, `grep -n "TEMP\|PROBE"` returning nothing) → **Posterize…** →
Apply at the default 4 levels — the smooth diagonal gradient split into
crisp, flat-colored bands, each grid cell landing in one of a handful of
distinct colours instead of its own smooth shade, screenshotted before
and after.

**171 Rust tests total** (165 → 171). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Brightness/Contrast.** Image > Adjustments > Brightness/Contrast applies
a flat per-channel offset (`brightness`) plus a scale around the mid-grey
point 128 (`contrast`) — the same widely-used "legacy" formula many
editors implement: `factor = 259*(contrast+255) / (255*(259-contrast))`,
`output = factor*(value-128) + 128 + brightness`, clamped to `0..=255`.
Alpha untouched, same as every other adjustment in this batch. Both
sliders are clamped to `-255..=255` rather than erroring on an
out-of-range value — there's no invalid input here, just one that
saturates, the same way a bounded numeric field would.

`Document::brightness_contrast` is the fourth caller of the
`adjust_layer_pixels` helper, needing nothing new from it either — just a
closure computing the scaled-and-shifted value per channel. New
`edit_checkpointed` command taking the layer id, `brightness`, and
`contrast` (both `i32`, since Rust has no 9-bit integer to hold
`-255..=255` exactly). The frontend adds a **Brightness/Contrast…**
toolbar button next to Posterize…, opening a modal with two sliders
(`-150..=150`, Photoshop's own dialog range) — the UI narrows the range
the same way Posterize's slider does, while the backend command itself
still accepts the full `-255..=255`.

**Verified two ways.** New `document.rs` tests cover the zero/zero no-op
case, a positive brightness shifting every channel and clamping at 255,
a contrast of exactly -255 collapsing every channel to mid-grey 128 (the
scale factor is exactly zero at that extreme, a clean deterministic
case), that same collapse shifted by a brightness offset, a contrast of
exactly +255 pushing values on either side of 128 to pure black or white
while 128 itself stays put, an out-of-range brightness saturating rather
than erroring, confinement to an active selection, a locked layer, and
an unknown layer id. Live under Xvfb: loaded the same colourful
sample-image gradient layer via a temporary probe button (removed before
committing, `grep -n "TEMP\|PROBE"` returning nothing) →
**Brightness/Contrast…** → raised Contrast to 40, applied — the gradient
became visibly more saturated with sharper colour separation between
grid cells, exactly the expected effect of pushing values away from
mid-grey, screenshotted before and after.

**180 Rust tests total** (171 → 180). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Hue/Saturation.** Image > Adjustments > Hue/Saturation shifts hue by a
number of degrees, scales saturation by a percentage, and offsets
lightness by a percentage — the richest of this batch, since it needs a
colour-space round trip rather than a flat per-channel formula. Each
pixel converts RGB -> HSL, the three sliders adjust hue/saturation/
lightness in that space, then it converts back HSL -> RGB; alpha stays
untouched throughout. Two new free functions, `rgb_to_hsl` and
`hsl_to_rgb`, implement the standard conversions (`rgb_to_hsl` treats a
pixel with no chroma — `max == min` — as hue 0°, saturation 0°, rather
than an arbitrary hue, since an achromatic pixel genuinely has none; this
is also why a grey pixel is provably unaffected by any hue shift).
`Document::hue_saturation` is the fifth caller of `adjust_layer_pixels`,
wrapping the conversion pair in a closure the same way every other
adjustment in this phase has. `hue` clamps to `-180..=180` and
`saturation`/`lightness` to `-100..=100` (Photoshop's own dialog ranges)
before use, the same saturating convention `brightness_contrast`
established rather than erroring on an out-of-range value.

New `edit_checkpointed` command taking the layer id, `hue`, `saturation`,
and `lightness` (all `i32`). The frontend adds a **Hue/Saturation…**
toolbar button opening a modal with three sliders, matching Photoshop's
own dialog's three controls and ranges exactly (no UI-narrowing needed
here, unlike Posterize or Brightness/Contrast, since the backend's own
clamped range already matches Photoshop's).

**Verified two ways.** New `document.rs` tests cover a +120° hue shift
turning pure red into pure green (a clean, hand-verifiable rotation
around the colour wheel), a ±180° shift landing on the same result either
direction (confirming the wraparound), -100% saturation collapsing pure
red to its own mid-grey lightness, +100%/-100% lightness turning any
colour white or black outright, a neutral grey pixel staying exactly
unchanged under a 90° hue shift (exercising `rgb_to_hsl`'s zero-chroma
branch), alpha staying untouched, an out-of-range slider saturating,
confinement to an active selection, a locked layer, and an unknown layer
id. Live under Xvfb: loaded the same colourful sample-image gradient
layer via a temporary probe button (removed before committing,
`grep -n "TEMP\|PROBE"` returning nothing) → **Hue/Saturation…** → raised
Hue to 60°, applied — the entire blue/purple/pink palette rotated to
purple/red/orange/green, exactly the expected result of a 60° rotation
around the colour wheel, screenshotted before and after.

**191 Rust tests total** (180 → 191). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Black & White.** Image > Adjustments > Black & White desaturates a
layer to greyscale, setting all three RGB channels to the same ITU-R
BT.601 luma `threshold` already computes (`0.299R + 0.587G + 0.114B`) —
the difference from Threshold being that the luma is kept as a continuous
tone rather than snapped to pure black or white. Alpha untouched.
Photoshop's own Black & White dialog offers six colour-range sliders
(reds, yellows, greens, cyans, blues, magentas) for a fully custom
weighting; this uses one fixed, standard weighting instead — a deliberate
scope cut in the same spirit as Paint Bucket's fixed tolerance and
Posterize's UI-capped slider, not an oversight.

`Document::black_and_white` is the sixth caller of `adjust_layer_pixels`
and the simplest of the batch: no sliders, no clamping, just the luma
computation reused via the shared `to_unit`/`to_byte` helpers
[`composite.rs`] already provides. New `edit_checkpointed` command taking
only the layer id. The frontend adds a **Black & White** toolbar button —
a one-shot action with no dialog, the same pattern **Invert Colors**
already established, since neither needs any parameter beyond which
layer to act on.

**Verified two ways.** New `document.rs` tests cover the luma computation
against the same hand-verified byte values Threshold's own weighting
tests used (white stays 255, pure red becomes 76, pure green becomes
150), alpha staying untouched, confinement to an active selection, a
locked layer, and an unknown layer id. Live under Xvfb: loaded the same
colourful sample-image gradient layer via a temporary probe button
(removed before committing, `grep -n "TEMP\|PROBE"` returning nothing) →
**Black & White** — the multicoloured grid converted cleanly to a smooth
greyscale gradient, darker toward the original blue corner and lighter
toward the original yellow/pink corner, matching the relative luma of
each original colour, screenshotted before and after.

**196 Rust tests total** (191 → 196). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Vibrance.** Image > Adjustments > Vibrance behaves like
Hue/Saturation's own saturation slider, but weighted to protect
already-saturated pixels — and, not incidentally, skin tones, usually
the least saturated colours in a photo — from clipping to a garish
maximum. `vibrance` scales saturation by `1 - current_saturation`
(computed in the same RGB -> HSL space `hue_saturation` established): a
pixel that's already fully saturated gets no boost (or cut) at all,
while a near-grey pixel gets the full effect either direction; a
`saturation` slider then applies uniformly on top, the same linear scale
`hue_saturation`'s own saturation control uses. Both `-100..=100`,
clamped rather than erroring on an out-of-range value.

`Document::vibrance` reuses `rgb_to_hsl`/`hsl_to_rgb` directly rather
than going through `adjust_layer_pixels` and a shared HSL step —
it's the second HSL-based adjustment, after `hue_saturation` itself, and
needed no new colour-space machinery at all. New `edit_checkpointed`
command taking the layer id, `vibrance`, and `saturation`. The frontend
adds a **Vibrance…** toolbar button opening a modal with two sliders,
matching Photoshop's own dialog's two controls and ranges.

**Verified two ways.** New `document.rs` tests cover a fully saturated
pixel staying exactly unchanged under +100 vibrance (nothing left to
boost), a lightly saturated pixel (hand-picked at saturation 0.2) boosted
all the way to full saturation under the same +100, and — the clearest
demonstration of the "protection" vibrance is for — the same -100
vibrance leaving a fully saturated pixel completely untouched while
driving the lightly saturated one all the way to grey, side by side in
one test. Further tests cover the uniform `saturation` slider behaving
identically to `hue_saturation`'s own, alpha staying untouched, an
out-of-range slider saturating, confinement to an active selection, a
locked layer, and an unknown layer id. Live under Xvfb: loaded the same
colourful sample-image gradient layer via a temporary probe button
(removed before committing, `grep -n "TEMP\|PROBE"` returning nothing) →
**Vibrance…** → lowered Saturation to -70, applied — the vivid palette
desaturated toward soft pastel tones across the whole gradient,
screenshotted before and after.

**205 Rust tests total** (196 → 205). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Photo Filter.** Image > Adjustments > Photo Filter tints a layer toward
a chosen colour by blending each pixel's RGB toward it by a `density`
percentage (`0..=100`, clamped above 100 rather than erroring — Photoshop's
own slider tops out there too). Alpha untouched. Photoshop's own dialog
also offers a "Preserve Luminosity" checkbox that renormalizes brightness
after tinting; this omits it — a deliberate scope cut, the same kind
Black & White's single fixed luma weighting already made in this project.

`Document::photo_filter` reuses the exact same `lerp`/`to_unit`/`to_byte`
helpers `gradient_fill` already established for its own colour blending,
composed through `adjust_layer_pixels` — the seventh caller of that
helper, and the first to take a colour parameter of its own rather than
just numeric sliders. New `edit_checkpointed` command taking the layer
id, `color` (`[u8; 3]`), and `density`. The frontend adds a **Photo
Filter…** toolbar button opening a modal with a colour picker (defaulting
to a warm orange, echoing Photoshop's own default Warming Filter) and a
density slider defaulting to 25%, matching Photoshop's own default.

**Verified two ways.** New `document.rs` tests cover full density fully
replacing a pixel's colour, zero density leaving it completely unchanged,
half density landing at the exact hand-computed midpoint between the two
colours, alpha staying untouched, density saturating above 100 rather
than erroring, confinement to an active selection, a locked layer, and
an unknown layer id. Live under Xvfb: loaded the same colourful
sample-image gradient layer via a temporary probe button (removed before
committing, `grep -n "TEMP\|PROBE"` returning nothing) → **Photo
Filter…** → raised Density to 80%, applied — the whole multicoloured
gradient tinted toward the orange filter colour while still showing
subtle underlying luminosity variation across the grid, exactly the
expected partial-density blend rather than a flat colour fill,
screenshotted before and after.

**213 Rust tests total** (205 → 213). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Exposure.** Image > Adjustments > Exposure applies the same
three-control model Photoshop's own dialog uses, per channel, to a
`0.0..=1.0` working value: `exposure` (a stop count — `2^exposure`
multiplies the value, the same doubling-per-stop a camera sensor uses),
`offset` (added after exposure, shifts black), and `gamma`
(`value.powf(1.0 / gamma)`, curving the midtones). Each control clamps
rather than errors on an out-of-range value: `exposure` to `-2000..=2000`
(hundredths of a stop, `±20.00`, Photoshop's own range), `offset` to
`-50..=50` (hundredths, `±0.50`), `gamma` to `1..=999` (hundredths,
`0.01..=9.99` — never zero, which would make `1.0 / gamma` divide by
zero). The value is floored at zero before the gamma power (a negative
base raised to a fractional exponent is undefined in `f32::powf`) and
clamped to `0.0..=1.0` only at the very end, so a highlight exposure
pushes past white exactly the way it would on a real sensor before
finally clipping — the clamp is a display limit, not a computation limit.
Alpha untouched.

`Document::exposure` is the eighth caller of `adjust_layer_pixels`. New
`edit_checkpointed` command taking the layer id, `exposure`, `offset`,
and `gamma` (all `i32`, the same hundredths-scaled-integer convention
`hue_saturation` and `vibrance` already use for fractional ranges without
needing a float across the Tauri IPC boundary). The frontend adds an
**Exposure…** toolbar button opening a modal with three sliders — the UI
narrows `exposure` to `±2.00` stops and `gamma` to `0.10..=3.00` (the
practically useful ranges) while `offset` matches the backend's own
`±0.50` exactly, each display value formatted to two decimals rather
than showing the raw hundredths integer.

**Verified two ways.** New `document.rs` tests cover the all-default
case being an exact no-op, a positive offset lifting pure black toward
mid-grey, one stop of exposure exactly doubling a midtone and clamping a
highlight past white, exposure being purely multiplicative — it cannot
lift a true-black pixel no matter how many stops are dialed in, unlike
offset — a gamma of 2.0 applying a hand-verified square-root curve, alpha
staying untouched, an out-of-range slider saturating, confinement to an
active selection, a locked layer, and an unknown layer id. Live under
Xvfb: loaded the same colourful sample-image gradient layer via a
temporary probe button (removed before committing, `grep -n
"TEMP\|PROBE"` returning nothing — this increment's probe needed a wider
Xvfb virtual screen than prior ones, since the accumulated toolbar
buttons across nine adjustments no longer fit even a 1550px-wide window;
restarting Xvfb at 2400×1100 resolved it) → **Exposure…** → raised Offset
to +0.40, applied — the whole gradient lifted dramatically toward white,
with the darkest corner brightening the most visibly and the lightest
corner clipping to pure white, exactly the expected offset-lift curve,
screenshotted before and after.

**223 Rust tests total** (213 → 223). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Gradient Map.** Image > Adjustments > Gradient Map replaces each
pixel's colour with a point along the line from a shadow colour to a
highlight colour, picked by that pixel's own ITU-R BT.601 luma — the
same weighting `threshold` and `black_and_white` already use — so a
shadow-luma pixel lands exactly on the shadow colour, a highlight-luma
pixel exactly on the highlight colour, and everything between blends
smoothly. Photoshop's own dialog accepts an arbitrary multi-stop
gradient preset; this always maps to a straight two-colour line, the
same two-stop scope `gradient_fill` already uses for its own gradients —
a deliberate scope cut, not an oversight. Alpha untouched.

`Document::gradient_map` is the ninth caller of `adjust_layer_pixels`,
composing three pieces this project already had lying around: the luma
computation `threshold`/`black_and_white` established, and the `lerp`/
`to_unit`/`to_byte` blend helpers `gradient_fill`/`photo_filter` already
use — landing this late in the batch made it almost entirely a
composition of existing math rather than new math. New `edit_checkpointed`
command taking the layer id, `shadow_color`, and `highlight_color` (both
`[u8; 3]`). The frontend adds a **Gradient Map…** toolbar button opening
a modal with two colour pickers, defaulting to black and white — the
same default Photoshop's own dialog opens with, and a Black & White-style
result until the swatches are changed.

**Verified two ways.** New `document.rs` tests cover a black pixel
mapping exactly to the shadow colour and a white pixel exactly to the
highlight colour (the two boundary cases), the luma weighting itself
against the same hand-verified 76/150 values Threshold's and Black &
White's own tests use (proving it's genuinely luma-driven, not a flat
per-channel average), alpha staying untouched, confinement to an active
selection, a locked layer, and an unknown layer id. Live under Xvfb:
loaded the same colourful sample-image gradient layer via a temporary
probe button (removed before committing, `grep -n "TEMP\|PROBE"`
returning nothing) → **Gradient Map…** → applied at the default
black-to-white swatches — the multicoloured grid mapped cleanly to a
black-to-white gradient matching each cell's original luma, visually
identical to what Black & White alone would have produced with those two
colours, confirming the two-colour line degenerates correctly to a plain
greyscale map at its default extremes, screenshotted before and after.

**230 Rust tests total** (223 → 230). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Channel Mixer.** Image > Adjustments > Channel Mixer builds each output
channel as a weighted sum of all three input channels plus a constant —
`output_c = r*matrix[c][0] + g*matrix[c][1] + b*matrix[c][2] +
matrix[c][3]`, one row of the matrix per output channel, clamped to
`0..=255`. The three per-channel coefficients are percentages
(`-200..=200`, i.e. `-2.00..=2.00`, Photoshop's own range) and the
constant is a direct `-200..=200` byte-scale offset — both clamped rather
than erroring on an out-of-range value. The identity matrix
(`[[100,0,0,0], [0,100,0,0], [0,0,100,0]]`) is a no-op; moving a row's
own 100-weight onto a different input channel swaps channels outright,
and a negative weight inverts a channel's contribution — this one
command subsumes the plain channel-swap and channel-invert tricks
Photoshop users often reach for Channel Mixer to do, without needing
separate commands for them.

`Document::channel_mixer` is the tenth caller of `adjust_layer_pixels`
and the first to take a full matrix rather than a handful of scalar
sliders. New `edit_checkpointed` command taking the layer id and the
`3×4` matrix (`[[i32; 4]; 3]`, IPC-flat as nested fixed-size arrays). The
frontend adds a **Channel Mixer…** toolbar button opening a modal with a
compact `R`/`G`/`B`-by-`R`/`G`/`B`/`Constant` grid of twelve number
inputs (plain numbers rather than sliders, since a 3×4 grid of sliders
wouldn't fit any reasonably sized dialog) and a **Reset** button that
restores the identity matrix — the same shared `.modal`/`.modal__actions`
structure every other dialog in this phase uses, widened for this one
via an inline style since the grid needs more than the usual 280px.

**Verified two ways.** New `document.rs` tests cover the identity matrix
being an exact no-op, a hand-picked matrix building each output channel
as the documented weighted sum (including a fractional 50% coefficient
landing on an exact clean value), an all-zero-coefficient matrix with
just a constant producing a flat colour regardless of input, a negative
coefficient inverting a channel's contribution (checked at both ends of
the input range), an out-of-range coefficient saturating at the slider
clamp and the resulting output still clamping to a valid byte, alpha
staying untouched, confinement to an active selection, a locked layer,
and an unknown layer id. Live under Xvfb: loaded the same colourful
sample-image gradient layer via a temporary probe button (removed before
committing, `grep -n "TEMP\|PROBE"` returning nothing) → **Channel
Mixer…** → set the R row to `[0, 100, 0, 0]` and the G row to `[100, 0,
0, 0]` (a full R↔G channel swap) → applied — the palette visibly shifted
from blue/purple/pink to blue/teal/green/magenta, exactly the expected
result of swapping which input channel feeds which output, screenshotted
before and after.

**239 Rust tests total** (230 → 239). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Levels.** Image > Adjustments > Levels is the classic histogram remap:
each RGB channel value is normalized against an input black/white range
(`(value - input_black) / (input_white - input_black)`, clamped to
`0.0..=1.0`), gamma-corrected (`normalized.powf(1.0 / gamma)`), and then
remapped onto an output black/white range
(`output_black + corrected * (output_white - output_black)`).
`input_black`/`input_white`/`output_black`/`output_white` are all
`0..=255` bytes; `gamma` is hundredths (`1..=999`, i.e. `0.01..=9.99`,
Photoshop's own dialog range). At the defaults (`0`/`255` input,
`1.00` gamma, `0`/`255` output) every step collapses to a no-op. Like
Black & White's single fixed luma weighting, this always applies to the
RGB composite channel rather than exposing Photoshop's own per-channel
dropdown (Red/Green/Blue individually) — a deliberate scope cut, not an
oversight. `input_white` is clamped to at least one greater than
`input_black` rather than erroring or dividing by zero when a caller
pushes the sliders to a zero-width input range. Alpha is untouched.

`Document::levels` is the eleventh caller of `adjust_layer_pixels`. New
`edit_checkpointed` command taking the layer id plus the five `u8`/`i32`
parameters. The frontend adds a **Levels…** toolbar button opening a
modal with five range sliders (Input Black, Input White, Gamma — shown
as a `0.01`-precision multiplier like `1.00` rather than the raw
hundredths integer — Output Black, Output White), reusing the same
`.control` slider rows every other adjustment dialog in this phase
already uses.

**Verified two ways.** New `document.rs` tests cover the defaults being
an exact no-op, narrowing the input range remapping a mid-value onto the
correct point of the output range, a gamma of `2.00` applying the same
square-root curve already exercised by Exposure's gamma test (input
`64` → output `128`, cross-checked against that earlier test's identical
result), narrowing the output range, `input_white` being clamped above
`input_black` rather than dividing by zero, alpha staying untouched,
confinement to an active selection, a locked layer, and an unknown layer
id. Live under Xvfb: created an 800×600 document, loaded the same
colourful sample-image gradient layer via a temporary probe button
(removed before committing, `grep -n "TEMP\|PROBE"` returning nothing)
→ **Levels…** → dragged Input White down from `255` to `156`, leaving
gamma and the output range at their defaults → applied — the image's
highlights visibly blew out to solid white across a much larger portion
of the gradient and the overall image read noticeably brighter and more
saturated, exactly the expected effect of narrowing the input white
point, screenshotted before and after.

**248 Rust tests total** (239 → 248). `cargo fmt`, `clippy`, and
`npm run build` all clean.

**Curves.** Image > Adjustments > Curves applies a tone curve identically
to all three RGB channels — the same RGB-composite-only scope cut Levels
already makes, rather than Photoshop's own per-channel Red/Green/Blue
dropdown. Photoshop's own Curves dialog is an interactive editor with an
arbitrary number of freely draggable points connected by a smooth spline;
here the curve is fixed to five control points at evenly spaced input
positions (`0`, `64`, `128`, `192`, `255`) whose five output values are
each independently adjustable via a slider, and adjacent points are
connected by straight line segments rather than a spline — a second
deliberate scope cut, invisible for modest adjustments and only really
apparent on extreme ones, in exchange for a curve that's driven entirely
by ordinary sliders (matching every other adjustment dialog in this
phase) and trivially unit-testable, rather than needing a canvas-based
drag-and-drop point editor. At the identity mapping (output equal to
input at all five positions) every value reproduces exactly, because each
segment's output span exactly matches its input span. Alpha untouched.

`Document::curves` is the twelfth caller of `adjust_layer_pixels`. New
`edit_checkpointed` command taking the layer id plus a `[u8; 5]` of
output values (IPC-flat as a fixed-size array, the same approach Channel
Mixer's `3×4` matrix already established). The frontend adds a
**Curves…** toolbar button opening a modal with five range sliders
labelled by their fixed input position ("Input 0", "Input 64", … "Input
255"), each showing and controlling that point's output value, plus a
**Reset** button restoring the identity curve — the same
`.modal`/`.modal__actions` structure and Reset-button convention Channel
Mixer already established.

**Verified two ways.** New `document.rs` tests cover the identity curve
being an exact no-op, a control point's output value reproducing exactly
at its own input position, linear interpolation between two control
points landing on the exact expected halfway value, flattening a whole
input range to a constant output by setting three consecutive points to
the same value, alpha staying untouched, confinement to an active
selection, a locked layer, and an unknown layer id. Live under Xvfb:
created an 800×600 document, loaded the same colourful sample-image
gradient layer via a temporary probe button (removed before committing,
`grep -n "TEMP\|PROBE"` returning nothing) → **Curves…** → dragged the
"Input 128" point down from `128` to `35`, leaving the other four points
at their identity defaults → applied — the midtone band of the gradient
(the rows straddling the original mid-grey) visibly darkened into deep
blue/purple while the shadow and highlight rows at the top and bottom
stayed close to their original brightness, exactly the expected effect
of crushing only the middle of the tone curve while leaving its ends
anchored, screenshotted before and after.

**256 Rust tests total** (248 → 256, 249 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

**Color Balance.** Image > Adjustments > Color Balance shifts each RGB
channel by an amount that depends on how shadow-like, midtone-like, or
highlight-like a pixel's luminance is — the classic three-range tonal
adjustment, applied via nine sliders (Shadows/Midtones/Highlights ×
Cyan↔Red/Magenta↔Green/Yellow↔Blue). Photoshop's own version blends its
three ranges with a proprietary lookup curve and offers a "Preserve
Luminosity" option that re-normalizes lightness after the shift; both
are deliberate scope cuts here (consistent with Photo Filter already
omitting Preserve Luminosity), in favour of a simple, fully documented,
and exactly testable blending scheme: BT.601 luma (the same weighting
Threshold and Black & White already use, here left on its natural
`0.0..=255.0` byte scale) is split into shadow/midtone/highlight weights
with two linear ramps that never overlap and always sum to exactly
`1.0` — `shadow_weight = clamp((127 - luma) / 127, 0, 1)` (`1.0` at luma
`0`, `0.0` from luma `127` up), `highlight_weight = clamp((luma - 128) /
127, 0, 1)` (`0.0` up to luma `128`, `1.0` at luma `255`), and
`midtone_weight = 1.0 - shadow_weight - highlight_weight` (exactly `1.0`
at luma `127` and `128`, tapering to `0.0` at both ends). The 127/128
split means a pixel at exactly luma `127` or `128` is 100% midtone —
useful for hand-computing exact expected test values, and the reason
this scheme was chosen over the more natural-looking but fraction-prone
`luma / 255.0` normalization Levels and Curves use elsewhere. Each
range's three per-channel sliders (`-100..=100`, Photoshop's own range)
are blended by a pixel's three weights and added directly to the
channel byte, then clamped. No Preserve Luminosity. Alpha untouched.

`Document::color_balance` is the thirteenth caller of
`adjust_layer_pixels`. New `edit_checkpointed` command taking the layer
id plus three `[i32; 3]` arrays (shadows, midtones, highlights), each
`[cyan↔red, magenta↔green, yellow↔blue]` mapping directly onto
`[R, G, B]`. The frontend adds a **Color Balance…** toolbar button
opening a modal with a 3×3 grid of number inputs (one row per tonal
range, one column per channel pair) reusing the `.channel-mixer` table
styling Channel Mixer already established, plus a **Reset** button
zeroing all nine values.

**Verified two ways.** New `document.rs` tests cover the all-zero
defaults being an exact no-op, a pure-shadow pixel (luma `0`) receiving
only the shadow sliders' shift, a pure-midtone pixel (luma `127`,
sitting exactly on both linear ramps' flat zero region) receiving only
the midtone sliders' shift, a pure-highlight pixel (luma `255`)
receiving only the highlight sliders' shift (incidentally also
exercising clamping at the `255` ceiling), the sliders being clamped to
`-100..=100`, alpha staying untouched, confinement to an active
selection, a locked layer, and an unknown layer id. Live under Xvfb:
created an 800×600 document, loaded the bundled colourful sample-image
gradient layer via a temporary probe button (removed before committing,
`grep -n "TEMP\|PROBE"` returning nothing), opened Color Balance, set
Shadows Cyan↔Red to `100` and Highlights Magenta↔Green to `100`, and
applied — the canvas visibly changed (the dark shadow corner picked up
a warmer, more violet cast and the pale highlight region picked up a
visible tint), confirming the command reaches the canvas end to end.
Because the on-screen canvas is a downscaled, interpolated render of
the document and a screenshot-pixel spot check on it turned out to be
unreliable for pinning exact per-channel signs, the precise
shadow-reddens / highlight-greens behaviour was additionally verified
directly against `Document::color_balance` applied to the real bundled
`sample.png` at five coordinates outside the UI entirely, confirming
each shifted channel's before/after byte values match the documented
formula exactly (e.g. a shadow pixel's red channel `20 → 84` at luma
≈45, matching `20 + shadow_weight × 100` to the nearest byte).

**265 Rust tests total** (256 → 265, 258 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 12 — Select > Modify > Smooth / Border (rounded-rectangle and ring selections)

Select > Modify > Smooth rounds a selection's corners. Photoshop's own
Smooth operates on arbitrary, possibly irregular selections by rounding
off jagged edges and filling small gaps in a pixel-mask representation.
This project's selection system represents a selection as a shape plus
its bounding box rather than a mask (`Rectangle` or `Ellipse`, plus an
`inverted` flag) — cheap to clone and exact for those two shapes, but
with no notion of "jagged pixels" to smooth away. The well-defined
analogue on a `Rectangle` selection is to round its corners by a given
radius, which is exactly what a third `SelectionShape::RoundedRectangle
{ radius }` variant adds. Applied to an `Ellipse` selection, Smooth is a
no-op: an ellipse's boundary is already smooth everywhere, so rounding
its nonexistent corners changes nothing — a deliberate scope cut in the
same spirit as every other adjustment in this project that trims
Photoshop's full generality down to a single well-defined behaviour.
`radius` is clamped to at most half the shorter side of the selection's
bounding box, since a larger corner radius has no further visual effect
once the rectangle is already as rounded as it can get (a "stadium"
shape). Smooth is an error if nothing is selected, or if `radius` is
zero.

Containment for the new shape is a standard rounded-rectangle hit test:
clamp the query point onto the rectangle inset by `radius` on every
side, then require the point be within `radius` of that clamped point.
On a flat edge (away from any corner) this reduces to an ordinary
straight-edge distance check, so — unlike an ellipse — a rounded
rectangle's flat sides stay selected right up to their original
boundary; only the four corner regions get cut away. `SelectionShape`
being a mixed enum (two unit variants, one struct variant) serializes
under serde's default external tagging as `"rectangle"` / `"ellipse"`
for the old two, and `{ roundedRectangle: { radius } }` for the new one
— no extra derive attributes needed.

`Document::smooth_selection` is a new top-level command alongside the
existing `expand_selection`/`contract_selection`, exposed in the
frontend as a **Smooth…** toolbar button that reuses the same shared
Expand/Contract dialog (a small heading/label lookup table now keys off
all three modes instead of a binary ternary) and sends a `radius`
parameter instead of `amount`. The marching-ants selection outline gains
a `selectionRadiusStyle` helper that expresses the pixel radius as CSS's
independent horizontal/vertical border-radius percentages (`x% / y%`),
so the displayed rounded corners track the true pixel radius even
though the outline element itself is laid out in percentages, not
pixels, of the canvas.

**Verified two ways.** New `document.rs` tests cover: smoothing a
rectangle producing the expected `RoundedRectangle` shape with unchanged
bounds, the radius being clamped to half the shorter side, smoothing an
ellipse being an exact no-op, a zero radius being an error, smoothing
with nothing selected being an error, and — mirroring the existing
ellipse-corner-exclusion test — a rounded rectangle excluding a true
corner pixel from a brush stroke while still including a pixel on the
flat middle of an edge (demonstrating rounding only cuts the corners,
not the whole boundary, unlike an ellipse). Live under Xvfb: created an
800×600 document, used **Select All** to get a full-canvas rectangle
selection (a live pointer-drag on an empty canvas proved unreliable to
drive headlessly and isn't this feature's concern), opened **Smooth…**,
set the radius to `60`, and applied — the marching-ants outline visibly
grew rounded corners while its flat edges stayed straight, exactly the
CSS helper's intent. Switched to the Brush tool and clicked once inside
a corner that the rounding had cut away (no paint landed — correctly
blocked) and once in the selection's centre (a paint dot appeared),
confirming paint confinement respects the new shape exactly as the unit
tests already proved algebraically.

**271 Rust tests total** (265 → 271, 264 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

**Border.** Select > Modify > Border turns a selection into a band hugging
the *inside* of its own edge, excluding the interior beyond that band —
the classic "picture frame" selection, useful for painting an outline
around a shape without touching its middle. Photoshop's own Border
straddles the original edge (extending outward too, into fresh canvas
area that would need re-clamping) and feathers the result; this
hard-edged selection system instead keeps the shape's *outer* boundary
exactly where it was and only carves a same-shaped hole out of the
interior — a deliberate scope cut that still produces the same everyday
"frame a selection" effect without growing the bounding box. Once the
border width is at least half the shorter side, the hole disappears
entirely and the whole shape is selected again, same as before Border
was applied. Reapplying Border recomputes the band from the selection's
original shape, not the current ring — it does not stack into a border
of a border. An error if nothing is selected, or the width is zero.

Rather than a new `SelectionShape` variant, Border is a new `border:
Option<u32>` field directly on `Selection`, composing with *any* shape
— `Rectangle`, `Ellipse`, or the `RoundedRectangle` Smooth added — since
containment only needed a small refactor: the shape-matching logic
inside `Selection::contains` was pulled out into a free `shape_contains(shape,
bounds, px, py)` function, and border containment is just "inside the
shape at the selection's own bounds, but *not* inside that same shape
re-tested against a `shrink_rect`-shrunk copy of those bounds." A
`RoundedRectangle`'s radius is defensively re-clamped inside
`shape_contains` itself (not only at creation) since a Border-shrunk
inner rectangle can be smaller than the shape's original radius.

`Document::border_selection` is a new top-level command alongside
`smooth_selection`, exposed in the frontend as a **Border…** toolbar
button that plugs into the same shared Expand/Contract/Smooth dialog
(the heading/label lookup table now covers all four modes) and sends a
`width` parameter. The marching-ants outline gains a second, inner
outline — computed via a JS `shrinkBounds` mirroring the Rust
`shrink_rect`, reusing the existing `overlayStyle`/`selectionRadiusStyle`
helpers — whenever `border` is set and hasn't collapsed the hole away.

**Verified two ways.** New `document.rs` tests cover: Border setting the
`border` field without touching `shape` or `bounds`, a zero width being
an error, bordering with nothing selected being an error, a rectangle
border selecting a pixel near the edge while excluding one at dead
centre, a border at least half the shorter side selecting the whole
shape (the hole having collapsed away), and an ellipse border selecting
a ring — a pixel between the inner and outer ellipse radii is selected,
one inside the inner ellipse is excluded. Live under Xvfb: created an
800×600 document, used **Select All** for a full-canvas rectangle
selection, opened **Border…**, set the width to `60`, and applied — the
marching-ants outline visibly grew a second, inset rectangle 60px in
from every edge, forming a clear picture-frame ring. Switched to the
Brush tool and clicked once in the band between the two outlines (a
paint dot appeared) and once in the centre hole (no paint landed —
correctly blocked), confirming paint confinement respects the ring
exactly as the unit tests already proved algebraically.

**277 Rust tests total** (271 → 277, 270 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 13 — Layer > Rasterize (a genuine no-op)

Layer > Rasterize converts a vector, text, or smart-object layer into an
ordinary pixel layer. Every `Layer` in this app has been a document-sized
RGBA8 pixel buffer since Phase 1 — there is no vector, text, shape, or
smart-object layer type to convert *from* (the same fact `PIXEL LAYER`
in `docs/PHOTOSHOP_PARITY.md` already records as trivially true) — so
`Document::rasterize_layer` is always a genuine no-op. Rather than
leaving this unimplemented or checking the parity box off with only a
documentation note, it ships as a real, tested, UI-reachable command:
it validates that the given id names an existing layer (the same "No
layer with id N" error every other layer command gives for an unknown
id) and otherwise touches nothing at all — no pixels, no dirty rect, no
document state — exactly matching Photoshop's own behaviour of
disabling the Rasterize command entirely once a layer is already
pixels, rather than silently accepting the click and doing something
unexpected.

Unlike every paint or adjustment command in this project, Rasterize
does not check the layer's pixel lock: since it never touches pixels,
whether the layer is locked is irrelevant to it, and a locked layer
rasterizes successfully just like an unlocked one. The Tauri command
wrapper still checkpoints it through the usual `edit_checkpointed`
path, for consistency with how every other layer command is wired
in, even though undoing a Rasterize is invisible by construction.

The frontend adds a **Rasterize Layer** button to `LayerPanel`,
alongside the existing Merge Down button, wired straight through to the
new command with no dialog — there is nothing to configure.

**Verified two ways.** New `document.rs` tests cover: rasterizing an
existing layer leaving the whole document view byte-for-byte identical,
rasterizing a locked layer still succeeding (the one place in this
project a locked layer accepts a command that would otherwise be
blocked), and rasterizing an unknown layer id being an error. Live
under Xvfb: created an 800×600 document, clicked **Rasterize Layer** in
the layer panel — no error notice appeared, the layer stayed present
with its name and content unchanged, and Undo became available (the
command was checkpointed like any other), confirming the command
reaches the backend and returns successfully with zero visible effect,
exactly as designed.

**280 Rust tests total** (277 → 280, 273 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 14 — Layer > New Fill Layer > Solid Color

Layer > New Fill Layer > Solid Color adds a new top layer filled
entirely with a chosen colour. A real Photoshop fill layer stays
"live" — double-clicking it later reopens a colour picker and repaints
the whole layer in place, all without needing a mask or touching any
layer below it. This app's layer model has no such generative layer
kind (every layer is an ordinary pixel buffer, the same fact the
`PIXEL LAYER` and `RASTERIZE` entries in `docs/PHOTOSHOP_PARITY.md`
already record), so the scope cut here is the same one Add Layer (from
a PNG file) already makes: `Document::add_solid_color_layer` creates an
ordinary pixel layer whose initial content happens to be a flat fill,
exactly as if the whole canvas had been painted with the Paint Bucket
at 100% opacity — editable afterward like any other layer, just not
re-openable as a live "recipe." The new layer is always named "Color
Fill 1" — there is no auto-incrementing layer-name scheme in this app
yet (the first layer of a brand new document is likewise always
plainly "Layer 1"). The function cannot fail: a colour and the
document's own size are always valid, so unlike `add_layer` it returns
a bare `LayerId` rather than a `Result`.

The frontend adds a **Solid Color…** toolbar button next to **Add
layer…**, gated on a document being open (not on a layer being
selected, since it always adds a new layer regardless of what else is
selected). It opens a small modal with a single `<input type="color">`
swatch and an **Add Layer** button — no dialog complexity beyond
picking the colour, since there is nothing else to configure.

**Verified two ways.** New `document.rs` tests cover: a solid colour
layer filling every pixel of the canvas with the exact requested RGBA
value, the new layer being named correctly and pushed onto the top of
the stack, and the fill colour's alpha channel being honoured (a
semi-transparent fill layer). Live under Xvfb: created an 800×600
document (starting with one ordinary transparent "Layer 1"), opened
**Solid Color…**, left the colour picker at its default white, and
clicked **Add Layer** — a new "Color Fill 1" layer appeared at the top
of the layer panel and the canvas immediately went from the
transparent checkerboard to solid opaque white across its full extent,
confirming the command reaches the backend, creates a real layer, and
composites correctly.

**283 Rust tests total** (280 → 283, 276 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 15 — Layer > New Fill Layer > Gradient

Layer > New Fill Layer > Gradient adds a new top layer filled with a
linear gradient from a start colour to an end colour, running the
canvas's own top-left-to-bottom-right diagonal. It reuses the exact
same linear-interpolation math the Gradient tool's own `gradient_fill`
already implements — `Document::add_gradient_layer` creates a brand new
fully transparent layer via `add_solid_color_layer` (Phase 14) and then
runs `gradient_fill` across it from `(0, 0)` to `(width, height)`.
Photoshop's own Gradient Fill Layer dialog lets you configure angle,
scale, gradient style (linear/radial/angle/reflected/diamond), and
offset; this always uses a fixed linear diagonal — a deliberate scope
cut, in the same spirit as Gradient Map's own fixed two-stop straight
line. The function cannot fail: the freshly created layer is never
locked, and a document's diagonal is always nonzero (a document can't
be 0×0), so the two preconditions `gradient_fill` itself checks always
hold — enforced with an `.expect()` documenting exactly why, rather
than threading a `Result` through for an error that can't happen. The
new layer is always named "Gradient Fill 1", matching "Color Fill 1"
and "Layer 1"'s equally fixed naming.

The frontend adds a **Gradient Fill…** toolbar button next to **Solid
Color…**, opening a modal with two colour-picker swatches (Start Color,
End Color) and an **Add Layer** button — the same two-colour dialog
shape Gradient Map and Photo Filter already use, just producing a new
layer instead of adjusting an existing one.

**Verified two ways.** New `document.rs` tests cover: a gradient layer
interpolating along the canvas diagonal at exactly the byte values
`gradient_fill_interpolates_along_the_line` already established for a
horizontal gradient (the same `t=0.25`/`t=0.75` fractions arise from a
square canvas's diagonal as from a horizontal line, letting the two
tests cross-check each other), the new layer being named correctly and
pushed onto the top of the stack, and a fully transparent start/end
colour leaving the new layer fully transparent (nothing to show through
on a layer that starts out blank). Live under Xvfb: created an 800×600
document, opened **Gradient Fill…**, left both colour pickers at their
black/white defaults, and clicked **Add Layer** — a new "Gradient Fill
1" layer appeared at the top of the layer panel and the canvas
immediately displayed a smooth diagonal gradient running black at the
top-left corner to white at the bottom-right, confirming the command
reaches the backend, creates a real layer, runs the gradient fill on
it, and composites correctly.

**286 Rust tests total** (283 → 286, 279 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 16 — Edit > Transform > Flip Horizontal / Flip Vertical / Rotate 180°

Adds the three dimension-preserving members of Edit > Transform: Flip
Horizontal, Flip Vertical, and Rotate 180°, each mirroring or rotating
a single layer's own pixels in place. All three apply to the whole
layer regardless of any active selection — modelled on Image > Image
Rotation, which is likewise unaffected by a selection, rather than the
selection-aware behaviour Edit > Transform can have on a normal layer
in real Photoshop. Precisely constraining a flip or rotation to an
arbitrary (possibly non-rectangular) selection shape would need a real
pixel mask this project's shape+bounds selection system doesn't have —
a deliberate scope cut, in the same spirit as Border's own inability to
straddle a selection's original edge.

`flip_layer_horizontal` and `flip_layer_vertical` swap pixels
two-pointer style — column pairs for a horizontal flip, whole rows for
a vertical one (`swap_with_slice` on a `split_at_mut` pair, no
allocation) — leaving an unpaired middle row/column of an odd-sized
layer untouched. `rotate_layer_180` is implemented directly as a
single reversal of the whole pixel buffer: swapping the pixel at index
`i` with the one at `total - 1 - i` is exactly the same transform as
`(x, y) -> (width-1-x, height-1-y)` for a row-major buffer, so there's
no need to compose a horizontal and a vertical flip. None of the three
change a layer's dimensions — every layer stays document-sized, so all
three are always well-defined — unlike a 90° rotation, which would
need to swap width and height and so isn't offered here; the checklist
entries for Rotate 90° Clockwise/Counter Clockwise stay unchecked with
that reasoning noted directly in `docs/PHOTOSHOP_PARITY.md`, rather
than silently skipped. All three error the same way every other
pixel-rewriting command does: unknown layer id, or a locked layer.

The frontend adds a **Flip H** / **Flip V** / **Rotate 180°** row of
buttons to `LayerPanel`, right below **Rasterize Layer**.

**Verified two ways.** New `document.rs` tests cover: a horizontal flip
mirroring a 3-pixel row exactly (including the untouched middle pixel
of the odd width), a vertical flip mirroring a 3-pixel column the same
way, a 180° rotation mapping each of a 2×2 layer's four pixels to its
diagonally opposite corner, and both a locked-layer and an
unknown-layer error for each of the three commands. Live under Xvfb:
created an 800×600 document, loaded the bundled colourful sample-image
gradient as a layer, clicked **Flip H** — the gradient's blue corner
moved from top-left to top-right exactly as expected — then **Flip V**
on top of that, landing on the same result a straight 180° rotation of
the original would produce (verified by eye against the corner
colours). Started a fresh window and repeated with **Rotate 180°**
directly on the untouched original: the top-left corner's colour
became what had been the bottom-right corner's, and vice versa,
confirming the single-pass buffer reversal produces the exact same
result the two composed flips did.

**295 Rust tests total** (286 → 295, 288 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 17 — Image > Image Rotation > 90° Clockwise / 90° Counter Clockwise

Phase 16's Rotate 180° stayed per-layer because a half turn preserves
dimensions; a quarter turn can't (a W×H canvas becomes H×W), so Rotate
90° needed a different shape of command entirely — one that resizes
the whole document, not one layer. `Document::rotate_document_90`
rebuilds every layer's pixel buffer at the swapped dimensions and
updates the document's own `width`/`height` together, so the "every
layer stays document-sized" invariant this project relies on
throughout never breaks, even transiently. Each layer's new buffer is
filled by pulling from the old one: for clockwise, new pixel `(nx,
ny)` comes from old pixel `(ny, old_height - 1 - nx)`; for
counter-clockwise, from `(old_width - 1 - ny, nx)` — the standard
"transpose, then reverse rows/columns" matrix rotation. Both formulas
were derived by hand against a small lettered 2×3 grid (documented
directly in the function's own doc comment and its tests) rather than
trusted from memory, and cross-checked by a round-trip test: four
successive clockwise rotations, and separately four successive
counter-clockwise ones, both return a layer to its exact original
pixels and the document to its original dimensions.

The active selection and whatever `reselect` would have restored are
both cleared by a rotation: a selection's bounds are meaningless
against a document whose dimensions just changed shape, and there's no
sensible way to carry either forward. The operation cannot otherwise
fail — every layer is exactly document-sized before and after by
construction, so there's nothing to validate — even a document with no
layers yet simply swaps its own width and height.

The frontend adds a **Rotate 90° CW** / **Rotate 90° CCW** button pair
to the main toolbar (not `LayerPanel`, since this acts on the whole
document rather than one layer), gated on a document being open.

**Verified two ways.** New `document.rs` tests cover: clockwise and
counter-clockwise rotation each matching the hand-derived 2×3 example
exactly, the four-rotations-returns-to-original round trip in both
directions, a document with no layers still swapping its width and
height, and a rotation clearing both the active selection and the
reselect history. Live under Xvfb: created an 800×600 (landscape)
document, loaded the bundled colourful sample-image gradient as a
layer, and clicked **Rotate 90° CW** — the canvas immediately became a
600×800 portrait (confirmed by the dimensions readout at the bottom of
the window), with every corner's colour landing exactly where the
hand-derived formula predicts (the original top-left blue corner
moved to the new top-right, the original bottom-left teal corner
became the new top-left, and so on for all four corners). Clicking
**Rotate 90° CCW** immediately afterward rotated it straight back to
the original 800×600 orientation with the original corner colours
restored exactly, confirming the two directions are true inverses of
each other.

**300 Rust tests total** (295 → 300, 293 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 18 — Edit > Copy / Cut / Paste (and Paste Special > Paste in Place)

Every prior increment either read a layer's pixels in place or rewrote
them in place; this one is the first to move pixels *between* layers
and hold them somewhere outside the document entirely between the two
halves of the gesture. A new opaque `document::Clipboard` type — a
sub-rectangle's worth of RGBA8 pixels plus the document coordinates it
was captured from — is threaded through three new
`Document` methods and stashed on `AppState` in `lib.rs`
(`clipboard: Mutex<Option<Clipboard>>`), deliberately *not* on
`Document` itself: a real clipboard survives undo, redo, and even
switching to a different document, none of which anything `Document`
tracks does, so it needed to live one level up, alongside (but
independent from) the undo/redo history.

`Document::copy(id)` captures layer `id`'s pixels within the active
selection's bounding box — or the whole canvas, with no selection —
into a `Clipboard`. It doesn't just crop to that box: a shared
`extract` helper walks every pixel in the box and tests it against the
selection's own shape (via the existing `Selection::contains`), so an
ellipse, a rounded rectangle, a bordered ring, or an inverted selection
all copy out with the pixels outside their actual shape (but inside
the bounding box) coming back fully transparent — exactly as pasting
that clipboard onto an empty layer would look. `Document::cut(id)` is
`copy` followed by clearing (to `[0, 0, 0, 0]`) exactly the same
selection-masked pixels from the source layer, and reports that region
as the dirty rect for recompositing. Copying is allowed from a locked
layer (nothing is written, so there's nothing to protect against,
matching Photoshop's own behaviour); cutting still checks the lock, the
same as every other command that rewrites a layer's pixels, and leaves
both the document and whatever was already on the clipboard untouched
if it fails.

`Document::paste(clipboard, name)` adds the clipboard's contents as a
new top layer, positioned at the exact document coordinates it was
copied from. This app has no scrollable viewport to paste into the
middle of — the canvas is always shown at its own document
coordinates — so a plain Paste landing back at the original position
*is* Paste Special > Paste in Place, and both menu items are backed by
the same one command; Paste Into and Paste Outside are not (they'd
need clipping the paste to a *second* selection, not just placing it),
and stay unchecked in `docs/PHOTOSHOP_PARITY.md`. Because the
clipboard outlives the document it was copied from, pasting is clipped
per-pixel against whatever document is open *now*, which can have
different dimensions than the one at copy time — after a 90° rotation
(Phase 17), say, or after opening a different image. `paste` cannot
fail: a paste that lands partly or fully outside the current canvas
just produces a new layer with that much less visible on it, the same
as pasting into a too-small canvas in real Photoshop.

The three new Tauri commands are `copy`, `cut`, and `paste`. `cut` and
`paste` are checkpointed like any other discrete edit; `copy` doesn't
touch the document at all, but still returns a full `Snapshot` (an
unchanged one) rather than `()`, purely so the frontend can drive it
through the same `runCommand` path as every other command instead of a
one-off. The frontend adds a **Copy** / **Cut** / **Paste** button
group to the main toolbar (Copy and Cut gated on a selected layer,
Paste on a local `canPaste` flag that flips true the first time either
Copy or Cut succeeds and never flips back — mirroring the backend
clipboard's own "outlives everything" lifetime) plus the usual
Ctrl/Cmd+C / X / V keyboard shortcuts alongside the existing
Ctrl/Cmd+Z / Shift+Z / D / A / Shift+I bindings.

**Verified two ways.** New `document.rs` tests cover: copying the
whole layer with no selection; copying only a rectangular selection's
bounding box; copying through a non-rectangular (ellipse) selection,
hand-verified pixel-by-pixel against the ellipse's own inside/outside
math for all 16 pixels of a 4×4 canvas; copying from a locked layer
succeeding; cutting clearing exactly the selected pixels and reporting
that rect dirty, with the untouched pixels around it spot-checked;
cutting a locked layer failing and leaving it byte-for-byte unchanged;
pasting landing a copied region at its original coordinates on a
brand-new top layer; pasting clipping correctly into a *smaller*
current document (exercising both the row-break and column-skip
clipping paths in one test); and pasting a clipboard whose origin is
now entirely outside the current canvas producing an all-transparent
layer without panicking. Live under Xvfb: opened the bundled
colourful gradient sample, dragged a rectangular selection over its
top-left 2×2 tile block, clicked **Copy**, then **Paste** — a new
"Pasted Layer" appeared in the layer list sitting exactly over the
original colours (invisible on canvas since the content is identical,
as expected). Deleted that layer, clicked **Cut** on the same
selection, and watched that same 2×2 block turn solid black on the
base layer — confirming the pixels were actually cleared, not just
logically tracked. Clicked **Paste** again and the block's original
blue-to-purple gradient colours reappeared exactly where they'd been
cut from, on a fresh "Pasted Layer", confirming the full
copy/cut/paste round trip end to end through the UI.

**311 Rust tests total** (300 → 311, 304 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 19 — Edit > Delete (and Clear) / Edit > Fill

Two small, closely related commands that both reuse machinery Phase 18
just built for Cut: the same "walk `bounds`, test each pixel against
the active selection, overwrite the ones inside it" loop, just with a
different destination colour. That loop was pulled out of `cut` into
a new private `Document::paint_region(id, bounds, color)` helper, and
`cut` now calls it instead of carrying its own copy — so this phase
started with a small refactor (no behaviour change, covered by the
existing Cut tests continuing to pass) before adding the two new
public methods on top of it:

- `delete_selection(id)` — Edit > Delete — calls `paint_region` with
  `color = [0, 0, 0, 0]`: the active selection (or the whole layer,
  with none) goes fully transparent. This app has no separate Edit >
  Clear command: Clear only differs from Delete in real Photoshop when
  the target is the special locked "Background" layer (Clear there
  fills with the background colour instead of erasing, since a
  Background layer can't hold transparency); every layer in this app
  already supports transparency, so Delete and Clear would be
  byte-for-byte identical here, and one command covers both menu
  items.
- `fill_selection(id, color)` — Edit > Fill — calls `paint_region`
  with any `color`, overwriting the selection with a flat colour
  instead of clearing it. This is deliberately not the same code path
  as the existing `flood_fill` (Paint Bucket): Paint Bucket stops at a
  colour boundary from a seed point, while Fill paints every selected
  pixel unconditionally, matching Photoshop's own distinction between
  the two. The colour source is a single RGBA value — no pattern,
  history, or content-aware fill sources, and no blend-mode/opacity
  options beyond 100% Normal — the same "paint once, flatly, no live
  recipe" scope cut `add_solid_color_layer` (Phase 14) already made
  for a brand new layer, just applied here to an existing one in
  place.

Both commands are confined to the selection's exact shape, not just
its bounding box, the same as Copy/Cut: `paint_region` tests every
pixel with `Selection::contains`, so filling or deleting through an
ellipse or rounded-rectangle selection leaves the corners outside the
shape untouched.

The frontend adds **Delete** and **Fill…** buttons to the existing
Copy/Cut/Paste toolbar group (both gated on a selected layer). Fill
opens a small modal with a single colour swatch, mirroring the Solid
Color Fill Layer dialog's own layout; Delete needs no dialog and acts
immediately. Neither got a keyboard shortcut: an unmodified Delete/
Backspace binding would fight with every text and number input already
on the page (typing in the Fill colour field, a Levels input, etc.),
so this stays a toolbar-only action — a deliberate, documented scope
cut rather than an oversight.

**Verified two ways.** New `document.rs` tests cover: deleting only
the selected region and leaving the rest of the layer untouched (with
the exact dirty rect asserted); deleting with no selection clearing
the whole layer; deleting on a locked layer failing and leaving it
byte-for-byte unchanged; filling only the selected region with the
given colour while the rest is untouched; filling with no selection
filling the whole layer; filling through a non-rectangular (ellipse)
selection leaving the corners outside its shape at their original
colour (reusing the same hand-derived 4×4 ellipse layout from Phase
18's copy-masking test); and filling on a locked layer failing and
leaving it unchanged — eight tests in all, plus the usual "unknown
layer id is an error" case for each command. Live under Xvfb: opened
the bundled colourful gradient sample, dragged a rectangular selection
over its top-left 2×2 tile block, opened **Fill…**, and clicked
**Fill** with the default white swatch — the selected block turned
solid white while every pixel outside it kept its original gradient
colour. Clicked **Delete** on the same still-active selection
afterward and watched that same white block turn fully transparent
(matching the same background colour Phase 18's Cut test produced),
confirming both commands actually rewrite pixels rather than just
updating some tracked state.

**320 Rust tests total** (311 → 320, 313 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 20 — Filter > Blur > Box Blur

The first filter in this app that reads from more than one source pixel
to produce a single output pixel — every prior adjustment (Levels,
Curves, Threshold, Photo Filter, ...) is a pure per-pixel function, but
a blur is inherently a neighbourhood operation, so this phase is the
project's first real convolution. A box blur (a flat mean over a
square window) is the simplest one there is — much simpler than a true
Gaussian blur's bell-curve-weighted average — which made it the
well-scoped starting point for this whole family of filters rather
than Gaussian Blur itself.

`Document::box_blur(id, radius)` walks every pixel in the active
selection (or the whole layer, with none) and replaces it with the
flat average of every channel — R, G, B, and A independently — across
a `(2*radius+1)`-square window centred on it. Sampling past a layer's
edge repeats the edge pixel (clamp-to-edge) rather than wrapping
around or padding with transparency, which has a second useful effect
beyond avoiding a black/transparent fringe at the border: every
window, everywhere, is exactly `(2*radius+1)^2` samples, so the
integer-division rounding in the average is uniform across the whole
layer instead of shifting depending on how close a pixel is to an
edge. Every sample is read from a snapshot of the layer's pixels taken
before the pass starts, so pixels already blurred earlier in the same
pass never leak into pixels blurred later — a genuine "old pixels in,
new pixels out" convolution rather than an accidental IIR filter.

The averaging is deliberately not alpha-aware: Photoshop's own blur
filters treat colour as premultiplied by alpha internally, so a fully
opaque pixel blurring toward a fully transparent neighbour doesn't
pick up a dark fringe from that neighbour's arbitrary, invisible RGB
values. This implementation averages the four channels completely
independently and un-premultiplied — the same "no extra colour science
beyond what's already stored in the file" scope cut the Levels, Curves,
and Color Balance adjustments already make elsewhere in this project.
A blur near a hard transparency edge can therefore show a faint fringe
that real Photoshop wouldn't, a known, documented limitation rather
than a bug.

The frontend adds a **Box Blur…** button to the adjustments toolbar
group (next to Color Balance), opening a dialog with a single Radius
slider (1–40px, defaulting to 4) — the same layout as the existing
Threshold dialog's single-slider pattern.

**Verified two ways.** New `document.rs` tests build a small 3×3 test
layer whose red channel climbs left-to-right, top-to-bottom (10, 20,
30 / 40, 50, 60 / 70, 80, 90) specifically so every pixel's blurred
value can be hand-derived from its position alone: the centre pixel's
full 3×3 window averages back to its own original value (450⁄9 = 50,
exactly, since the grid is symmetric around the centre); the top-left
corner's edge-clamped window comes out to 210⁄9 = 23 (asserting the
*truncating*, not rounding, integer division); the bottom-right
corner comes out to 690⁄9 = 76; and the uniformly-255 alpha channel
survives the average exactly, confirming it really is blurred through
the same code path as the colour channels rather than being special-
cased. A second test confines a selection to a single pixel and
reuses the same hand-derived corner value as a built-in
cross-check, confirming every pixel outside the selection is left
completely untouched. Locked-layer, unknown-layer, and zero-radius
error cases round out the set. Live under Xvfb: opened the bundled
colourful gradient sample (a grid of flat-coloured tiles separated by
sharp white grid lines) and applied **Box Blur…** at the default 4px
radius — every grid line across the whole canvas visibly softened
into a blurred gradient in a single click, confirming the filter
applies across the entire layer, not just near where the cursor
happened to be.

**325 Rust tests total** (320 → 325, 318 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 21 — Layer > Duplicate Layer

While surveying `docs/PHOTOSHOP_PARITY.md` for the next candidate, it
turned up that Duplicate Layer — Photoshop's Ctrl/Cmd+J, one of the
most reached-for Layer menu commands there is — was simply missing
from the ~500-item audit the checklist was extracted from at the start
of this project, even though closely related commands (Merge Down,
Merge Visible, Flatten, Rasterize) were all tracked and already
shipped. Rather than build it "for free" and leave the tracked total
silently wrong, `docs/PHOTOSHOP_PARITY.md` gained a new line for it
directly under RASTERIZE in PART III, with a note explaining the gap —
bumping the denominator from 590 to 591 distinct capabilities tracked,
not just the shipped count.

`Document::duplicate_layer(id)` clones the target layer's pixels and
every attribute (visibility, opacity, blend mode, lock state) as a new
layer inserted directly above the original — Photoshop's own
placement, not necessarily the very top of the stack, which is what
every other "add a layer" command in this app (`add_layer`,
`add_solid_color_layer`, `add_gradient_layer`) does instead. The whole
`Layer` struct is cloned rather than its fields copied out by hand, so
a future field added to `Layer` is duplicated correctly without this
function needing to change. The duplicate's name is the original's
with `" copy"` appended, matching Photoshop's own default naming
before a user renames it. The only failure mode is an unknown layer
id; duplicating a locked layer is fine, and the duplicate itself
starts out locked too, matching the original.

Because a duplicate doesn't always land at the top of the stack, this
phase also had to extend the frontend's own `runCommand` selection
logic: previously the only special case was `selectAfter: "top"`
(select whatever ends up topmost), which is wrong here whenever the
duplicated layer wasn't already the top one. `runCommand` now also
accepts `selectAfter: { above: <id> }`, which finds where `<id>` (the
layer that was just duplicated) ended up in the *new* layer list and
selects whatever landed directly above it — exactly the newly created
duplicate, by construction, regardless of where in the stack the
original sat. The frontend adds a **Duplicate Layer** button to the
layer panel's per-layer controls, right after Rasterize Layer.

**Verified two ways.** New `document.rs` tests cover: a duplicate
landing directly above its original in a three-layer stack (not at the
top, since the original wasn't the top layer either) with a distinct
id from the original; every attribute (opacity, blend mode, lock
state) and the exact pixel buffer surviving the copy, with `" copy"`
appended to the name; the original layer being completely untouched
after duplicating it; and the usual "unknown layer id is an error"
case. Live under Xvfb: opened the bundled gradient sample (one layer,
"sample.png"), clicked **Duplicate Layer**, and watched a new
"sample.png copy" layer appear directly above the original in the
layer panel, already selected (confirming the new `{ above }`
selection logic picked the actual duplicate, not just whatever ended
up on top) — the layer count and the newly-enabled Merge Down button
both confirmed a second, real layer now exists.

**329 Rust tests total** (325 → 329, 322 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 22 — Filter > Sharpen > Unsharp Mask

Photoshop's own Unsharp Mask is the classic "subtract a blurred copy
from the original, then add that difference back in, amplified" edge
enhancement — and Phase 20's box blur turned out to be exactly the
low-pass filter it needs, making this the natural next filter once box
blur existed rather than an unrelated new piece of infrastructure. Its
convolution loop was pulled out into a new free function,
`box_blur_at(source, doc_width, width, height, row, col, radius)`,
which computes just the blurred value at one pixel; `box_blur` itself
now calls it once per pixel and writes the result straight to the
layer (a pure refactor, verified by the existing box-blur tests
continuing to pass with their exact same hand-derived values), and
`unsharp_mask` calls the same function to get its "blurred copy"
without duplicating a single line of sampling logic.

`Document::unsharp_mask(id, radius, amount, threshold)` computes, for
every pixel in the active selection (or the whole layer, with none):
`diff = original - blurred` on the R, G, and B channels only — alpha
is a transparency channel, not a contrast one, so sharpening leaves it
completely alone. If `|diff|` is at least `threshold`, the output is
`original + diff * amount`, clamped to `0..=255`; otherwise the pixel
is left exactly as it was. `threshold`'s whole job is protecting flat,
low-contrast regions (skin, sky) from picking up sharpening noise
while real edges — where `|diff|` is large — still get boosted, the
same purpose it serves in Photoshop's own dialog. `amount` is a plain
multiplier here (`1.0` is a nominal "100%") rather than Photoshop's
1–500% dial with its own internal scaling; the frontend still presents
it as a 1–500% slider and divides by 100 before sending it to the
backend, so the dialog itself matches Photoshop's own numbers exactly.
Errors on a zero radius, a non-finite or non-positive amount, or a
locked/unknown layer.

The frontend adds an **Unsharp Mask…** button next to Box Blur in the
adjustments toolbar group, opening a dialog with three sliders —
Amount (1–500%), Radius (1–40px), and Threshold (0–255) — matching
Photoshop's own three-control layout for this exact dialog.

**Verified two ways.** New `document.rs` tests reuse the same
hand-built 3×3 ramped test layer from the box-blur tests (red channel
climbing 10 → 90 by tens) so every sharpened value can be derived from
already-known box-blur results: the centre pixel's blurred value (50)
equals its original, so `diff = 0` and it's left unchanged; the
top-left corner's original (10) and box-blurred (23) values give
`diff = -13`, and at 50% amount `10 + (-13 × 0.5) = 3.5`, which rounds
(half away from zero) to 4; the bottom-right corner's `diff = 14`
sharpens `90 + (14 × 0.5)` to exactly 97. A second test sets the
threshold above both corners' `|diff|` (13 and 14) with a full-strength
100% amount and confirms neither pixel moves — proving the guard
actually blocks a change that would otherwise happen, not just that
nothing happens by default. A third test confines the effect to a
single-pixel selection and confirms everything outside it is
untouched. Zero-radius, non-positive/non-finite-amount, locked-layer,
and unknown-layer error cases round out the set — 7 tests, all passing
on first run. Live under Xvfb: opened the bundled gradient sample and
applied **Unsharp Mask…** at an exaggerated 500% amount (radius 2px,
default threshold) — every white grid line between tiles immediately
grew a visible colour halo (a classic unsharp-mask ringing artifact,
the same overshoot real Photoshop produces at extreme settings),
confirming the filter is genuinely doing edge-contrast work rather
than a no-op.

**336 Rust tests total** (329 → 336, 329 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 23 — Filter > Blur > Motion Blur

The third filter built on the box-blur convolution shape this project
now has, and the first to change that shape rather than reuse it
outright: instead of averaging a square neighbourhood, Motion Blur
averages a straight line of samples through each pixel, along a
chosen direction. `box_blur_at`'s own sample-then-average loop was
split into two pieces to make this possible without duplicating the
averaging logic: a new `average_samples(source, doc_width, samples)`
takes any iterator of `(x, y)` coordinates and does the summing and
dividing, and `box_blur_at` now just builds a square iterator and
hands it off (a pure refactor — the existing box-blur tests pass
unmodified with their exact same hand-derived values). The new
`motion_blur_at` builds a *line* of coordinates instead: `2 * distance
+ 1` samples at integer steps from `-distance` to `distance` along
`(cos(angle), sin(angle))`, each offset rounded to the nearest whole
pixel (not a true anti-aliased line — the same hard-edged, no-
antialiasing scope cut this project's selection system already makes)
and clamped to the layer's own edges exactly like `box_blur_at`'s
square window is.

`Document::motion_blur(id, angle, distance)` walks the active
selection (or the whole layer) and, for every pixel, replaces it with
`motion_blur_at`'s directional average — all four channels, un-
premultiplied, the same scope cut `box_blur` and `unsharp_mask` both
already make. `angle` is in degrees, 0° horizontal, matching
Photoshop's own dial; `distance` behaves like `box_blur`'s own
`radius` (how far the line extends on *each* side of the pixel, so the
streak is `2 * distance + 1` pixels long) rather than Photoshop's
single "total streak length" number — the same "close enough, not a
pixel-for-pixel port of Photoshop's maths" simplification `box_blur`'s
own `radius` already makes. Errors on a zero distance, a non-finite
angle, or a locked/unknown layer.

The frontend adds a **Motion Blur…** button next to Unsharp Mask, with
an Angle slider (-180° to 180°) and a Distance slider (1–60px).

**Verified two ways.** New `document.rs` tests reuse the ramped 3×3
test layer a third time: at 0° (horizontal), motion blur reduces to a
1-D box average along each row, giving the same shape of hand-derived
values as the square box-blur tests — the left edge clamps to
`(10+10+20)/3 = 13`, the middle column averages back to its own
original `20` exactly (symmetric window), and the right edge clamps to
`(20+30+30)/3 = 26`; at 90° (vertical), the identical maths applies
down a column instead of along a row (`20`, `40`, `60`). A third test
confines the effect to a single-pixel selection and confirms
everything else is untouched; zero-distance, non-finite-angle, locked-
layer, and unknown-layer error cases round out the seven tests, all
passing on first run. Live under Xvfb: opened the bundled gradient
sample and applied **Motion Blur…** at its defaults (0°, 10px) — every
*vertical* white grid line between tiles was smeared away completely
along the horizontal blur direction, while every *horizontal* grid
line stayed perfectly sharp, visually confirming the blur really is
directional rather than a disguised box blur.

**343 Rust tests total** (336 → 343, 336 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 24 — Layer > New > Layer via Copy / Layer via Cut

Photoshop's Ctrl/Cmd+J and Ctrl/Cmd+Shift+J: with a selection active,
lift just the selected pixels onto a brand-new layer — copying them
(the source stays as it was) or cutting them (the source is left with a
transparent hole). Like Duplicate Layer in Phase 21, neither was a
tracked line in the original audit `docs/PHOTOSHOP_PARITY.md` was
extracted from, so both were added there as their own lines under PART
III (591 → 593 tracked) rather than shipped uncounted.

The whole point of these commands, versus simply pressing Copy and then
Paste, is that they never go through the clipboard — the user's real
clipboard contents survive, and nothing the user previously copied can
leak in. That fell out almost for free from Phase 18's design: the
clipboard lives on `AppState` in `lib.rs`, not on `Document`, and
`Document::copy` / `Document::cut` merely *return* a `Clipboard` value —
storing it is the Tauri command's job. So `Document::new_layer_via_copy`
is literally `self.copy(id)?` followed by `self.paste(&clipboard, name)`,
and `new_layer_via_cut` is `self.cut(id)?` followed by the same `paste`,
with the `Clipboard` value living and dying inside the call. No new
pixel math anywhere: selection masking (ellipses, rounded rectangles,
borders, inversion), lock checking, and the paste-at-original-coordinates
placement are all inherited from the already-tested primitives. The
inherited lock semantics are deliberately asymmetric and are pinned by
tests: `via_copy` succeeds on a locked layer (nothing is written to it),
`via_cut` errors and leaves both the document and the layer stack
untouched. Both new layers land at the top of the stack — the same
simplification plain Paste already makes rather than Duplicate Layer's
"directly above the source" placement.

The frontend adds **Layer via Copy** and **Layer via Cut** buttons to
the Clipboard toolbar group, plus the Ctrl/Cmd+J and Ctrl/Cmd+Shift+J
shortcuts alongside the existing C/X/V bindings; the new layers are
named "Layer via Copy" / "Layer via Cut", Photoshop's own defaults.

**Verified two ways.** Six new `document.rs` tests: via-copy produces a
new layer holding exactly the selected region (transparent outside it)
while the source layer is byte-for-byte untouched; via-cut produces the
same new layer *and* clears the selected region on the source (with the
exact dirty rect asserted), leaving the unselected pixels alone; via-copy
succeeding on a locked layer versus via-cut refusing one and leaving the
layer count and pixels unchanged; and the unknown-layer error for each.
Live under Xvfb: opened the bundled gradient sample, dragged a rectangle
over the top-left tiles and clicked **Layer via Cut** — a new "Layer via
Cut" layer appeared on top, already selected, and the **Paste** button
stayed disabled throughout, the visible proof the clipboard was never
touched. Hiding that new layer exposed the transparent hole cut from
`sample.png` exactly under the selection; re-showing it, reselecting
the source layer and clicking **Layer via Copy** added a third "Layer via
Copy" layer with no new hole, Paste still disabled.

**349 Rust tests total** (343 → 349, 342 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 25 — Filter > Blur / Blur More and Filter > Sharpen / Sharpen More / Sharpen Edges

Photoshop's five no-dialog, one-click filters, and the first increment
where every new command is a thin fixed-parameter wrapper over filters
that already exist. Phases 20 and 22 built the two general tools —
`box_blur(radius)` and `unsharp_mask(radius, amount, threshold)` — and
each preset is one call into them with Photoshop's own intent baked in:

| Preset | Built as | Photoshop's description |
| --- | --- | --- |
| Blur | `box_blur(1)` | "softens by one pixel" |
| Blur More | `box_blur(3)` | "three to four times stronger than Blur" |
| Sharpen | `unsharp_mask(1, 0.5, 0)` | a light, everywhere boost |
| Sharpen More | `unsharp_mask(1, 1.0, 0)` | "a stronger Sharpen" |
| Sharpen Edges | `unsharp_mask(1, 1.0, 20)` | "sharpens only where there's an edge" |

Sharpen Edges is the interesting one: Photoshop's "leave smooth areas
alone" behaviour is precisely what Unsharp Mask's threshold already
does, so it's Sharpen More gated behind a threshold of 20 levels rather
than a new edge detector. Photoshop's Blur uses a lightly
centre-weighted 3×3 kernel where this app's is a flat 3×3 mean — the
same flat-versus-weighted simplification `box_blur` itself already
makes, restated here rather than hidden. All five inherit selection
confinement, lock checking, and error handling from the underlying
filter; nothing new touches pixels.

The frontend adds five buttons — **Blur**, **Blur More**, **Sharpen**,
**Sharpen More**, **Sharpen Edges** — after Motion Blur in the
adjustments toolbar row, each firing its command directly with no
dialog, as in Photoshop.

**Verified two ways.** Because each preset is a wrapper, its test pins
the preset to a value already hand-derived for the underlying filter at
exactly those parameters — a deliberate cross-check that the wiring
really lands on the intended parameters rather than merely "does
something": Blur reproduces the box-blur corner value 23; Sharpen the
unsharp-mask corner value 4 and bottom-right 97; Sharpen More the
full-strength 0 (clamped) and 104; Sharpen Edges leaves both corners at
10 and 90 because their |diff| of 13 and 14 sit under the threshold of
20. Blur More is the one genuinely new derivation: at radius 3 on the
3×3 ramped layer, offsets −3..=3 clamp onto row/column 0 four times, 1
once and 2 twice (per-axis weights 4/1/2, 49 samples), so the top-left
corner is 10·(3·5·7 + 7·5 + 49)/49 = 1890/49 = 38, and the centre's
symmetric 3/1/3 weighting gives 2450/49 = 50 exactly. A final test
confirms every preset propagates a locked-layer or unknown-layer error
from the filter beneath it. Live under Xvfb, on the bundled gradient
sample: **Blur More** softened every grid line into a wide haze in one
click; **Sharpen** (after an undo) snapped them back crisp with a faint
halo; **Sharpen More** produced a clearly darker halo band along every
line; and **Sharpen Edges** sharpened the lines while the smooth
gradient inside each tile stayed visibly untouched — the threshold
doing its job on real content.

**355 Rust tests total** (349 → 355, 348 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 26 — Filter > Noise > Median / Despeckle / Dust & Scratches

The first *rank* filter, and a different kind of neighbourhood operation
from every blur so far: instead of averaging a window, a median filter
sorts it and keeps the middle sample. That one change is why it does
what blurs can't — an isolated speck (dust, a hot pixel,
salt-and-pepper noise) never survives to the middle of the sorted list,
so it vanishes outright, while a genuine edge keeps a value from one
side or the other rather than a smeared blend. A new free function,
`median_at`, samples the same `(2·radius+1)`-square, edge-clamped window
`box_blur_at` uses, but collects each channel's samples into its own
list, sorts it, and takes the middle element; the window always holds
an odd number of samples, so there is a true middle and no averaging of
two neighbours is ever needed.

Three commands sit on top of it. `Document::dust_and_scratches(id,
radius, threshold)` is the general one: a channel is replaced by its
neighbourhood median only when it differs from that median by at least
`threshold` levels, which is exactly Photoshop's Threshold control —
a real speck differs a lot and is removed, fine low-contrast texture
differs only slightly and is left alone. `Document::median(id, radius)`
is that with a threshold of 0 (replace everything), and is implemented
*on top of* `dust_and_scratches` rather than the other way round for
that reason. `Document::despeckle(id)` is `median` at radius 1: a 3×3
median is the textbook implementation of Photoshop's own description
of Despeckle ("detects edges and blurs everything except them"). All
three inherit the pre-pass snapshot, selection confinement, lock
checks, and the "all four channels independently" scope cut from the
blur filters.

The frontend adds a **Median…** dialog (radius 1–16), a one-click
**Despeckle** button, and a **Dust & Scratches…** dialog (radius 1–16,
threshold 0–255) after the sharpen presets in the adjustments toolbar.

**Verified two ways.** Seven new `document.rs` tests reuse the ramped
3×3 layer whose box-blur windows were already derived by hand, so every
median can be checked against a known sample list: the centre's window
is all nine values 10..=90 and its 5th is 50; the top-left corner's
edge-clamped samples (10,10,20,10,10,20,40,40,50) sort to
10,10,10,10,20,20,40,40,50 with a 5th of 20 — where the mean gave 23,
the median lands on an actual sampled value — and the bottom-right
corner's sort to a 5th of 80 (the mean gave 76). A dedicated test puts
a single 255 speck in a flat field of 100 and confirms the median
throws it away entirely (100) where a box blur would only have dimmed
it to 117. The threshold test relies on both corners differing from
their medians by exactly 10: a threshold of 11 protects them, a
threshold of 10 (inclusive boundary) replaces them. Selection
confinement, zero-radius, locked-layer and unknown-layer errors, and
Despeckle-equals-radius-1-median round out the set, all passing on
first run. Live under Xvfb, on the bundled gradient sample: **Despeckle**
(and **Dust & Scratches** at its defaults, which is the same 3×3
median) visibly thinned the white grid lines — the sample's lines are
about two pixels wide, so a 3×3 window can't quite out-vote them —
while **Median…** at 2px (a 5×5 window) erased every grid line
completely, left the colour gradient entirely unblurred, and kept only
a tiny white dot at each line crossing: at an intersection the white
cross fills 16 of the 25 samples and so legitimately wins the median,
a textbook rank-filter artefact rather than a bug.

**362 Rust tests total** (355 → 362, 355 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 27 — Filter > Noise > Add Noise (Uniform / Gaussian / Monochromatic)

The first filter that needs randomness, which raised a question every
other filter got to skip: how do you hand-verify the exact bytes of a
random effect? The answer is to make the randomness deterministic per
seed. Rather than pull in the `rand` crate, `document.rs` gains a
20-line `XorShift32` (Marsaglia's xorshift32) whose entire value here is
that a test can seed it, compute the first few draws in a separate
script, and assert the filter's exact output — the same "hand-verified
expected values" bar every other phase meets. Photoshop's Add Noise is
deliberately different every time you run it; this app gets the same
behaviour by having the frontend send a fresh seed on every Apply, so
the determinism lives in the tests, not in the user's experience.

`Document::add_noise(id, amount, gaussian, monochromatic, seed)` maps
Photoshop's three controls directly. `amount` is its Amount dial as a
fraction of the full range (1.0 = 100%); each channel is offset by a
draw in −1..=1 scaled by `amount · 255`, rounded, and clamped to the
byte range. `gaussian` swaps the Uniform distribution for a bell curve,
approximated as the mean of three uniform draws (an Irwin–Hall
approximation — the same "close enough, no extra maths" simplification
`box_blur` makes versus a true Gaussian kernel). `monochromatic` uses a
single draw for R, G, and B together, so the grain is grey rather than
coloured. Alpha is never touched. Draws are consumed in a fully
specified order — row-major over the selection's bounding box, skipping
excluded pixels (which consume nothing), one draw per channel, or per
pixel when monochromatic, or three per channel/pixel when Gaussian — so
the exact output for a seed is defined, which is what makes the tests
possible. A zero seed is swapped for a fixed nonzero constant, since
xorshift's one hard rule is that zero is a fixed point.

The frontend adds an **Add Noise…** dialog with an Amount slider
(1–100%), a Distribution select (Uniform / Gaussian), and a
Monochromatic checkbox, after Dust & Scratches in the adjustments
toolbar; each Apply generates a new seed.

**Verified two ways.** Nine new `document.rs` tests. The generator's
own first outputs for seed 1 are pinned separately (270 369,
67 634 689, 2 647 435 461 — also derived by hand, then confirmed with a
Python re-implementation) so a regression in the PRNG and one in the
filter show up independently. Those draws map to −0.99987, −0.96851,
+0.23281, −0.85676, … and, at 25% amount on a flat 128 grey, give
exactly `[64, 66, 143]`, `[73, 135, 86]`, `[83, 77, 124]` for the first
three pixels in colour mode; `[64,64,64]`, `[66,66,66]`, `[143,143,143]`
in monochromatic mode (one draw per pixel); and `[91, 98, 95]` for the
first pixel in Gaussian mode (each channel the mean of three
consecutive draws) — all asserted byte-for-byte, with alpha 255
throughout. Further tests pin clamping at 100% amount (128 − 255 → 0,
128 + 59 → 187), determinism (the same seed twice gives identical
buffers, a different seed does not), selection confinement (unselected
pixels stay exactly 128), and the amount / locked-layer / unknown-layer
error cases — all passing on first run. Live under Xvfb, on the bundled
gradient sample: **Add Noise…** at 39% Uniform turned every tile into
dense rainbow speckle (each channel jittering independently), and after
an undo the same amount with **Monochromatic** ticked gave neutral grey
grain that kept every tile's hue intact — the two looks Photoshop's own
checkbox toggles between. The Gaussian option's exact behaviour is
pinned by its unit test rather than toggled live: it lives in a native
`<select>`, which the headless harness can't reliably drive.

**371 Rust tests total** (362 → 371, 364 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 28 — Image > Adjustments > Equalize (and its two selection variants)

Classic histogram equalisation: each channel's values are redistributed
so the darkest level present becomes 0, the brightest 255, and every
level in between lands where its cumulative share of the pixels puts
it. `Document::equalize(id, entire_image)` builds a 256-entry lookup
table per channel from a histogram of the sampled pixels —
`out(v) = round((cdf(v) − cdf_min) / (n − cdf_min) · 255)`, with
`cdf(v)` the count of sampled pixels at or below `v`, `cdf_min` the
count at the darkest populated level and `n` the sample count — and
then remaps the target pixels through it. A channel that holds a
single value everywhere (`cdf_min == n`) has nothing to spread and is
left unchanged rather than dividing by zero. R, G and B are equalised
independently, as Photoshop's own Equalize does; alpha is untouched.

With a selection active, Photoshop asks which of two things you meant,
and the `entire_image` flag is that question: `false` is "Equalize
selected area only" (histogram from the selected pixels, only they are
remapped), `true` is "Equalize entire image based on selected area"
(the same selection-built table applied to every pixel of the layer).
With no selection both are the plain menu command, so the three tracked
capabilities share one method and differ only in which pixels build
the histogram and which get remapped. The frontend adds an **Equalize**
button (selected-area-only when a selection exists, whole layer
otherwise) and an **Equalize from Sel.** button that is enabled only
while a selection exists.

**Verified two ways.** Seven new `document.rs` tests on 2×2 grey
layers small enough to run the CDF by hand: four distinct levels
(10, 20, 30, 40) have cdf 1, 2, 3, 4 with cdf_min 1, so they spread to
exactly 0, 85, 170, 255; repeated values (50, 50, 50, 200) give
cdf(50) = 3 = cdf_min and cdf(200) = 4, hence 0, 0, 0, 255; a
single-valued channel stays put. The two selection variants are pinned
on the same 10/20/30/40 layer with column 0 (values 10 and 30) selected:
"selected area only" yields 0, 20, 255, 40 — the unselected 20 and 40
untouched — while "entire image based on selection" yields 0, 0, 255,
255, because 20 sits above only one selected value (cdf 1 → 0) and 40
above both (cdf 2 → 255); both assert their dirty rect too. A further
test confirms the flag makes no difference without a selection, and
the usual locked/unknown-layer errors close the set — all passing on
first run. Live under Xvfb on the bundled gradient sample: **Equalize**
remapped the whole image dramatically (the blue/magenta/cyan/cream
gradient became green/red/yellow — the sample's narrow red channel
stretched to the full range while the wide blue channel barely moved,
which is exactly what per-channel equalisation predicts). After an
undo, a rectangle over the dark top-left tiles and **Equalize from
Sel.** applied that selection's table everywhere: inside it the dark
blues stretched up from black, and everything brighter outside
saturated to red/yellow, since every level above the selection's range
maps to 255 — the "based on selected area" semantics made visible.

**378 Rust tests total** (371 → 378, 371 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 29 — Filter > Other: Maximum, Minimum, High Pass, Offset

Photoshop's "Other" submenu is four unrelated utilities that share
nothing but a home, and they land here as four methods that reuse the
neighbourhood machinery the blur and noise filters already built.
**Maximum** and **Minimum** are the morphological dilate and erode:
every channel of every pixel becomes the largest (or smallest) value
found anywhere in the `(2·radius + 1)`-square window around it, the
window clamped at the layer edges the way `median_at` clamps. A shared
`extreme_at` helper walks that window once per pixel and keeps a
running max or min per channel, so `Document::maximum` and
`Document::minimum` are one `extreme_filter(id, radius, want_max)`
differing only in the comparison. Light regions spread into dark ones
under Maximum (a one-pixel white line becomes `2·radius + 1` wide);
dark regions spread into light ones under Minimum (that same line
vanishes once the radius exceeds half its width). Both honour the
selection and the layer lock, and both reject a zero radius — a
zero-radius max is the identity and Photoshop refuses it too.

**High Pass** keeps only what differs from the local average:
`out = original − box_blurred + 128` per colour channel, clamped to
0..=255. It reuses `box_blur_at`, so its "local average" is the same
flat square mean `Document::box_blur` uses rather than Photoshop's
Gaussian — the simplification `box_blur` itself already makes. A
region with no detail comes out a uniform mid-grey 128; only edges and
texture survive, which is why Photoshop's High Pass is the classic
first step of overlay-blend sharpening. Alpha is not a colour channel
and is left alone.

**Offset** shifts the whole layer by `dx` pixels right and `dy` down,
with everything that slides off one edge wrapping back in on the
opposite one — Photoshop's Wrap Around mode, the one that makes
seamless tiles (shift by half the canvas and the old outer edges meet
in the middle where the seam can be retouched). The source coordinate
is `(x − dx).rem_euclid(width)`, so negative and oversized amounts fold
correctly: `dx = −1` is `dx = width − 1`, and `dx = width` is a no-op.
Photoshop's other two fill modes for the vacated area (Repeat Edge
Pixels, Set to Transparent) and its confine-to-selection behaviour are
deliberate scope cuts, documented on the method: Offset here always
moves the entire layer and ignores the selection, the same
whole-layer stance `flip_layer_horizontal` takes. The frontend adds
**Maximum…**, **Minimum…**, **High Pass…** (radius sliders) and
**Offset…** (horizontal and vertical sliders spanning ±document
width/height) after Equalize.

**Verified two ways.** Seven new `document.rs` tests on the 3×3
red-ramp fixture (10, 20, 30 / 40, 50, 60 / 70, 80, 90), whose
neighbourhoods are small enough to list by hand. Maximum at radius 1:
the top-left corner's clamped window is {10, 10, 20, 10, 10, 20, 40,
40, 50} → 50, the centre sees the whole grid → 90, and the top-edge
pixel (1, 0) sees rows 0, 0, 1 × columns 0, 1, 2 → 60. Minimum on the
same layer: 10, 10, and the bottom-right window {50, 60, 60, 80, 90,
90, 80, 90, 90} → 50. A selection test confines each: Maximum with
only pixel (0, 0) selected changes it to 50 while its neighbour (1, 0)
stays 20; Minimum with only (2, 2) selected changes it to 50 while the
centre keeps its original 50. High Pass at radius 1 reuses the
box-blur test's already-verified local means (23, 50, 76) to expect
`10 − 23 + 128 = 115`, `50 − 50 + 128 = 128`, `90 − 76 + 128 = 142`,
with the flat green channel collapsing to 128 and alpha untouched at
255. Offset by (1, 0) rotates each row right — 10, 20, 30 → 30, 10, 20
— and by (0, 1) moves the bottom row to the top (70, 80, 90 above 10,
20, 30). A second Offset test pins the wrap arithmetic: shifting by
(3, −3) on a 3×3 layer is pixel-for-pixel identical to the original,
and `dx = −1` produces exactly the same pixels as `dx = 2` (20, 30,
10). Zero radii, locked layers and unknown ids all error without
touching the pixels. All passing on first run. Live under Xvfb on the
bundled gradient sample: **Maximum** at radius 6 dilated the thin
white grid lines into thick white bands, **Minimum** at radius 2 on
the original erased those same thin lines entirely (erosion by a
window wider than the line), **High Pass** at radius 3 flattened every
smooth gradient tile to neutral grey while the grid-line edges survived
as coloured fringes, and **Offset** by 206 px horizontally produced the
expected seam with the right-hand third of the image wrapped round to
the left edge. Undo restored the original after each.

**385 Rust tests total** (378 → 385, 378 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 30 — Filter > Other > Custom

The last item in Photoshop's Other submenu is the one that generalises
half the others: a user-supplied 5×5 convolution kernel. Each colour
channel of each pixel becomes `(Σ kernel[i] · sample[i]) / scale +
offset`, clamped to 0..=255, where the 25 coefficients are laid out row
by row over the neighbourhood centred on the pixel (`kernel[12]` is the
pixel itself, `kernel[0]` the sample two up and two left) and samples
past the layer edge clamp to the nearest edge pixel like every other
window filter here. `Document::custom(id, kernel, scale, offset)` does
that through a `convolve_at` helper that skips zero-weight cells,
accumulates in `i64`, and divides with integer division truncating
toward zero — the arithmetic a person can redo on paper. Alpha is
carried over unchanged, since Custom is a colour filter, and it honours
the selection and the layer lock. Photoshop's ranges are kept: −999..999
per coefficient, 1..9999 for Scale, −9999..9999 for Offset; a Scale of 0
is rejected rather than dividing by it. Every classic kernel is a
setting of this one dialog — the identity (a lone 1), a box blur (nine
1s over 9), the textbook sharpen (5 in the middle, −1 on each side), an
emboss (−1 and +1 on a diagonal with an Offset of 128) — which makes it
the stepping stone to the Stylize filters. Loading and saving kernels to
Photoshop's `.acf` files is a deliberate scope cut. The frontend adds a
**Custom…** button opening a 5×5 grid of number fields plus Scale and
Offset, with a Reset back to the identity.

Two small frontend bugs surfaced while typing a kernel in and are fixed
here because Custom is the most typing-heavy dialog in the app. First,
the global shortcut handler now ignores key presses whose target is a
text-like input (`text`, `number`, `search`, `email`, `url`, `password`)
or a textarea — before, Ctrl+A inside any number field ran **Select
All** on the canvas instead of selecting the field's text, and Ctrl+C /
Ctrl+V / Ctrl+Z were likewise hijacked; sliders and the colour picker
keep their shortcuts. Second, the kernel, Scale and Offset fields are
held as strings and parsed on Apply: a controlled numeric `value` snaps
the invalid intermediate `"-"` back to `0` the instant it is typed, so
a negative coefficient could never be entered — and a kernel without
negatives can't sharpen, emboss or find an edge.

**Verified two ways.** Eight new `document.rs` tests on the 3×3 red-ramp
fixture (10, 20, 30 / 40, 50, 60 / 70, 80, 90), every neighbourhood
small enough to write out. The identity kernel returns the layer
untouched with the full-canvas dirty rect. Identity plus Offset 5 lifts
every colour channel by 5 (10 → 15, 50 → 55, 90 → 95; the flat green
channel 0 → 5) while alpha stays 255, and Offset −20 clamps the top row
to 0, 0, 10. Nine 1s in the middle of the grid over Scale 9 reproduce
the box-blur test's own answers — 450/9 = 50 at the centre, 210/9 = 23
at the clamped corner, 690/9 = 76 opposite. The textbook sharpen gives
5·50 − (20+40+60+80) = 50 at the centre, 5·10 − (10+10+20+40) = −30 → 0
at the corner whose up and left samples clamp onto itself, and 5·90 −
(60+80+90+90) = 130 at the far corner. Scale 4 divides toward zero
(10 → 2, 50 → 12, 90 → 22); a −1 centre with Offset 100 inverts (10 →
90, 50 → 50, 90 → 10, green → 100). The far corner cell `kernel[24]`
proves the 5×5 reach — the top-left pixel reads the bottom-right's 90,
and pixels nearer the edge clamp onto it — while `kernel[14]` (+2, 0)
copies the right column onto the left (30, 60, 90). With only the
centre pixel selected, identity plus Offset 100 changes it to 150,
leaves the corners at 10 and 90, and reports the 1×1 dirty rect. Zero
Scale, a locked layer and an unknown id all error without touching a
pixel. All passing on first run. Live under Xvfb on the bundled
gradient sample: typing an emboss into the real grid — −1 at row 2
column 2, 0 in the centre, +1 at row 4 column 4, Offset 128 — turned
every smooth tile flat mid-grey and every white grid line into a
light/dark relief pair, exactly the classic emboss; Undo restored the
original. The first attempt at that typing is what exposed the two
shortcut and negative-sign bugs above; after the fixes, Ctrl+A stayed
inside the field and `-1` was accepted as typed.

**393 Rust tests total** (385 → 393, 386 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 31 — Filter > Stylize: Find Edges, Solarize, Emboss, Trace Contour

Four of Photoshop's Stylize filters, each a few lines once the window
machinery from the last phases exists. They share a new private
`filter_pixels(id, pick)` skeleton — snapshot the layer, run `pick` on
every selected pixel against that untouched snapshot, return the dirty
rect, error on a locked or unknown layer — so a filter never reads its
own output and none of the four repeats the selection loop.

**Find Edges** inverts a Sobel edge magnitude: a new `sobel_at` helper
weights the 3×3 neighbourhood by `[−1 0 1; −2 0 2; −1 0 1]` for `Gx`
and its transpose for `Gy`, and each colour channel becomes
`255 − min(255, |Gx| + |Gy|)`. Flat areas come out white, edges dark in
whichever channel changed. The L1 sum keeps the arithmetic in integers a
person can check; there are no parameters, as in Photoshop. **Solarize**
is the tent curve `min(v, 255 − v)` per channel — the lower half of the
range is untouched and the upper half folded back down, so the whole
result lands in 0..=127, which is why the classic recipe follows it with
Auto Levels. **Emboss** takes Photoshop's angle, height and amount:
with `angle` in degrees (0° from the right, anticlockwise like the
Motion Blur dial, so the default 135° lights from the upper left), each
channel becomes `128 + (away − toward) · amount / 100`, where `toward`
is the sample `height` pixels from the pixel in the light's direction
and `away` the sample the same distance the other way, both edge-clamped
and nearest-neighbour like Motion Blur. A surface whose bright side
faces the light reads light and its far side dark, the raised look;
flat areas come out mid-grey. **Trace Contour** takes Photoshop's level
and Lower/Upper edge: for each channel, a pixel is marked when it sits
on the chosen side of `level` (below for Lower, at-or-above for Upper)
and one of its four neighbours sits on the other; marked channels go to
0 and the rest to 255, so a contour in one channel draws in that
channel's complement on white and a contour in all three draws black.
Neighbours past the edge clamp onto the pixel itself, so the border is
never a crossing. All four leave alpha alone and honour the selection.
The frontend adds one-click **Find Edges** and **Solarize** buttons and
**Emboss…** (angle, height, amount sliders) and **Trace Contour…**
(level slider, Upper edge checkbox) dialogs.

Auditing this batch also showed the parity list had never tracked
Emboss, Find Edges, Diffuse or Extrude — the four classic Stylize
entries — so they are added (Diffuse and Extrude unchecked), taking the
catalogue from 593 to 597.

**Verified two ways.** Seven new `document.rs` tests. Solarize on greys
10, 128, 200, 255 gives 10, 127, 55, 0 with alpha untouched. Find Edges
on the 3×3 red ramp (10..90 by tens): the centre's `Gx = (30 + 120 +
90) − (10 + 80 + 70) = 80` and `Gy = (70 + 160 + 90) − (10 + 40 + 30) =
240` sum past 255 and invert to 0, while the top-left corner with every
missing sample clamped gives `Gx = 40`, `Gy = 120`, 160 → 95; the flat
green channel is 255 everywhere, and a solid grey layer comes out pure
white. Emboss at angle 0, height 1, amount 100 gives the centre
`128 + 40 − 60 = 108` and both clamped corners 118; angle 180 mirrors
it to 148; angle 90 (light from above) gives `128 + 80 − 20 = 188`;
amount 200 and 50 scale the same −20 relief to 88 and 118; height 2
reaches the clamped edges (108 at the centre and at the left edge).
Trace Contour at level 50, Lower, marks exactly the 20, 30 and 40 —
each touches a 50 or 60 — and not the 10, whose neighbours are 20 and
40, giving reds 255, 0, 0, 0, 255, 255, 255, 255, 255; Upper at the same
level marks the other side of the contour, the 50, 60 and 70; level 0
Lower and level 255 Upper draw nothing. Emboss confined to the centre
pixel changes only it and reports the 1×1 dirty rect, and zero height,
zero amount, a NaN angle, locked layers and unknown ids all error. All
passing on first run. Live under Xvfb on the bundled gradient sample:
**Find Edges** turned the smooth tiles white with dark grid lines that
went red and green near the saturated edges where only one channel
changes; **Solarize** turned the white grid lines black and folded the
ramps so they peak mid-image; **Emboss** at 135°/3 px/100% flattened
the tiles to mid-grey with a light upper-left / dark lower-right relief
on every line; **Trace Contour** at level 128, Lower, left the canvas
white with blue contours in the dark-blue region (red and green both
marked), magenta at the top right (only green), cyan at the bottom left
(only red), nothing in the cream corner where every channel is already
above 128, plus the horizontal contour where green crosses the level.
Undo restored the original after each.

**400 Rust tests total** (393 → 400, 393 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 32 — Filter > Blur > Gaussian Blur

The workhorse of Photoshop's Blur menu. `Document::gaussian_blur(id,
radius)` treats `radius` as the standard deviation in pixels and
weights each sample by a bell curve rather than the flat mean Box Blur
uses. The kernel comes from a new `binomial_weights(sigma)` helper: the
normalised binomial that the textbooks use as the discrete Gaussian —
Pascal's triangle row `2n` with `n = 2·sigma²`, whose variance is
exactly `n/2 = sigma²` — cut off at `3·sigma` taps a side (the tails
beyond hold well under 0.3 % of the weight) and renormalised. It is
built outward from the centre by the ratio `C(2n, n+k+1) / C(2n, n+k)
= (n − k) / (n + k + 1)`, so nothing overflows however large the radius
and tails that underflow to zero simply drop out; `sigma = 1` gives
exactly `[1 4 6 4 1] / 16`. The blur is separable and runs as two
passes: every row of the *whole* layer is blurred horizontally into a
scratch buffer — the whole layer, not just the selection, because the
second pass reads rows above and below the selected pixels — and then
each selected pixel is blurred vertically from that buffer through the
`filter_pixels` skeleton. Each pass rounds to the nearest whole value
and clamps its samples to the layer's edges like Box Blur, and R, G, B
and A are blurred independently and un-premultiplied, the same scope
cut Box Blur makes. Photoshop allows radii from 0.1 to 250 px; this
takes whole pixels and the dialog offers 1–25. The frontend adds a
**Gaussian Blur…** button beside Box Blur with a radius slider.

**Verified two ways.** Four new `document.rs` tests. The weights for
radius 1 are `1/16, 4/16, 6/16, 4/16, 1/16` to within 1e-12; radius 2
is Pascal's row 16 cut to ±6 and renormalised, so its centre and
next-to-centre weights are `12870 / 65502` and `11440 / 65502` (65536
minus the two dropped 1s and 16s) and the thirteen sum to 1; radius 25
stays finite. On the 3×3 red ramp (10..90 by tens) radius 1 is the
`[1 4 6 4 1] / 16` kernel applied twice with edge clamping, worked
entirely by hand: the horizontal pass gives 14, 20, 26 / 44, 50, 56 /
74, 80, 86 (the top-left is `(10 + 40 + 60 + 80 + 30) / 16 = 13.75`,
rounded), and the vertical pass over those columns gives the final
25, 31, 37 / 44, 50, 56 / 63, 69, 75 (top-left `(14 + 56 + 84 + 176 +
74) / 16 = 25.25`; the centre stays 50 because the ramp is symmetric
around it) — and, being a gentler kernel than the box, the corner
lands at 25 where Box Blur's flat mean gave 23. A flat grey layer is
returned unchanged at radius 3; with only the top-left pixel selected
it alone becomes 25 (blurred with its unselected neighbours) while its
neighbour keeps 20 and the dirty rect is that one pixel; zero radius,
locked layers and unknown ids error without touching pixels. All
passing on first run. Live under Xvfb on the bundled gradient sample,
radius 6 turned the crisp one-pixel grid lines into wide, soft,
bell-shaped bands with no hard edges — the signature that separates a
Gaussian from a box blur — while the already-smooth gradient was
visibly unchanged. Undo restored the original.

**404 Rust tests total** (400 → 404, 397 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 33 — Filter > Stylize > Diffuse

Diffuse shuffles each pixel with one of its eight neighbours, so hard
edges dissolve into a grainy, out-of-focus texture without any
averaging. `Document::diffuse(id, mode, seed)` walks the selected
pixels in scan order and, for each, takes two draws from the seeded
`XorShift32` generator Add Noise already uses, mapping each through
`draw % 3 − 1` to a horizontal and a vertical offset in −1..=1 (clamped
to the layer). What happens next is the mode, a `DiffuseMode` enum
mirroring Photoshop's four radio buttons: **Normal** takes that
neighbour's colour unconditionally; **Darken Only** takes it only when
it is darker (a smaller R+G+B); **Lighten Only** only when it is
lighter. **Anisotropic** uses no randomness at all — the pixel takes
whichever in-bounds neighbour is closest in colour (the smallest summed
R, G, B difference, the first in scan order on a tie), which shuffles
along edges rather than across them. Whole pixels move, alpha
included, so a copied neighbour keeps its own transparency. The result
is deterministic for a given seed and selection, and the frontend sends
a fresh seed on every apply, as Add Noise does, so re-applying gives a
different shuffle. The dialog uses four radio buttons rather than a
native `<select>`, so it can be driven headlessly like every other
control here.

**Verified two ways.** Five new `document.rs` tests on the 3×3 red ramp
(10..90 by tens). The seed-1 draw sequence is the one the Add Noise
tests already pin — 270369, 67634689, 2647435461, … — and mapped two
per pixel through `% 3 − 1` it gives the offsets (−1, 0), (−1, +1),
(+1, 0) / (0, −1), (+1, 0), (−1, +1) / (0, +1), (+1, −1), (0, −1), all
cross-checked against a scripted xorshift. Normal therefore produces
reds 10, 40, 30 / 10, 60, 80 / 70, 60, 60 — the corner clamps onto
itself, the centre takes its right-hand 60, the bottom row reads a
clamped 70 then 60, 60. Darken Only on the same draws keeps every
lighter neighbour out (10, 20, 30 / 10, 50, 60 / 70, 60, 60) and
Lighten Only every darker one (10, 40, 30 / 40, 60, 80 / 70, 80, 90).
Anisotropic is worked purely by hand: each pixel takes its
nearest-valued in-bounds neighbour, so the corner 10 (neighbours 20,
40, 50) becomes 20 and the centre, whose 40 and 60 both differ by 10,
takes the first, giving 20, 10, 20 / 50, 40, 50 / 80, 70, 80 — and the
seed is shown to play no part. Two documents diffused with the same
seed are identical; with only pixel (1, 0) selected it receives the
*first* draw pair and takes its left neighbour's 10 while everything
else stays put and the dirty rect is that one pixel; a locked layer and
an unknown id error without touching pixels. All passing on first run.
Live under Xvfb on the bundled gradient sample: **Normal** turned the
crisp one-pixel grid lines into jittery, broken, one-pixel-scattered
edges — the classic dissolved look — and after an undo **Lighten Only**
left every line continuous and only spread its white outward into
ragged neighbours, never breaking it, exactly the one-directional
rule. Undo restored the original after each.

**409 Rust tests total** (404 → 409, 402 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 34 — Filter > Blur > Surface Blur

The edge-preserving blur: it smooths flat and gently varying areas
while leaving real edges untouched, which is what makes it the usual
tool for skin and noise. `Document::surface_blur(id, radius,
threshold)` makes each colour channel a weighted mean of the
`(2·radius+1)`-square, edge-clamped window in which a neighbour's
weight is `threshold − |neighbour − centre|` when that is positive and
zero otherwise. Samples within `threshold` of the pixel's own value
count in proportion to how close they are; anything further away — the
far side of an edge — is ignored entirely, so an edge never bleeds
into the pixels beside it. The pixel itself always carries weight
`threshold`, so the weights never sum to zero, and the mean is rounded
to the nearest whole value with integer arithmetic. Photoshop's Surface
Blur has the same two controls (Radius 1–100, Threshold 2–255); here
the dialog offers Radius 1–16 and Threshold 1–255, and a threshold of 1
admits only exact matches, so it changes nothing. Alpha is untouched,
the selection is honoured, and a zero radius or threshold is rejected.
The frontend adds a **Surface Blur…** button after Gaussian Blur with
the two sliders.

**Verified two ways.** Four new `document.rs` tests on the 3×3 red ramp
(10..90 by tens) at radius 1, every weight written out by hand. At
threshold 25 the centre 50 admits only 40, 50 and 60 (weights 15, 25,
15), so `(15·40 + 25·50 + 15·60) / 55 = 50`; the top-left corner 10,
whose clamped window holds four 10s (weight 25 each), two 20s (weight
15) and a 40 and a 50 that fall outside, gives `1600 / 130 = 12.3 →
12` — far less pull than the box blur's 23 on the same window, which is
the whole point of the filter; the 20 beside it gives `(2·15·10 +
2·25·20 + 2·15·30 + 5·40) / 115 = 20.9 → 21`. The flat green channel
stays 0 and alpha stays 255. At threshold 255 every sample is admitted
with weight `255 − |difference|` and the centre, symmetric in its
window, still comes out 50 (`104750 / 2095`); at threshold 1 the layer
is returned byte-for-byte unchanged. A flat grey layer is unchanged at
radius 2, threshold 40; with only the top-left pixel selected it alone
becomes 12 while its neighbour keeps 20 and the dirty rect is that
pixel; zero radius, zero threshold, a locked layer and an unknown id
all error without touching pixels. All passing on first run. Live
under Xvfb on the bundled gradient sample at the default radius 5,
threshold 15: the smooth gradient was smoothed and the one-pixel white
grid lines stayed perfectly crisp with no halo — a 4× zoom on the same
grid intersection that Gaussian Blur had turned into wide soft bands
showed sharp single-pixel edges. Undo restored the original.

**413 Rust tests total** (409 → 413, 406 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 35 — Filter > Stylize > Glowing Edges

Find Edges' neon cousin: the same edges, drawn bright on black instead
of dark on white, then widened, brightened and softened by Photoshop's
three controls. `Document::glowing_edges(id, edge_width,
edge_brightness, smoothness)` runs a four-stage pipeline over the whole
layer into scratch buffers, so every stage sees its neighbours: (1) the
`sobel_at` edge magnitude per colour channel — the buffer Find Edges
inverts, used here as-is; (2) a maximum filter of radius `edge_width −
1`, the same `extreme_at` that Maximum uses, so a one-pixel edge
becomes `2·edge_width − 1` pixels wide (width 1 is no dilation); (3)
each value scaled by `edge_brightness / 5`, truncated and clamped, so
brightness 5 is the raw magnitude, 0 is black and Photoshop's default 6
lifts it by a fifth; (4) a box blur of radius `smoothness − 1`, the
same `box_blur_at` Box Blur uses, with smoothness 1 meaning none. Only
the selected pixels are written, from the final buffer, and alpha is
untouched. Photoshop's ranges are kept — Edge Width 1–14, Edge
Brightness 0–20, Smoothness 1–15 — and a zero width or smoothness is
rejected. The frontend adds a **Glowing Edges…** button with the three
sliders, defaulting to Photoshop's 2 / 6 / 5.

**Verified two ways.** Four new `document.rs` tests on the 3×3 red ramp
(10..90 by tens), building on the Sobel values the Find Edges test
already derived by hand. With width 1, brightness 5 and smoothness 1
the result *is* the Sobel L1 magnitude: 160 in the corners, 200
mid-top and mid-bottom, a clamped 255 across the middle row (the
mid-top pixel, for instance, has `Gx = (30 + 60 + 60) − (10 + 20 + 40)
= 80` and `Gy = (40 + 100 + 60) − (10 + 40 + 30) = 120`); the flat
green channel is black and alpha stays 255. Brightness 6 scales those
to 192, 240 and a clamped 255; brightness 3 to 96, 120, 153; brightness
0 to black. Width 2 is a radius-1 maximum and, since every 3×3 window
on this layer contains a 255, turns the whole layer white; smoothness
2 is a radius-1 box blur and, since every clamped window holds four
160s, two 200s and three 255s, turns every pixel into `1805 / 9 = 200`.
A flat grey layer comes out black at 2 / 6 / 3; with only the top-left
pixel selected it alone becomes 160 while its neighbour keeps 20 and
the dirty rect is that pixel; zero width, zero smoothness, a locked
layer and an unknown id all error. All passing on first run. Live under
Xvfb on the bundled gradient sample at the defaults, the canvas went
black and the grid became wide, soft, luminous lines — the neon look —
picking up colour near the saturated edges where only one channel has
an edge. Undo restored the original.

**417 Rust tests total** (413 → 417, 410 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 36 — Filter > Pixelate: Mosaic and Fragment

Auditing the filter menus for this phase showed the parity list had
never tracked three whole Photoshop submenus — Pixelate (Color
Halftone, Crystallize, Facet, Fragment, Mezzotint, Mosaic, Pointillize),
Distort (Displace, Pinch, Polar Coordinates, Ripple, Shear, Spherize,
Twirl, Wave, ZigZag) and Render (Clouds, Difference Clouds, Fibers, Lens
Flare, Lighting Effects). All twenty-one are now tracked, taking the
catalogue from 597 to 618, and the two exact-arithmetic Pixelate
filters ship here.

**Mosaic** cuts the layer into a grid of `cell_size`-pixel squares
anchored at the top-left corner and gives every pixel the mean colour
of its square. `Document::mosaic(id, cell_size)` computes each cell's
mean once from a snapshot of the whole layer — unselected pixels in a
cell still contribute, as in Photoshop — and then writes only the
selected pixels through the `filter_pixels` skeleton, so the cost is
one pass over the layer regardless of cell size. Cells that run off the
right or bottom edge average only the pixels they actually contain. All
four channels average independently with truncating integer division,
like Box Blur. A cell size of 1 is the identity and 0 is rejected.
**Fragment** takes no parameters, as in Photoshop: four copies of the
layer offset four pixels diagonally — up-left, up-right, down-left,
down-right — are averaged, through the same `average_samples` Box Blur
and Motion Blur use, with samples past the edge clamped. The frontend
adds a **Mosaic…** dialog (Cell Size 2–64) and a one-click **Fragment**
button.

**Verified two ways.** Four new `document.rs` tests. Mosaic at cell size
2 on the 3×3 red ramp (10..90 by tens) gives the top-left 2×2 {10, 20,
40, 50} → 30, the one-column strip beside it {30, 60} → 45, the one-row
strip below {70, 80} → 75 and the lone corner 90 → 90 — reds 30, 30,
45 / 30, 30, 45 / 75, 75, 90 — with green still 0 and alpha 255; cell
size 3 is one cell over the whole layer, 450 / 9 = 50 everywhere; cell
size 1 returns the layer byte-for-byte; with only the top-left pixel
selected it becomes 30 (its cell's mean still counts the unselected 20,
40 and 50) while its neighbour keeps 20 and the dirty rect is that one
pixel. Fragment is checked on a 9×9 layer whose red is `10·x + y`, so
every diagonal sample has a distinct value: the centre (4, 4) reads the
four corners 0, 80, 8, 88 → 176 / 4 = 44, its own value, because the
ramp is linear; (1, 1) reads 0, 50, 5, 55 → 27; (8, 8) reads 44 and
three clamped samples 84, 48, 88 → 66; (0, 0) reads 0, 40, 4, 44 → 22;
alpha stays 255 and a flat grey layer is unchanged. Zero cell size,
locked layers and unknown ids error for both. All passing on first run.
Live under Xvfb on the bundled gradient sample: Mosaic at 20 px
collapsed the gradient into flat 20-pixel blocks and averaged the thin
white grid lines into slightly lighter cells; after an undo, Fragment
turned every grid line into a pair of half-intensity lines eight pixels
apart — the ±4 diagonal copies — the "out of register" look. Undo
restored the original after each.

**421 Rust tests total** (417 → 421, 414 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 37 — Filter > Distort: Ripple and Twirl

The first two Distort filters, and with them the resampling primitive
the rest of that submenu will share: `sample_nearest(source, (sx, sy))`
returns the pixel nearest a continuous position, each coordinate
rounded to the nearest whole pixel and clamped to the layer so
positions off the edge repeat the edge pixel — Photoshop's "Repeat Edge
Pixels". Nearest-neighbour rather than bilinear is the same hard-edged
scope cut Motion Blur makes. Every Distort filter is then just a
formula for *where each output pixel pulls from*, run through the
`filter_pixels` skeleton, so whole pixels move, alpha included.

**Ripple** pulls each pixel from a sinusoidally displaced position:
`amplitude · sin(2π·y / wavelength)` horizontally and `amplitude ·
sin(2π·x / wavelength)` vertically, both in pixels, so straight lines
wobble like a reflection on water. Photoshop's dialog has a percentage
Amount and a Small / Medium / Large size; the frontend maps those to a
wavelength of 8, 16 or 32 px and an amplitude of `amount% ×
wavelength / 8`, so 100 % on Small is a one-pixel ripple and each size
keeps Photoshop's proportions. A zero amplitude is the identity; a zero
wavelength or a non-finite amplitude is rejected. **Twirl** rotates the
layer about its centre by an angle that falls off with distance —
`angle · (1 − r/R)²` degrees, with `r` the pixel's distance from the
centre and `R` half the shorter side — so the middle spins hard and
everything at or beyond `R` stays put: the classic whirlpool. Positive
angles turn the content clockwise on screen, as on Photoshop's dial;
each pixel pulls from the position that rotates onto it. Angle 0 is the
identity and a non-finite angle is rejected. The frontend adds
**Ripple…** (Amount −999..999 %, size radios) and **Twirl…** (Angle
−999..999°) dialogs.

**Verified two ways.** Five new `document.rs` tests on a new
`ramp_square(n)` fixture whose red is `10·x + y`, so every sample
position has a distinct, readable value; every expectation was worked
by hand and cross-checked with a scripted evaluation of the same
formulas. Ripple at wavelength 4 makes `sin(2πt/4)` run 0, 1, 0, −1
over t = 0..4, so with amplitude 1 on a 4×4 layer each pixel reads
`(x + s[y], y + s[x])`: (1, 1) reads (2, 2) → 22, (0, 1) reads (1, 1) →
11, (3, 3) reads (2, 2) → 22, (2, 2) is untouched because `s[2] = 0`,
and the edge cases clamp — (1, 3) reads (0, 4) → (0, 3) → 3, (3, 1)
reads (4, 0) → (3, 0) → 30; amplitude 2 reaches two pixels (33 and 21);
amplitude 0 returns the layer byte-for-byte; with one pixel selected
only it moves and the dirty rect is that pixel. Twirl on a 5×5 layer
has `R = 2.5`, so the four pixels one step from the centre have falloff
`(1 − 1/2.5)² = 0.36` and 250° becomes exactly 90°: each reads the
pixel a quarter-turn anticlockwise from it — (3, 2) → 21, (2, 3) → 32,
(1, 2) → 23, (2, 1) → 12 — turning the content clockwise; the centre
keeps 22; (3, 3) at r = √2 turns ≈ 47°, its offset (1, 1) landing on
(1.41, −0.05) → (1, 0), so it reads 32; two steps out the falloff is
0.04 → 10° and (2, 0) rounds back to itself (42); the corners lie
beyond `R` and keep 0 and 44. Angle −250 sends (3, 2) to 23 and (2, 1)
to 32 instead, and angle 0 is the identity. Zero wavelength, NaN
amplitude, infinite angle, locked layers and unknown ids all error. All
passing on first run. Live under Xvfb on the bundled gradient sample:
Ripple at 302 %, Large turned every straight grid line — and the
layer's own border — into a clean sine wave of 32-px wavelength and
about 12-px amplitude; after an undo, Twirl at 422° spiralled the grid
into a whirlpool around the centre while the edges beyond `R` kept
their straight lines. Undo restored the original after each.

**426 Rust tests total** (421 → 426, 419 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 38 — Filter > Distort: Pinch and Spherize

Two more Distort filters that are one mechanism. Both pull every pixel
inside the ellipse inscribed in the layer from a position at the same
angle but a different distance from the centre: a private
`radial_remap(id, strength)` measures each pixel's normalised radius
`ρ` — its distance from the centre in units of the half-width
horizontally and the half-height vertically, so the effect fills the
inscribed ellipse as Photoshop's does — and samples from `ρ · (1 −
strength · (1 − ρ))` instead, through the nearest-neighbour
`sample_nearest` from Phase 37. A positive strength pulls from nearer
the centre and so magnifies it (a bulge); negative pulls from further
out and shrinks it (a pinch); 0 is the identity; the rim `ρ = 1` always
maps to itself, so the edge of the effect is seamless, and pixels
beyond the ellipse are untouched. **Spherize** is `strength = 0.75 ·
amount / 100` and **Pinch** is its exact mirror, `−0.75 · amount /
100`, both over Photoshop's −100..=100 %. The 0.75 cap is deliberate:
it keeps the mapping strictly increasing (its slope at the centre is
`1 − strength`, never zero), so at +100 % Spherize magnifies the middle
4× like a lens rather than collapsing it. The first draft used
`ρ^exponent` instead, and the live run showed why that is wrong — with
`ρ²` the magnification at the centre is unbounded and the central grid
intersection of the sample blew up into a white blob, which
Photoshop's lens never does — so the formula was replaced, the hand
values re-derived and the live pass repeated before anything was
committed. Photoshop's Horizontal Only and Vertical Only Spherize modes
are a documented scope cut. The frontend adds **Pinch…** and
**Spherize…** dialogs, each an Amount slider from −100 to 100.

**Verified two ways.** Three new `document.rs` tests on the 9×9
`ramp_square` fixture (red `10·x + y`), where the centre is (4, 4), the
half-axes are 4.5 and the pixels along the middle row sit at `ρ = 2/9,
4/9, 6/9, 8/9`. Spherize +100 % scales each offset by `0.25 + 0.75ρ`:
(5, 4) reads `4 + 5/12 = 4.42` → (4, 4) = 44, (6, 4) reads `4 + 2 ·
7/12 = 5.17` → 54, (7, 4) reads `4 + 3 · 0.75 = 6.25` → 64 and (8, 4)
reads `4 + 4 · 11/12 = 7.67` → 84 — the middle stretched outward —
while the centre and the corners (ρ > 1) keep 44 and 0 and alpha stays
255. Pinch +100 % scales by `1.75 − 0.75ρ` instead: (5, 4) reads `4 +
19/12 = 5.58` → 64, (6, 4) reads 6.83 → 74, (7, 4) reads 7.75 → 84 and
(8, 4) reads 8.33, clamped to the edge → 84. Every position was worked
by hand as a fraction and cross-checked with a scripted evaluation of
the same formula. Pinch at 60 % is byte-identical to Spherize at −60 %;
both at 0 return the layer unchanged; with only (5, 4) selected it
alone changes and the dirty rect is that pixel; NaN and infinite
amounts, locked layers and unknown ids all error. All passing on first
run. Live under Xvfb on the bundled gradient sample: Pinch at 100 %
drew the grid inward toward the centre, lines converging like a
squeezed cloth; after an undo, Spherize at 100 % bowed the grid
outward with the centre magnified like a lens and no collapse at the
middle. Undo restored the original after each.

**429 Rust tests total** (426 → 429, 422 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 39 — Filter > Distort: ZigZag and Polar Coordinates

The last two Distort filters. **ZigZag** sends concentric ripples out
from the centre, like a stone dropped in a pond: each pixel's
normalised radius `ρ = r / R` (`R` the distance from the centre to the
nearest edge) becomes a displacement `d = A · sin(π · ridges · ρ)`
pixels, with amplitude `A = amount / 100 · R / ridges`, so `ridges`
counts the half-waves between the centre and the rim and the pattern
keeps its proportions at any canvas size. What `d` does is Photoshop's
`style` radio group: **Out From Center** moves the sample along the
radius to `r + d`; **Around Center** rotates it about the centre by
`d · π / R` (a displacement of `R` pixels is a half turn); **Pond
Ripples** shifts it by `d` in both x and y, the diagonal motion
Photoshop itself describes as "toward the upper left or lower right".
**Polar Coordinates** has two directions. Rectangular to Polar wraps
the layer into rings: each output pixel's angle clockwise from twelve
o'clock, as a fraction of a full turn, picks the source column, and its
normalised radius — distance from the centre in units of the
half-width and half-height, so the rim is the inscribed ellipse — picks
the source row, top row at the centre and bottom row on the rim,
which is why the centre pixel itself reads the top-left corner. Polar
to Rectangular is the inverse reading: column `x` is the angle `x /
width` of a turn and row `y` the radius `y / (height − 1)`, unrolling a
ring into a row. Both filters use the same nearest-neighbour
`sample_nearest` the whole Distort submenu shares, act on the whole
layer (not just an inscribed shape, for ZigZag), and move whole pixels
including alpha. The frontend adds **ZigZag…** (Amount −100..100 %,
Ridges 1..20, style radios) and **Polar Coordinates…** (a two-way radio
choice) dialogs.

**Verified two ways.** Four new `document.rs` tests on the 9×9
`ramp_square` fixture (red `10·x + y`), every position worked by hand
and cross-checked with a scripted evaluation of the same formulas. With
`R = 4` and 2 ridges the displacement amplitude is `R / ridges = 2 px`.
Out From Center at 100 %: one step out (`ρ = 0.25`) the sine is 1, so
(5, 4) reads `r = 3` → (7, 4) = 74 and (4, 5) reads (4, 7) = 47; three
steps out (`ρ = 0.75`) it is −1, so (7, 4) reads `r = 1` → 54; at two
and four steps the sine is 0 and nothing moves, nor does the centre; a
diagonal pixel at `r = √2` reads `r = 3.0` → 66; a negative amount
sends (5, 4) clean through the centre to (3, 4) = 34. Around Center
turns the same 2 px into a `π/2` rotation: (5, 4) reads a quarter turn
on, (4, 5) = 45; (7, 4)'s −2 px is a quarter turn back, (4, 1) = 41.
Pond Ripples shifts diagonally: (5, 4) reads (7, 6) = 76, (7, 4) reads
(5, 2) = 52, and (6, 4), where the sine is 0, stays 64. For Polar
Coordinates, Rectangular to Polar sends the top-centre pixel (angle 0,
`ρ = 8/9`) to (0, 7.11) → 7, three o'clock (a quarter turn) to (2, 7) =
27, nine o'clock (three quarters) to (7, 7) = 77, and a 45° point to
(1.13, 5.03) → 15; Polar to Rectangular inverts the same map, sending
(0, 4) — straight up at half radius — to (4, 1.75) → 42 and (6, 4) —
240° — to (2.05, 5.13) → 25. Amount 0 is the identity for ZigZag; a
one-pixel selection confines each filter to a 1×1 dirty rect; zero
ridges, non-finite amounts, locked layers and unknown ids all error.
All passing on first run. Live under Xvfb on the bundled gradient
sample: ZigZag Out From Center at 53 % turned every straight grid line
into a smooth ripple radiating from the canvas centre; after an undo,
Rectangular to Polar wrapped the whole grid into concentric rings
crossed by radial spokes, exactly the unrolled-cylinder mapping the
formula predicts. Undo restored the original after each.

**433 Rust tests total** (429 → 433, 426 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 40 — Filter > Pixelate > Color Halftone

Auditing the earlier Pixelate batch left this one unshipped: the filter
that reduces a photo to a grid of solid-colour circular dots, echoing a
colour newspaper print. `Document::color_halftone(id, max_radius)` gives
each colour channel its own square screen of `2 · max_radius`-pixel
cells — but instead of Photoshop's four *rotated* screens (one angle per
channel), the three channels here get three *offset* screens: R at
`(0, 0)`, G at `(max_radius, 0)`, B at `(0, max_radius)`. A rotated grid
would need anti-aliased circles to look right at the radii this dialog
allows, and this project has consistently favoured exact, hand-checkable
integer arithmetic over that (the same trade-off Motion Blur, Ripple and
Twirl already made with nearest-neighbour sampling) — offsetting the
grids instead still keeps the three screens from stacking exactly, which
is all the rotation is really for. For each cell, the channel's
*average* value over every pixel in that cell becomes a dot centred on
the cell, with the dot's area proportional to that average — a circle's
area grows with the square of its radius, so "area ∝ average" becomes
the single integer inequality `(dx² + dy²) · 255 ≤ max_radius² ·
average`, with no square root anywhere. A pixel inside its channel's dot
for that cell becomes that channel at full value (255); outside, 0 — so
every output pixel is one of eight colours (black, the three primaries,
the three secondaries, white), the blocky "overlapping ink dots" look of
the real filter. Alpha is untouched, the selection is honoured, and a
zero radius errors. The frontend adds a **Color Halftone…** button with
a Max Radius slider (1–64, Photoshop's own dialog runs 4–127).

**Verified two ways.** Four new `document.rs` tests. On a solid white 4×4
layer at radius 2 (one cell per channel, spanning the whole canvas, so
every average is 255 and the inequality reduces to `dx² + dy² ≤ 4`): R's
single cell is centred at (2, 2); G's is shifted to two half-canvas
cells centred at x = 0 and x = 4; B's the same shift on y. Because those
three centres differ, a perfectly flat white input still splits into
four distinct colours — (0, 0) comes out cyan (R's dot excludes it, G's
and B's both include it), (1, 0) blue, (0, 1) green, and (2, 2) and
(3, 3) both land in every dot and stay white — which is the whole point
of offsetting the screens, all four values worked out from the three
centres by hand. A second test isolates the averaging itself: a 4×4
layer whose top half is black and bottom half white averages to
`(0·8 + 255·8) / 16 = 127` (truncated) in R's one cell, and
`(dx² + dy²) · 255 ≤ 4 · 127 = 508` keeps only `dx² + dy² ≤ 1`, a
five-pixel plus shape around the centre — smaller than the eleven-pixel
dot a full average of 255 gives and bigger than the single centre pixel
a wrongly-computed average of 0 would give, so the test pins the average
itself and not just the geometry, with all five plus-shape coordinates
listed. A one-pixel selection changes only that pixel (to cyan) and
reports a 1×1 dirty rect; a zero radius, a locked layer and an unknown
id all error without touching pixels. All four hand-derived value sets
matched on the first run, cross-checked with a small Python script before
being written into the test. Live under Xvfb on the bundled gradient
sample at radius 8, the whole canvas resolved into a regular grid of
overlapping blue, green and magenta dots that grow and shrink with the
local tone — blue-dominant where the sample is darkest blue, magenta
where red and blue both run high, green and yellow toward the bright
corner — exactly the expected colour-halftone look. Undo restored the
original crisp gradient and grid lines.

**437 Rust tests total** (433 → 437, 430 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 41 — Filter > Pixelate > Crystallize

The other unshipped filter from the earlier Pixelate audit: instead of
Color Halftone's regular dot grid, Crystallize breaks the layer into
irregular polygonal "crystal" cells — a Voronoi diagram — and fills each
with its own average colour. `Document::crystallize(id, cell_size,
seed)` reuses `Mosaic`'s anchored, edge-clamped grid of `cell_size`-pixel
squares, but instead of colouring each grid square directly, it places
one randomly jittered "site" inside each square (two draws from the
seeded `XorShift32` generator Add Noise, Diffuse and Glowing Edges
already use, mapped to an offset inside that square's own — possibly
clamped — width and height) and then assigns every pixel in the layer to
whichever of the up to nine sites in its own grid square and the eight
squares around it is nearest; since sites are never more than one grid
square apart, the true nearest site is always among those nine, so the
search stays small and bounded. That's a documented simplification of
Photoshop's denser, unstructured point scattering — one jittered site
per grid square rather than a true Poisson-disc distribution — chosen
because it still produces organic-looking cells while keeping the
algorithm's cost and its test values tractable. Every pixel (selected or
not, so an edit still averages in its unselected neighbours, exactly
`mosaic`'s convention) is assigned to its nearest site in a first pass
that accumulates every channel — alpha included — into a running sum per
site; a second pass computes each site's average and, through the
`filter_pixels` skeleton, writes only the selected pixels with their
site's average. The frontend sends a fresh seed on every apply, as with
Add Noise and Diffuse, through a **Crystallize…** dialog with a Cell Size
slider (3–64 px, Photoshop's own dialog runs 3–300).

**Verified two ways.** Four new `document.rs` tests, all built on the
existing `ramp_square(6)` fixture (red = `10x + y`) with `cell_size = 3`
(an exact 2×2 grid of 3×3 squares) and `seed = 1` — the same seed-1
xorshift32 sequence the Diffuse tests already pin. Mapping its first
eight draws through `draw % 3` (each square is exactly 3 px wide) places
the four sites at (0, 1), (3, 2), (2, 4) and (4, 3), one per square in
scan order; a small Python script implementing the same nine-neighbour
search and per-site averaging this method does was run first to get
ground truth, then cross-checked pixel by pixel before being written
into the test. Averaging all 36 pixels over their nearest site gives
four region colours — 7, 35, 19 and 49 — and the test spot-checks one
pixel from each region (for instance `(0, 0)` and `(2, 0)` both land in
the 7-region despite being two squares apart, while `(3, 0)` two pixels
away is already in the 35-region), plus confirms the flat green channel
and full alpha survive untouched. A second test confines the same
computation to a one-pixel selection: since the whole-canvas pass that
builds the site averages never looks at the selection, the touched
pixel gets the identical value (7) it would without one, while its
unselected neighbour keeps its original ramp value and the dirty rect is
the one pixel. A third test confirms the same seed reproduces byte-
identical output while a different seed changes it. The fourth checks
that a zero cell size, a locked layer and an unknown id all error
without touching pixels. All four passed on the first run against the
scripted ground truth. Live under Xvfb on the bundled gradient sample at
cell size 16, the smooth gradient broke into a mosaic of irregular flat-
coloured polygons that still visibly followed the underlying colour
flow — blue in the corner, magenta along the top edge, green along the
left, cream in the bright corner — exactly the crystallize look. Undo
restored the original crisp gradient and grid lines.

**441 Rust tests total** (437 → 441, 434 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 42 — Filter > Pixelate > Pointillize

The last Pixelate filter, and one that reuses almost everything
Crystallize just built. Photoshop's Pointillize scatters solid dots
across the canvas over a plain background — the classic pointillist
look — and this implementation gets there by scattering the exact same
jittered Voronoi sites Crystallize uses, then stamping a solid,
`cell_size / 2`-pixel-radius circle at each one instead of filling its
whole region. Crystallize's site generation, nearest-site search and
per-site averaging were pulled out into three shared free functions —
`jittered_sites`, `nearest_site` and `voronoi_site_averages` — so
`Document::pointillize(id, cell_size, background, seed)` is now a thin
wrapper: it builds the same sites and averages Crystallize would, then
for each selected pixel checks whether it's within its *nearest* site's
radius (so neighbouring dots can never overlap, even when their sites
land closer together than `cell_size` apart, since a pixel only belongs
to a dot when that dot's site is also its nearest one); inside, the
pixel gets that site's average colour, exactly as Crystallize computes
it; outside, the caller-supplied `background` RGBA colour. Photoshop
paints the gaps with the current background-colour swatch; since this
project has no persistent background-colour setting, the colour is
passed in directly from the dialog instead. A **Pointillize…** dialog
adds a Cell Size slider (3–64, matching Crystallize's) and a colour
picker for the background, defaulting to white; the frontend sends a
fresh seed on every apply, as Crystallize and Add Noise already do.

**Verified two ways.** Refactoring Crystallize into shared helpers first
was itself verified for free: all four of its existing tests were rerun
immediately after the refactor and passed with byte-identical output,
confirming the extraction changed nothing about its behaviour. Four new
`document.rs` tests then cover Pointillize itself, built on the same
`ramp_square(6)`, `cell_size = 3`, `seed = 1` fixture as the Crystallize
tests — the same four sites at (0, 1), (3, 2), (2, 4) and (4, 3) and the
same four region averages (7, 35, 19, 49) — but now with `radius =
cell_size / 2 = 1`, so only the site itself and its up to four
orthogonal neighbours fall inside each dot. Site (0, 1)'s whole plus
shape is on-canvas — (0, 0), (0, 1), (0, 2) and (1, 1) all come out 7 —
while the diagonal neighbour (1, 0), one step further from the site,
falls in a gap and comes out the white background; three more spot
checks confirm the other three sites' dots land exactly where the same
Python reference script used for Crystallize says they should. A
selection test confirms the whole-canvas averaging pass still runs
regardless of the selection (the one touched pixel gets the identical
7 an unrestricted run would give) while an untouched neighbour is left
at its original ramp value rather than being painted with the gap
colour. A third test confirms same-seed determinism and cross-seed
difference, and the fourth checks the usual zero-cell-size,
locked-layer and unknown-id errors. All four passed on the first run.
Live under Xvfb on the bundled gradient sample at cell size 16 with a
white background, the smooth gradient turned into a scatter of small
solid-coloured dots over white, following the same colour flow
Crystallize's polygons did — recognizably pointillist. Undo restored
the original crisp gradient and grid lines.

**445 Rust tests total** (441 → 445, 438 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

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
`.msi`/`.exe` on Windows, `.deb`/`.rpm`/`.AppImage` on Linux).

**Verified for real**, not just wired up: ran this on Linux, then `dpkg -i`'d
the resulting `.deb` — a genuine system package install, with a `.desktop`
entry and icons registered, not a dev build. Launched the installed
`/usr/bin/image-editor` binary (not `cargo run`, not `tauri dev`) under Xvfb
and drove it: **New…** created a document, and a real pointer drag painted a
stroke on it, both rendering correctly. Confirms the packaged build actually
runs and works, not just that it compiles. Not yet verified: macOS, and
actually running the Windows `.msi`/`.exe` (built by CI below, but only
compiled and uploaded there — never installed and launched on a real or
virtual Windows machine).

## Tests

```bash
cd src-tauri && cargo fmt --check
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo test      # 142 tests: blend math, model, strokes (incl. flood fill/gradient), compositor (incl. merge visible/flatten image), dirty-region recompositing, protocol, export, project files (incl. layer lock), new document, selections (incl. select all/invert/reselect), layer lock, merge visible, flatten image, merge down, eyedropper, undo/redo, pipeline
npm run build                   # frontend: typecheck + production build
```

These same five commands run in CI (`.github/workflows/ci.yml`) on every push to
`main` and every pull request, in two parallel jobs. The Rust job installs the
GTK/WebKit headers Tauri needs on Linux; it does **not** build the frontend
first, because `tauri::generate_context!` tolerates a missing `dist/` — only a
real bundle needs it.

A third job, `rust-windows`, runs the same `fmt`/`clippy`/`test` trio on
`windows-latest` (WebView2 ships preinstalled there, so no system deps to
install). It only runs on pushes to `main` and on manual dispatch, not on
every PR — Windows minutes bill at 2x on this private repo, so PRs stay
Linux-only for fast, cheap feedback, and the Windows build is checked when a
branch actually lands.

A fourth, `build-installers`, actually runs `npm run tauri:build` — a full
release compile and bundle, on Linux and Windows in parallel — and uploads
the resulting installers as workflow artifacts. Same gating as
`rust-windows` and for the same reason (billing, and there's no reason to
pay for a release compile on every PR push before a release is wanted); this
is CI's version of the local **Build a distributable** step above, kept as a
separate job rather than folded into `rust`/`rust-windows` so a slow release
build never blocks the fast fmt/clippy/test feedback on every PR. macOS is
still deferred, same as everywhere else in this project.

macOS builds and the native file dialog are still unverified.

The Rust suite is where the behaviour is pinned: blend-function identities and
singularities, layer operations and their error paths, brush and eraser
strokes (coverage, segment continuity, overlap handling, clipping),
compositing (opacity, visibility, stacking order, alpha accumulation),
exporting a document round-trips through PNG intact, undo/redo (checkpoint,
history bounding, redo-cleared-on-new-edit, the "nothing to undo/redo" error
paths), starting a blank document at a chosen size (and its memory limit),
rectangle/ellipse selections confining paint and erase strokes to their
bounds (select all, invert and its confinement math, and reselect all
included), a locked layer rejecting a stroke outright, merging visible
layers reproducing the same composite as the layers it replaces, flattening
discarding hidden layers' content entirely, merging one layer down into
another respecting each one's own visibility, sampling the exact colour at
a given pixel, a flood fill's 4-connectivity/tolerance/selection
confinement, a gradient's per-pixel interpolation with hand-computed exact
byte values, and end-to-end runs over the bundled samples. The frontend is a thin
shell over those commands and is covered by the typecheck plus the production
build.

## Layout

```
index.html              Vite entry point
src/
  App.tsx               UI shell: toolbar, canvas, drop target, command plumbing
  LayerPanel.tsx        layer list and per-layer controls
  types.ts              mirrors the Rust types crossing the IPC boundary
  main.tsx              React root
  styles.css
src-tauri/
  src/blend.rs          blend modes and their functions
  src/document.rs       Document and Layer: the core model
  src/composite.rs      the compositor
  src/png.rs            PNG decode and composite encode
  src/project.rs        the layered project file format
  src/lib.rs            Tauri commands, AppState, and the composite:// protocol
  src/main.rs           desktop entry point
  tests/pipeline.rs     end-to-end tests over the bundled samples
  tauri.conf.json       window, bundle, and CSP config
  capabilities/         per-window permission grants
samples/                test images
```

## How an image gets on screen

1. React sends a file path, or a layer edit, to one of the Rust commands in
   `lib.rs`. The open document lives in Rust behind a mutex; the frontend holds
   no pixel data.
2. The command mutates the `Document`, re-runs the compositor — the whole
   document, or (for a stroke) just the rect it touched, see Phase 6 —
   PNG-encodes the result, and caches both the raw pixels and the encoded
   bytes in `AppState` behind a generation counter. It returns that counter
   together with the new layer state — not the bytes.
3. React points its `<img>` at `composite://composite.png?g=<generation>`. The
   webview's own resource fetch hits the `composite://` protocol registered in
   `lib.rs`, which serves the cached bytes straight back, no IPC round trip for
   the pixels themselves.

One command call per edit either way, but the composite no longer inflates
through base64 or shares the IPC channel with everything else — files up to
the 64 MB decode ceiling ship as a plain binary response.
