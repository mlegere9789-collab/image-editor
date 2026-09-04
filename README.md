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

## Phase 11 — Invert / Threshold / Posterize / Brightness-Contrast / Hue-Saturation / Black & White / Vibrance / Photo Filter / Exposure / Gradient Map / Channel Mixer / Levels (adjustments)

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
