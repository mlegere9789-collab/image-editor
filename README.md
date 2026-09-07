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

## Phase 43 — Filter > Distort > Wave

The last new Distort filter, and the one that turns Ripple's single fixed
sine into Photoshop's actual dialog: a count of independently randomised
generators, each with its own wavelength and amplitude drawn from a
range. `Document::wave(id, generators, wavelength_min, wavelength_max,
amplitude_min, amplitude_max, horizontal_scale, vertical_scale, seed)`
draws three values per generator from the seeded `XorShift32` generator
— a wavelength in `wavelength_min..=wavelength_max`, an amplitude in
`amplitude_min..=amplitude_max`, and a phase offset in `0..wavelength` —
the same style of seeded draw every randomised filter here uses. Every
pixel's horizontal displacement is `horizontal_scale / 100` times the
*sum*, over every generator, of `amplitude · sin(2π · (y + phase) /
wavelength)`; its vertical displacement is the same sum over `x` instead
of `y`, scaled by `vertical_scale / 100` — the same axis-swap Ripple
uses, now with several waves layered together instead of one. Sampling
is nearest-neighbour with edge repeat via `sample_nearest`, the scope
cut Ripple and Twirl already make; Photoshop's Triangle and Square wave
types and its Wrap Around undefined-area mode are further, documented
scope cuts — only Sine and Repeat Edge Pixels are implemented. A
**Wave…** dialog exposes all seven of Photoshop's own numeric controls:
Number of Generators, Wavelength Min/Max, Amplitude Min/Max, and
Horizontal/Vertical Scale, with the min/max sliders keeping each other
consistent (dragging one past the other drags it along too, since a
maximum below its minimum is rejected).

**Verified two ways.** Four new `document.rs` tests. With one generator
and both wavelength and amplitude fixed to a single value (min = max —
so only the phase draw does anything), the effect collapses to exactly
Ripple's own formula with a phase shift: seed 1's third draw gives phase
1, and on the 4×4 `ramp_square` fixture (red = `10x + y`) the resulting
per-pixel displacement was worked out entirely by hand — `(0, 0)` reads
`(1, 1) = 11`, `(1, 0)` reads `(2, 0) = 20`, `(2, 0)` and `(3, 0)` both
clamp to `(3, 0) = 30`, `(1, 1)` and `(3, 3)` land on themselves, and
`(2, 2)` reads `(1, 1) = 11` — then cross-checked with a small Python
script evaluating the same formula. A second test uses two generators
(wavelength 2–6, amplitude 1–3) on the 6×6 fixture; the six draws it
consumes were fed through a Python reference implementing the same
draw-and-map logic to confirm five spot-checked pixels, proving the
displacements really do sum rather than the last generator simply
overwriting the others. A third test confirms both scales at 0 is the
identity and that a one-pixel selection moves only that pixel with a
1×1 dirty rect. The fourth checks that zero generators, a zero minimum
wavelength, a wavelength or amplitude maximum below its minimum, a
locked layer and an unknown id all error without touching pixels. All
four passed on the first run. Live under Xvfb on the bundled gradient
sample at the defaults (5 generators, wavelength 10–40, amplitude
5–20), the crisp grid lines dissolved into a jagged, chaotic-looking
distortion — visibly busier than Ripple's single clean sine, exactly
what summing five independently randomised waves should look like — and
the layer's own border showed the same jaggedness. Undo restored the
original crisp gradient and grid lines.

**449 Rust tests total** (445 → 449, 442 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 44 — Filter > Distort > Shear

Photoshop's Shear bends a layer along a vertical curve dragged in its own
dialog strip; `Document::shear(id, control_points, wrap_around)` keeps the
spirit — a curve of horizontal offsets running top to bottom — but trades
Photoshop's smooth spline for `control_points.len()` evenly spaced
anchors joined by straight segments, a documented scope cut made because
straight segments between a handful of slider values are easy to
hand-verify and easy to expose as plain sliders, unlike a spline. For row
`y`, the anchor curve is sampled at `t = y / (height − 1)` by linearly
interpolating between the two anchors `t`'s position falls between, and
the result is rounded to the nearest whole pixel — this filter shifts
whole rows rather than resampling them. Every pixel in that row is then
pulled from `x − offset(y)` in the same row, so a positive offset drags
the row's content right. What happens off the left or right edge is
`wrap_around`, Photoshop's own radio choice: `false` clamps the source
column into range (**Repeat Edge Pixels**, the convention every other
Distort filter here uses via `sample_nearest`); `true` wraps it with
`rem_euclid` instead (**Wrap Around**), so content sheared off one edge
reappears on the other — the one mode Wave's own write-up above left as a
scope cut, implemented here since Shear is the filter Photoshop actually
puts it on. A **Shear…** dialog exposes five control-point sliders
(Top, three intermediate anchors, Bottom, each −100..=100 px) plus the
Repeat Edge Pixels / Wrap Around radio pair.

**Verified two ways.** Five new `document.rs` tests, all hand-derived
against the implementation's own linear-interpolation formula. A flat
curve (`[0, 0]`) is confirmed the identity under both edge modes. A
three-anchor curve `[0, 3, 0]` over a 4-row `ramp_square` fixture (red =
`10x + y`) gives row offsets `[0, 2, 2, 0]` by hand — row 1 and row 2 sit
2/3 and 1/3 through their respective segments — and the resulting pixels
under Repeat Edge Pixels were worked out by hand from that: row 0 and row
3 (offset 0) untouched, rows 1 and 2 (offset 2) reading columns clamped
at 0 for the two left columns that shift off the edge. A second test
reuses that same curve and row-1 offset with `wrap_around = true` and
confirms the two columns that clamped to 0 in the previous test instead
wrap to columns 2 and 3, reading further along the row. A third confirms
a one-pixel selection moves only that pixel with a 1×1 dirty rect. The
fourth checks that fewer than two control points, a non-finite one, a
locked layer, and an unknown id all error without touching pixels. All
five passed on the first run. Live under Xvfb on the bundled gradient
sample, a curve with the top anchor at +87 px and the bottom at −91 px
(Wrap Around selected) turned the crisp rectangular grid into a
consistent diagonal slant, with the wrapped-around content visible as
small triangular slivers of the opposite edge's colour tucked into the
top-left and bottom-right corners — exactly the "this edge reappears on
that one" behaviour `rem_euclid` is meant to produce. Undo restored the
original, unsheared grid.

**454 Rust tests total** (449 → 454, 447 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 45 — Filter > Render > Clouds and Difference Clouds

The first Render filters, and the first filters in this project that
*generate* content rather than transform what's already there. Both share
`clouds_field(width, height, seed)`, a fractal value-noise field in `[0,
1]` — a documented, hand-checkable approximation of Photoshop's own
undocumented, proprietary cloud renderer, not a port of it. Four octaves
are summed, each half the previous octave's cell spacing and half its
weight, starting from `base_cell = (width.max(height) / 4).max(1)`
pixels; each octave draws a grid of pseudo-random corner values from the
seeded `XorShift32` generator — coarsest octave first, the same style of
seeded draw every randomised filter here uses — and every pixel's value
for that octave is bilinearly interpolated between the four grid corners
around it. The weighted sum (weights 1, 1/2, 1/4, 1/8) is divided by the
total weight, keeping the field within `[0, 1]`.

`Document::clouds(id, foreground, background, seed)` replaces every
selected pixel outright — Render filters generate new content rather than
transforming existing pixels, the one place in this project where the
source pixel is ignored entirely — with `background` lerped to
`foreground` by the noise field, channel by channel including alpha.
Photoshop's own Clouds dialog has no controls at all; it paints with
whatever the current foreground/background colours are, so those (plus
the seed, standing in for Photoshop's own unseeded randomness) are this
filter's only real parameters. `Document::difference_clouds` reuses the
exact same noise field and foreground/background lerp to get a "cloud
colour", but instead of replacing the pixel it combines that colour with
the layer's *existing* one via the Difference blend formula, `|existing −
cloud|`, on the three colour channels only — alpha is left untouched,
since Difference is a colour blend. Repeated applications of Difference
Clouds fold the pattern back on itself, which is Photoshop's own
description of the effect. A **Clouds…** and a **Difference Clouds…**
dialog each expose a Foreground and a Background colour picker.

**Verified two ways.** Six new `document.rs` tests, split evenly between
the two filters. `clouds`'s first test uses a 16×16 canvas (`base_cell =
16 / 4 = 4`, so the four octaves' cell spacings are 4, 2, 1, 1 — the last
two land exactly on pixel corners, meaning no interpolation, while the
first two exercise it for real) and seed 1: a Python port of
`clouds_field`'s exact arithmetic — same `XorShift32` sequence, same
bilinear and weighting maths — computed the noise field independently,
giving `0.29261351729122304` at `(0, 0)` and `0.44604673015419394` at
`(7, 7)`; lerping `background = [10, 20, 30, 255]` to `foreground = [200,
150, 50, 255]` by those two values and rounding predicts `[66, 58, 36,
255]` and `[95, 78, 39, 255]`, which is exactly what the Rust
implementation produced. A second test confirms a one-pixel selection
changes only that pixel (checked by inequality against the original,
since the noise value itself isn't the point of that test) with a 1×1
dirty rect; a third confirms a locked layer and an unknown id both error.
`difference_clouds` reuses the same computed field and foreground/
background values against a solid `[100, 100, 100, 255]` layer: the same
Python script's cloud colours, `[66, 58, 36]` and `[95, 78, 39]`, put
through `|100 − cloud|` predict `[34, 42, 64]` and `[5, 22, 61]` with
alpha left at 255 — again exactly what the Rust implementation produced —
plus matching selection-confinement and error-propagation tests. All six
passed on the first run. Live under Xvfb on the bundled gradient sample:
**Clouds** with the default white foreground and black background
replaced the crisp colour grid with a soft, genuinely cloud-like
grey-on-white fractal pattern; Undo restored the gradient. **Difference
Clouds** on the same sample produced the classic "psychedelic" look —
saturated, complementary-looking colour swirls where the cloud pattern
and the gradient's own colours cancelled and clashed — and Undo again
restored the original gradient.

**460 Rust tests total** (454 → 460, 453 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 46 — Filter > Pixelate > Facet

Photoshop's own Facet has no dialog at all — it clumps pixels of similar
colour into small blocks using its own undocumented algorithm, described
as giving flat art "a hand-painted appearance." That's the same shape of
effect Crystallize already produces, just at a smaller, fixed scale, so
rather than invent and separately verify a second clumping algorithm,
`Document::facet(id, seed)` is a documented delegation: `self.crystallize(id,
FACET_CELL_SIZE, seed)` with `FACET_CELL_SIZE` a fixed constant (4 pixels)
chosen small enough that the cells read as paint-like clumps rather than
Crystallize's own showcase-sized crystals, and no user-facing parameters
beyond the seed — matching Photoshop's own parameterless dialog. A
**Facet** button applies it directly, no modal, the same direct-apply
pattern Find Edges and Solarize already use for parameterless filters.

**Verified two ways.** Two new `document.rs` tests. Since the algorithm
itself is exactly Crystallize's, already covered by that filter's own
five tests, `facet`'s test doesn't re-derive the maths — it instead pins
the delegation: running `facet(id, 7)` on one document and
`crystallize(id, 4, 7)` on an identical one produces byte-identical
pixels, confirming `FACET_CELL_SIZE` really is 4 and the seed really does
pass straight through. A second test confirms a locked layer and an
unknown id both error. Both passed on the first run. Live under Xvfb on
the bundled gradient sample, Facet's small fixed cell size showed up as
fine, irregular jittered-cell edges breaking up the smooth gradient and
its ruled grid lines — visibly finer and more painterly than
Crystallize's own default-sized crystals — and Undo restored the crisp
original.

**462 Rust tests total** (460 → 462, 455 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 47 — Filter > Render > Fibers

The third Render filter, and — like `clouds` — a documented approximation
of an undocumented, proprietary Photoshop renderer rather than a port of
one. Where `clouds_field` builds smooth noise from a coarse grid
bilinearly interpolated per pixel, `Document::fibers` takes the opposite
approach: independent per-pixel white noise, one draw per pixel from the
seeded `XorShift32` generator, then averaged vertically down each column
— averaging along a single axis is exactly what turns noise into
streaks, so this is the natural way to get a fibrous, woven look rather
than Clouds' soft blobs. `variance` (Photoshop's own 1..=100 range)
scales how far each raw draw can stray from grey before smoothing: `0.5
+ (n − 0.5) · variance / 100`, so 100 passes the full `[0, 1)` draw
through unscaled and 1 collapses nearly everything to a flat 0.5 — low
variance means long, uniform fibres once smoothed; high variance means
short, choppy ones, matching Photoshop's own description of the control.
`strength` (Photoshop's own 1..=64 range) is the radius of the vertical
box average taken independently down each column, `2 · strength + 1`
samples with edge repeat past the top and bottom; a higher strength
smooths further, stretching the fibres out. The resulting `[0, 1]`
fraction is lerped, channel by channel including alpha, between
`background` and `foreground`, replacing every selected pixel outright
the same way `clouds` does. A **Fibers…** dialog exposes both of
Photoshop's own numeric controls (Variance, Strength) plus the
Foreground/Background colour pickers `clouds` and `difference_clouds`
already use.

**Verified two ways.** Four new `document.rs` tests, all grounded in a
Python port of `fibers`'s exact arithmetic (same `XorShift32` sequence,
same variance scaling, same vertical box average with edge-clamped
indices) run independently on a 3×5 canvas. At variance 100 and strength
1 (a 3-sample vertical average), seed 1, the port gives `t(0,0) =
0.023914845117057364`, `t(1,2) = 0.42619942237312597` and `t(2,4) =
0.12960797804407775`; lerping `background = [10, 10, 200, 255]` to
`foreground = [220, 30, 30, 255]` by those and rounding predicts `[15,
10, 196, 255]`, `[100, 19, 128, 255]` and `[37, 13, 178, 255]`
respectively — exactly what the Rust implementation produced. A second
test re-runs the same fixture at variance 1 instead of 100: the same
port gives `t(0,0) = 0.4952391484511706`, almost exactly grey, lerping to
`[114, 20, 116, 255]` — near the midpoint between the two colours,
demonstrating the variance scaling's compression toward 0.5 concretely
rather than just asserting an inequality. Selection-confinement and
error-propagation (a variance outside `1..=100`, a zero strength, a
locked/unknown layer) tests round it out. All four passed on the first
run. Live under Xvfb on the bundled gradient sample, Fibers with the
default variance (50), strength (4), white foreground and black
background produced a clean, fine, vertically-streaked grey texture —
visually indistinguishable from Photoshop's own "brushed metal" look for
this filter — and Undo restored the original colour gradient.

**466 Rust tests total** (462 → 466, 459 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 48 — Filter > Pixelate > Mezzotint

Photoshop's own Mezzotint offers several pattern types — dots, lines and
strokes at various sizes — through its own undocumented algorithm.
`Document::mezzotint(id, cell_size, seed)` collapses these to one:
square cells of random dots, on the grounds that replicating the others'
exact undocumented shapes wouldn't be any more hand-verifiable than this
one, and dots are the type that best matches the coarse, blocky
"engraving" look the filter is named for. `cell_size` sets up the same
anchored grid `mosaic` and `color_halftone` already use; each cell
averages its red, green and blue channels independently (alpha is left
untouched, the same convention `color_halftone` uses) and draws its own
random threshold per channel from the seeded `XorShift32` generator — a
channel becomes solid 255 for every pixel in that cell if the cell's
average for that channel exceeds the draw, solid 0 otherwise, so a whole
cell's channel flips together rather than any mid-tone surviving. Since
each of the three channels thresholds independently, a single cell's
output is always one of eight colours (black, the three primaries, the
three secondaries, white) — the same eight-colour palette `color_halftone`
produces, for the same reason.

**Verified two ways.** Three new `document.rs` tests. The first uses a
4×4 canvas split into four solid-coloured 2×2 quadrants — (100, 50, 200),
(10, 240, 30), (255, 0, 0) and (128, 128, 128) — so each cell's average is
exactly its quadrant's colour; a Python port of the same seeded
per-cell-per-channel draw (seed 3) gives thresholds `(99, 3, 71)`, `(73,
203, 243)`, `(67, 4, 15)` and `(79, 1, 10)` for the four cells in turn.
Comparing each average against its own threshold by hand — e.g. quadrant
one's 100 > 99, 50 > 3 and 200 > 71 are all true — predicts white, pure
green, pure red and white for the four quadrants respectively, confirmed
pixel-for-pixel against the actual output, including that every pixel
within a cell shares its cell's single outcome. A second test reuses that
exact fixture and seed to confirm a one-pixel selection changes only that
pixel (to the already-known white) with a 1×1 dirty rect, alpha
untouched; a third confirms a zero cell size, a locked layer and an
unknown id all error. All three passed on the first run. Live under Xvfb
on the bundled gradient sample at the default 8px cell size, the smooth
gradient collapsed into a coarse, high-contrast field of solid red,
green, blue, cyan, magenta, yellow, black and white blocks — visually a
dead match for Photoshop's own random-dot Mezzotint look — and Undo
restored the original gradient.

**469 Rust tests total** (466 → 469, 462 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 49 — Filter > Render > Lens Flare

The fourth Render filter, and the first that composites onto the layer's
existing colour rather than replacing it outright, generating new
content but blending it in the way an actual light source would.
`Document::lens_flare(id, center_x, center_y, brightness)` screens two
elements onto the layer: the main flare itself, and a smaller, dimmer
secondary reflection mirrored through the canvas centre — the two
elements a viewer's eye is drawn to first in a real flare. This is a
documented, deliberately reduced approximation of Photoshop's own four
lens types, which add several more hexagonal or ring-shaped secondary
flares along that same line; those are a scope cut, since a closed-form
radial falloff is what makes this filter hand-verifiable at all, and
hexagons or rings wouldn't be. The radius, `0.15 · min(width, height)`,
isn't user-adjustable, matching Photoshop's own dialog, which has no
size control either — only Brightness (10..=300 %, its own exact range)
and the flare's centre point. The main flare's intensity at distance `d`
is a soft core, `(1 − d/radius)²` out to `radius`, plus a wider, dimmer
halo, `0.3 · (1 − d/(4·radius))²` out to `4·radius`; the secondary flare
— `0.35 · radius` in size, positioned at the point on the far side of the
canvas centre from the main flare — is a bare core at 0.4× strength.
Both intensities sum, scale by `brightness / 100`, and clamp to `[0, 1]`;
that fraction screens each of the three colour channels toward white,
`existing + t · (255 − existing)`, leaving alpha untouched. A **Lens
Flare…** dialog exposes Center X/Y sliders (bounded to the canvas, and
seeded to its centre when the dialog opens) plus the Brightness slider.

**Verified two ways.** Three new `document.rs` tests. A Python port of
the exact core/halo/secondary falloff formulas independently computed a
5×5 canvas — solid `(50, 50, 50, 255)`, flare at `(1, 1)`, brightness
150 % — pixel by pixel: the flare's own centre saturates to pure white
since `d = 0` maxes the core out before scaling; the secondary
reflection, mirrored through the canvas centre `(2, 2)` onto `(3, 3)`,
comes out at `(173, 173, 173)` (`0.4` core `× 1.5` brightness `= 0.6`
screened against 50); the far corner `(4, 0)` is untouched, far enough
from both flares to clear even the wide halo's `4 · radius` cutoff — all
three matched the Rust implementation exactly. A second test confirms a
one-pixel selection changes only that pixel with alpha untouched and a
1×1 dirty rect; a third confirms a brightness outside `10..=300` and a
locked/unknown layer all error. All three passed on the first run. Live
under Xvfb on the bundled gradient sample, positioning the flare toward
the upper-left at 236 % brightness produced a bright core with a soft
surrounding glow plus a distinctly visible secondary dot on the mirrored
side of the canvas — an immediately recognisable lens flare — and Undo
restored the original gradient.

**472 Rust tests total** (469 → 472, 465 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 50 — Filter > Distort > Displace

The last new Distort filter. Photoshop's own dialog browses to a separate
displacement-map file and offers Stretch to Fit or Tile to reconcile that
file's size with the target canvas; `Document::displace(id, map_layer_id,
horizontal_scale, vertical_scale, wrap_around)` sidesteps that whole
problem by using an already-open layer as the map instead — every layer
here shares the document's exact canvas size, so the map is never
resampled, and Stretch to Fit/Tile become moot rather than needing their
own implementation. For pixel `(x, y)`, the map layer's red channel
there gives the horizontal displacement, `(red − 128) / 128 ·
horizontal_scale`, and its green channel the vertical, `(green − 128) /
128 · vertical_scale` — 128 (mid-grey) means no shift in either
direction, 0 the full shift one way and 255 the full shift the other,
exactly Photoshop's own convention. The displaced position is rounded to
the nearest whole pixel — this filter moves whole pixels rather than
resampling, the same convention `shear` uses — and, also like `shear`,
`wrap_around` picks between Photoshop's two undefined-area modes: clamping
the source into range (**Repeat Edge Pixels**) or wrapping it with
`rem_euclid` (**Wrap Around**). A **Displace…** dialog exposes a
Displacement Map layer picker (populated from every other layer in the
document), Horizontal/Vertical Scale sliders, and the same Repeat Edge
Pixels/Wrap Around radio pair `shear` uses; it's disabled whenever fewer
than two layers exist, since a document needs a second layer to serve as
the map.

**Verified two ways.** Four new `document.rs` tests, all built on one
shared 3×3 map fixture: mid-grey `(128, 128)` everywhere except `(0, 0)`
at `(127, 127)` and `(2, 2)` at `(130, 128)`. Paired with a horizontal and
vertical scale of exactly 128.0, `(map − 128) / 128 · 128` reduces to
clean integer pixels of displacement — worked out by hand: `(0, 0)`
shifts by `(−1, −1)`, `(1, 1)` doesn't move, and `(2, 2)` shifts by `(+2,
0)`. On the 3×3 `ramp_square` fixture (red = `10x + y`), the first test
confirms Repeat Edge Pixels: `(0, 0)`'s shift clamps back to itself
(red 0), `(1, 1)` is unchanged (red 11), and `(2, 2)`'s shift also clamps
back to itself (red 22). A second test reuses the identical fixture under
Wrap Around instead: `(0, 0)` now wraps to `(2, 2)` (red 22) and `(2, 2)`
wraps to `(1, 2)` (red 12), while `(1, 1)` is unaffected either way. A
third test confirms a one-pixel selection under Wrap Around changes only
that pixel (Repeat Edge Pixels was deliberately not used here, since
`(0, 0)`'s shift happens to clamp back to itself under that mode — a
coincidental no-op rather than a real check, the same pitfall caught and
fixed in Mezzotint's own selection test); a fourth confirms a non-finite
scale, an unknown map layer, and a locked/unknown target layer all error.
All four passed on the first run. Live under Xvfb: loaded the bundled
gradient sample, added a second copy of it as a layer to serve as the
map, and applied Displace at a 75px horizontal and vertical scale — the
canvas's ruled grid lines visibly bent and warped following the map's own
smooth colour gradient, most noticeably near the top-left where the
map's colours vary fastest — and Undo restored the original, perfectly
straight grid.

**476 Rust tests total** (472 → 476, 469 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 51 — Filter > Stylize > Extrude

Pops the layer into a grid of raised square blocks — a documented,
deliberately simplified stand-in for Photoshop's real 3-D block rendering
(viewer-facing side walls, a genuine perspective pop), chosen because a
closed-form diagonal shade is hand-verifiable in a way that projecting
cube faces isn't. `Document::extrude(id, cell_size, depth, random, seed)`
reuses `mosaic`'s exact anchored grid and per-cell averaging, so every
block starts as a flat square filled with its own average colour —
Photoshop's Pyramids block type and its stretched-image front faces
(as opposed to Solid Front Faces) are further, documented scope cuts.
`cell_size` (Photoshop's own 2..=255 range) sets the block size and
`depth` (its own 1..=255 range) the maximum shading swing; each block's
own factor — how much of that swing it actually gets — is either drawn
fresh from the seeded `XorShift32` generator (`random = true`,
Photoshop's own Random depth) or the block's own ITU-R BT.601 luma over
255 (`random = false`, Level-based: brighter blocks pop harder, the same
luma weights `threshold` and `black_and_white` already use). For a pixel
at local position `(lx, ly)` inside its block, `t = ((cell−1−lx) +
(cell−1−ly)) / (2·(cell−1)) − 0.5` runs from `+0.5` at the top-left
corner to `−0.5` at the bottom-right, and every colour channel (never
alpha, which stays the block's own average like `mosaic`) is offset by
`t · factor · depth`, clamped to `0..=255` — a diagonal bevel from bright
to dark that reads as a raised block without any actual 3-D geometry. An
**Extrude…** dialog exposes Size and Depth sliders plus a Level-based /
Random radio pair.

**Verified two ways.** Four new `document.rs` tests. The first uses a
single 4×4 block, solid `(200, 100, 50, 255)`, so its average is exactly
that colour and its luma works out to `124.2` (factor `0.4870588...`);
at depth 100 the diagonal formula predicts `+24.35` at the top-left
corner, `−24.35` at the bottom-right, and `±8.12` one step in from each —
all four hand-computed values, cross-checked with a small Python script,
matched exactly. A second test puts two solid-coloured 4×4 blocks side
by side in Random mode: a Python port of the same seeded `XorShift32`
sequence independently computed the two blocks' factors
(`0.6321277192328125` and `0.5212643640115857`), and at depth 255 the
resulting corner shades clamp at both extremes — one corner to `0`, the
opposite block's corner to `255` — exercising the clamp alongside the
random draw itself. A third confirms a one-pixel selection changes only
that pixel (to the already-known top-left-corner colour from the first
test) with alpha untouched; a fourth confirms a cell size or depth
outside their Photoshop ranges and a locked/unknown layer all error. All
four passed on the first run. Live under Xvfb on the bundled gradient
sample at the default 20px size and 30 depth (Level-based), the smooth
gradient broke into a fine grid of individually diagonally-shaded
blocks — each visibly brighter at its top-left corner and darker at its
bottom-right, exactly the raised-block bevel the formula is meant to
produce — and Undo restored the original smooth gradient and its ruled
grid lines.

**480 Rust tests total** (476 → 480, 473 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 52 — Filter > Render > Lighting Effects

A single Point light re-lights the layer using its own ITU-R BT.601 luma
as a bump-mapped height field, shaded with a Blinn-Phong diffuse and
specular model — a documented, deliberately scoped-down stand-in for
Photoshop's real Lighting Effects, which supports up to three lights of
three different types (Point, Spot, Infinite), a texture-channel picker,
and Gloss/Metallic material sliders. `Document::lighting_effects(id,
light_x, light_y, light_height, intensity, ambience, bump_height,
color)` models exactly one Point light (Spot and Infinite are a
documented scope cut, as is the multi-light limit), always reads the
layer's own colour as its height field (no texture-channel picker), and
fixes the material to a non-metallic plastic via two constants,
`SPEC_STRENGTH = 0.3` and `SHININESS = 16` (standing in for the
Gloss/Metallic sliders Photoshop exposes). For every pixel `(x, y)`, a
central-difference gradient of the neighbouring luma — clamped at the
layer's edges rather than wrapping, `dzdx = (h(x+1,y) − h(x−1,y)) / 2 ·
scale` and the same for `dzdy`, where `scale = bump_height / 100` —
feeds a surface normal `n = normalize(−dzdx, −dzdy, 1)`. The direction to
the light, `l = normalize(light_x − x, light_y − y, light_height)`, gives
a diffuse term `max(0, n·l)`, and the classic Blinn-Phong half-vector
`h = normalize(l + (0, 0, 1))` (the fixed straight-on viewer this
project's flat 2-D canvas implies) gives a specular term `max(0, n·h)
^ SHININESS`. Each output channel is `orig · (ambience + intensity ·
diffuse) · tint + 255 · SPEC_STRENGTH · spec · tint`, rounded and clamped
to `0..=255`, where `tint = color[c] / 255` lets `color` tint both the
diffuse light and its highlight; alpha is untouched. `intensity`,
`ambience`, and `bump_height` are all 0-100 percentages, matching
Photoshop's own slider ranges for this simplified model. A **Lighting
Effects…** dialog exposes Light X/Y and Height sliders (X/Y default to
the canvas centre on open, the same pre-seeding `openLensFlareDialog`
already established), Intensity/Ambience/Bump Height sliders, and a
colour picker for the light's tint.

**Verified two ways.** Three new `document.rs` tests, all cross-checked
against an independent Python port of the same formula before being
written into Rust. The fixture is a 5×5 canvas split at `x = 2` — solid
`(200, 200, 200, 255)` for `x < 2`, solid `(50, 50, 50, 255)` for `x ≥
2` — lit by a light at `(1, 1, 30)` with intensity 100, ambience 20, bump
height 100, and a white tint. `(0, 0)` sits on the bright side closest to
the light, where the pre-clamp value is `315.94`, so it saturates to
`255`; `(4, 4)` is far on the dark side, pre-clamp `133.04 → 133`; `(2,
2)` sits exactly on the luma cliff, where the lateral gradient is so
steep (`dzdx = (50 − 200) / 2 = −75`) that the tilted normal makes both
the diffuse and specular terms clamp to zero against this light's
direction, leaving exactly the ambience floor, `50 × 0.20 = 10.0`, with
no fractional part to round at all; `(1, 3)` is bright side but far from
the light, pre-clamp `42.66 → 43`. Every one of these was deliberately
chosen clear of an exact `.5` boundary — an earlier fixture attempt (a
flat surface with the light directly overhead) kept landing on exact
half-integers, where Python's banker's rounding and Rust's
away-from-zero `f32::round()` disagree, and had to be discarded in
favour of this one. All three passed on the first run, matching the
Python reference exactly. A second test confines the same fixture to a
one-pixel selection at `(0, 0)` and confirms only that pixel changes (to
the already-verified `255`), with the rest of the canvas untouched. A
third confirms out-of-range intensity/ambience/bump-height, a
non-positive or non-finite light height, and a locked/unknown layer all
error. Live interactive verification under Xvfb was attempted but
blocked by an environment issue unrelated to this filter's code: this
session's Xvfb instance stopped delivering synthetic `xdotool` pointer
clicks to the webview entirely partway through this phase (confirmed via
a control test against an unrelated, pre-existing, always-enabled
toolbar button, and confirmed to persist across a full Xvfb-and-app
restart), so no click in the UI — on this filter's own dialog or on
anything else — could be exercised this time. The dialog's wiring
(state, the pre-seeded X/Y callback, the command call, and the
`runCommand("lighting_effects", …)` parameter names matching the Tauri
command's own camelCase-converted argument names exactly) was reviewed
by hand instead. This is a gap in this phase's verification, documented
rather than silently skipped, on top of test/build coverage that is
otherwise complete.

**483 Rust tests total** (480 → 483, 476 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 53 — Filter Gallery > Artistic > Colored Pencil

Emphasises edges with the layer's own colour and lets a flat "paper" grey
show through everywhere else — a documented approximation of Photoshop's
actual renderer, which draws directional cross-hatched pencil strokes
rather than a uniform per-pixel blend. `Document::colored_pencil(id,
pencil_width, stroke_pressure, paper_brightness)` builds an ITU-R BT.601
luma buffer from the layer, runs it through the same [`sobel_at`] edge
detector `find_edges` already uses, and widens that edge map with the
same [`extreme_at`] neighbourhood-maximum `glowing_edges`'s own
`edge_width` already uses, at radius `pencil_width − 1` (Photoshop's own
1..=24 range) — a wider pencil claims a wider halo around each edge.
`stroke_pressure` (0..=15) is a flat multiplier on the 0..=1-normalised
edge strength, and `paper_brightness` (0..=50) sets the flat grey
(`paper_brightness / 50 · 255`) that shows through wherever the blended
edge strength falls short of 1. Every colour channel becomes `orig ·
blend + paper · (1 − blend)`, rounded and clamped; alpha is untouched. A
**Colored Pencil…** dialog exposes Pencil Width, Stroke Pressure, and
Paper Brightness sliders.

**Verified two ways.** Five new `document.rs` tests, cross-checked
against an independent Python port of the same formula. The fixture is a
4×4 canvas, vertically uniform, split into a bright half
(`(200, 200, 200, 255)` for `x < 2`) and a dark half (`(50, 50, 50, 255)`
for `x ≥ 2`) — with R = G = B everywhere, the luma buffer equals the
input exactly, and Sobel's hand-computable magnitude comes out clean:
`255` (clamped) at the two boundary columns, `0` at the two outer
columns, since the outer columns' neighbourhoods are entirely flat once
edge-clamping is accounted for. At pencil width 1 (no dilation), full
pressure, and black paper, the two edge columns pass their own colour
straight through and the two flat columns become pure paper (black); at
width 2, the dilation radius reaches every column from at least one
boundary, so every pixel passes its own colour through unchanged
regardless of paper colour; at partial pressure (`5/15`) and paper
brightness `30` (an exact grey of `153`, no rounding needed), the two
edge columns blend a third of the way from paper to their own colour —
`200 × 1/3 + 153 × 2/3 = 168.667 → 169` and `50 × 1/3 + 153 × 2/3 =
118.667 → 119` — both comfortably clear of a `.5` rounding boundary. All
three matched the Python reference exactly on the first run. A fourth
test confines the fixture to a one-pixel selection at a flat column
(picked specifically because its pencil-width-1 output, `0`, differs
from its `200` input, avoiding the coincidental-no-op pitfall a
same-shaped selection test would otherwise risk); a fifth confirms an
out-of-range pencil width/stroke pressure/paper brightness and a
locked/unknown layer all error. Live interactive verification under
Xvfb was not attempted this phase: this session's Xvfb instance was
already confirmed, during the previous phase, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely (verified via
a control test against an unrelated, pre-existing toolbar button, and
confirmed to persist across a full Xvfb-and-application restart), so a
repeat attempt was judged unlikely to yield new information and the
dialog's wiring was instead reviewed by hand (state, the command call,
and the `runCommand("colored_pencil", …)` parameter names matching the
Tauri command's own camelCase-converted argument names exactly). This
carries forward the same documented gap from Phase 52 rather than
re-litigating it, on top of test/build coverage that is otherwise
complete.

**488 Rust tests total** (483 → 488, 481 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 54 — Filter Gallery > Artistic > Cutout

Simplifies the layer into broad, flat-coloured areas by composing two
already-existing operations rather than a real segmentation into
cut-paper shapes, which is a documented approximation, not a port.
`Document::cutout(id, levels, edge_simplicity)` runs a [`box_blur_at`]
pre-pass to erase fine detail — `edge_simplicity` (Photoshop's own
0..=10 range) is used directly as its box radius, so 0 skips the blur
entirely and 10 heavily simplifies detail before quantizing — and then
applies the exact same quantization step `Self::posterize` already uses,
to `levels` (Photoshop's own 2..=8 range) values per channel. Alpha is
untouched (the blur's own alpha average is discarded in favour of the
original, matching `posterize`'s own convention). Photoshop's separate
Edge Fidelity slider, which tunes how closely a real cutout follows
actual edges, is a documented scope cut with no equivalent here.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python port of the same blur-then-quantize
formula. The primary fixture is the existing `ramped_3x3` helper (a
pure-red 10..90 ramp): at its centre pixel, a radius-1 box blur's 3×3
neighbourhood covers the whole grid with no edge-clamp duplication, so
the red average is the plain mean, `450 / 9 = 50` exactly (integer
division, no remainder); at 5 levels the quantization step is `255 / 4 =
63.75`, and `50 / 63.75 = 0.7843` rounds to `1`, which times the step
rounds to `64` — comfortably clear of a `.5` boundary at both rounding
steps. A second test confirms edge simplicity `0` skips the blur
entirely, reproducing `posterize`'s own quantization directly (corner
value `90` also quantizes to `64` by the same arithmetic, value `10` to
`0`). A third confirms a uniform flat layer is unchanged by the
blurring step and then quantizes as a whole (a flat `128` at 2 levels,
step `255`, unambiguously rounds up to `255`), and separately confines
the ramp fixture to a one-pixel selection at the same centre pixel used
in the first test (chosen because its output, `64`, differs from its
input, `50`, avoiding the coincidental-no-op pitfall). A fourth confirms
out-of-range levels/edge-simplicity and a locked/unknown layer all
error. All four passed on the first run, matching the Python reference
exactly. A new **Cutout…** dialog exposes Number of Levels and Edge
Simplicity sliders, added after Colored Pencil in the same new Artistic
filter group.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as Phase 53: this session's Xvfb instance has been
confirmed, through a control test and a full Xvfb-and-application
restart in Phase 52, to have stopped delivering synthetic `xdotool`
pointer clicks to the webview entirely, and re-running that same
diagnostic a third time was judged unlikely to produce new information.
The dialog's wiring was reviewed by hand instead. This carries forward
the same documented gap rather than re-litigating it, on top of
test/build coverage that is otherwise complete.

**492 Rust tests total** (488 → 492, 485 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 55 — Filter Gallery > Artistic > Dry Brush

A documented approximation of Photoshop's real dry-brush painting
simulation, built from a [`median_at`] smoothing pass — the same
edge-preserving smoothing `Self::median` already uses, which erases fine
texture while keeping sharp boundaries intact, unlike a plain blur —
blended back toward the original per pixel. `Document::dry_brush(id,
brush_size, brush_detail)` uses `brush_size` (Photoshop's own 0..=10
range) directly as the median radius (0 skips smoothing entirely) and
`brush_detail` (Photoshop's own 0..=10 range) as how much of the
original, unsmoothed pixel shows back through: each channel becomes
`smoothed · (1 − detail) + original · detail`, where `detail =
brush_detail / 10`, so 0 is the median result untouched and 10 restores
the original exactly. Photoshop's separate Texture slider (a canvas-grain
overlay) is a documented scope cut with no equivalent here. Alpha is
untouched.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python port of the same formula. Reuses the
existing `ramped_3x3` fixture, at its corner `(0, 0)`: a radius-1 median's
clamped 3×3 neighbourhood samples red values `[10, 10, 10, 10, 20, 20,
40, 40, 50]` (edge-clamping duplicates the corner's own `10` four times,
`20` twice, `40` once, plus the diagonal neighbour `50` once); sorted,
the middle value (5th of 9) is `20` — deliberately different from the
corner's own original value, `10`, which is what makes this a meaningful
blend test rather than a coincidental no-op. At detail `0` the output is
the median untouched, `20`; at detail `1.0` it's the original exactly,
`10` (both endpoints exact, no rounding needed); at detail `0.4` it's
`20 × 0.6 + 10 × 0.4 = 16.0` exactly, again with no rounding ambiguity.
All three matched the Python reference exactly on the first run. A
second test confirms brush size `0` is a byte-for-byte no-op on the
whole layer (smoothing skipped entirely). A third confines the same
corner case to a one-pixel selection. A fourth checks that an
out-of-range brush size or detail and a locked/unknown layer all error.
A new **Dry Brush…** dialog exposes Brush Size and Brush Detail sliders,
added after Cutout in the same Artistic filter group.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous two: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**496 Rust tests total** (492 → 496, 489 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 56 — Filter Gallery > Artistic > Film Grain

Monochromatic seeded noise — one [`XorShift32`] draw per pixel, added
equally to all three channels, the same generator `Self::add_noise`
already uses — that fades out toward brighter pixels, the way real
photographic grain reads as more visible in shadows and midtones than in
highlights. A documented, simplified approximation of Photoshop's own
tonal-weighting curve, not a port of its exact shape.
`Document::film_grain(id, grain, highlight_area, intensity, seed)`:
`grain` and `intensity` (Photoshop's own 0..=20 and 0..=10 ranges) both
scale the noise's raw amplitude as fractions of `255`; `highlight_area`
(0..=20) scales how strongly each pixel's own ITU-R BT.601 luma
suppresses it, via `weight = 1 − luma · (highlight_area / 20)`, clamped
to `0..=1` — `0` applies grain uniformly regardless of brightness, `20`
fades it to nothing on a pure-white pixel while leaving black pixels at
full strength. The frontend sends a fresh `seed` on every apply, as with
Add Noise.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python port of the same `XorShift32` generator —
carefully re-implemented with explicit 32-bit float rounding after every
operation (via `struct.pack`/`unpack` round-tripping), since a naive
double-precision port would silently drift from Rust's actual `f32`
arithmetic. On `grey_2x2` (a flat `128` everywhere), with highlight area
`0` (weight `1` uniformly) and grain/intensity `10`/`10` (amplitude
`127.5`), seed `1`'s first four draws — `-0.99987`, `-0.96851`,
`+0.23281`, `-0.85676` — give final values `1`, `5`, `158`, and `19`,
none near a `.5` boundary. A second test isolates the highlight
weighting on a single white pixel: at highlight area `0` the first seeded
draw applies in full (`255 → 204`); at highlight area `20` the same draw
contributes exactly zero and the pixel stays untouched at `255` — a
clean before/after demonstration of the suppression term. A third
confines the flat fixture to a one-pixel selection. A fourth confirms
out-of-range grain/highlight-area/intensity and a locked/unknown layer
all error. All four passed on the first run, matching the Python
reference exactly. A new **Film Grain…** dialog exposes Grain, Highlight
Area, and Intensity sliders, added after Dry Brush in the same Artistic
filter group.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous three: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**500 Rust tests total** (496 → 500, 493 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 57 — Filter Gallery > Artistic > Neon Glow

A documented, hand-verifiable approximation of Photoshop's real Neon
Glow, which reworks the whole image's tones around a glow colour —
instead, each pixel is pulled toward a chosen colour in proportion to
its own edge strength, leaving flat areas exactly as they were and
letting only the glow's own colour bloom around detail.
`Document::neon_glow(id, glow_size, glow_brightness, color)` reuses the
same [`sobel_at`] edge detector `find_edges` already uses over the
layer's own luma, widened by the same [`extreme_at`]
neighbourhood-maximum `colored_pencil`'s own `pencil_width` already
uses, at radius `glow_size` (a documented simplification of Photoshop's
own `-24..=24` range — which also supports an inward variant this
project doesn't model — down to `0..=24`). `glow_brightness`
(Photoshop's own `0..=50` range) scales how far each pixel travels
toward `color`: `strength = (widened_edge / 255) · (glow_brightness /
50)`, clamped to `0..=1`, and every colour channel becomes `orig +
(color − orig) · strength`, rounded and clamped. Alpha is untouched. A
new **Neon Glow…** dialog exposes Glow Size and Glow Brightness sliders
plus a colour picker.

**Verified two ways.** Five new `document.rs` tests, cross-checked
against an independent Python port of the same formula, reusing the same
bright/dark split 4×4 fixture `colored_pencil`'s own tests already
established (columns 0-1 solid `(200, 200, 200, 255)`, columns 2-3 solid
`(50, 50, 50, 255)`, giving a clean Sobel magnitude map of `[0, 255,
255, 0]` across every row). At size `0` and full brightness (`50`), a
green glow colour fully replaces both edge columns regardless of their
own shade, while the two flat columns are left untouched. At a partial
brightness (`20`, strength factor `0.4`), every resulting channel value
lands on an exact integer with no rounding at all — e.g. column 1's
green channel is `200 + (255 − 200) × 0.4 = 222`. At size `1` (a
radius-1 dilation), the same reasoning as `colored_pencil`'s own
width-dilation test spreads both boundary columns across all four
columns, so every pixel becomes the glow colour at full brightness. A
fourth test confines the fixture to a one-pixel selection; a fifth
confirms an out-of-range glow size or brightness and a locked/unknown
layer all error. All five passed on the first run, matching the Python
reference exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous four: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**505 Rust tests total** (500 → 505, 498 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 58 — Filter Gallery > Artistic > Poster Edges

Composes two operations this project already has, rather than a new
low-level algorithm: `Self::posterize` to flatten colour into `levels`
(Photoshop's own `2..=6` range for this filter, narrower than standalone
Posterize's own dialog) bands, then a dark outline drawn wherever the
*posterized* result itself has a strong edge. `Document::poster_edges(id,
edge_thickness, edge_intensity, levels)` measures that outline with the
same [`sobel_at`] detector `find_edges` uses, widened by the same
[`extreme_at`] neighbourhood-maximum `colored_pencil`'s own
`pencil_width` already uses, at radius `edge_thickness` (Photoshop's own
`0..=10` range). `edge_intensity` (`0..=10`) scales how dark the outline
gets: every colour channel is multiplied by `1 − (widened_edge / 255) ·
(edge_intensity / 10)`, so a flat, edge-free area is left exactly as
posterize left it, and a fully-edged pixel at maximum intensity goes to
black. Alpha is untouched. Both the `posterize` pre-pass and the
darkening pass independently respect the selection, so a pixel outside
it is left completely untouched by either step — but because the edge
map is measured on the *mixed* result when only part of the layer is
selected (some pixels posterized, some not), a partial-selection
application can draw outline pixels along the selection's own boundary
in addition to the image's real edges, a documented consequence of
composing the two this way rather than a bug. A new **Poster Edges…**
dialog exposes Edge Thickness, Edge Intensity, and Posterization
sliders.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python script, reusing the same bright/dark split
4×4 fixture `colored_pencil`/`neon_glow`'s own tests already established
(columns 0-1 solid `(200, 200, 200, 255)`, columns 2-3 solid `(50, 50,
50, 255)`). At 6 posterization levels (step `51`), `200` quantizes to
`204` and `50` quantizes to `51`; the posterized values' own Sobel
magnitude map is `[0, 255, 255, 0]` across every row, the same shape as
before just measured on the new quantized colours. At thickness `0`,
intensity `10` (darken factor `1.0`): the two flat columns are left
exactly at posterize's own output (`204`, `51`), while the two
full-magnitude edge columns go fully black. At intensity `6` (darken
factor `0.6`) the same edge columns instead dim to `204 × 0.4 = 81.6 →
82` and `51 × 0.4 = 20.4 → 20`, both comfortably clear of a `.5`
boundary. A second test confirms thickness `1` (a radius-1 dilation)
spreads both boundary columns across every column, so at full intensity
every pixel goes black regardless of its own posterized shade. A third
test — which needed a mid-design correction after an initial wrong
assumption that `posterize` always applies to the whole layer regardless
of selection (it doesn't; like the standalone Posterize adjustment, it's
built on `adjust_layer_pixels`, which does respect the selection) —
selects an entire column and confirms the unselected columns are left at
their **raw, unposterized** original values (`200`/`50`, not `204`/`51`)
while the selected column is posterized and then fully darkened by its
own full-magnitude edge. A fourth test confirms an out-of-range
thickness/intensity/posterization and a locked/unknown layer all error.
All four passed on the first run, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous five: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**509 Rust tests total** (505 → 509, 502 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 59 — Filter Gallery > Artistic > Sponge

Reuses `crystallize`'s own jittered-Voronoi machinery — each pixel takes
its nearest jittered site's averaged colour, the same mottled-blotch
shape `crystallize`'s crystals already have — and pushes each blotch's
colour away from its own luma, boosting saturation the way a sponge's
uneven paint coverage reads as patches of richer colour. A documented
approximation, not a port of Photoshop's own algorithm, which
additionally reshapes the blotches' edges by a Smoothness slider this
project doesn't model (a documented scope cut).
`Document::sponge(id, brush_size, definition, seed)`: `brush_size`
(Photoshop's own `0..=10` range) maps to a Voronoi cell size of
`brush_size + 1` pixels; `definition` (Photoshop's own `0..=25` range)
sets the saturation multiplier, `1 + definition / 25`, so `0` reproduces
`crystallize`'s own output exactly (the multiplier is `1`, an algebraic
identity) and `25` doubles each channel's distance from the blotch's
luma. Alpha, like `crystallize`, is the blotch's own averaged alpha, not
the per-pixel original. The frontend sends a fresh `seed` on every
apply, as with Crystallize. A new **Sponge…** dialog exposes Brush Size
and Definition sliders.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python script. The first reuses `crystallize`'s
own already-verified fixture exactly — a 6×6 `ramp_square` (red = `10x +
y`), cell size `3` (`brush_size` `2`), seed `1` — and confirms `sponge`
at definition `0` reproduces `crystallize`'s own four region averages
(`7`, `35`, `19`, `49`) byte-for-byte, since the saturation step is a
no-op at that setting. A second, dedicated test isolates the saturation
math on a flat `2×2` swatch `(180, 90, 30, 255)` (one cell covers the
whole canvas, so its average is the colour itself): at definition `10`
(multiplier `1.4`) and luma `110.07`, red becomes `110.07 + (180 −
110.07) × 1.4 = 207.972 → 208`, green becomes `110.07 + (90 − 110.07) ×
1.4 = 81.972 → 82`, and blue becomes `110.07 + (30 − 110.07) × 1.4 =
−2.028`, clamping to `0` — none of these land near a `.5` boundary. A
third test confines the ramp fixture to a one-pixel selection, reusing
`crystallize`'s own selection test's reasoning (the site-averaging pass
always sees the whole layer, so the touched pixel gets the same average
it would without a selection; only it is written). A fourth confirms an
out-of-range brush size or definition and a locked/unknown layer all
error. All four passed on the first run, matching the Python reference
exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous six: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**513 Rust tests total** (509 → 513, 506 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 60 — Filter Gallery > Artistic > Watercolor

Simplifies detail with the same edge-preserving [`median_at`] smoothing
`dry_brush` already uses, then darkens each pixel in proportion to how
dark it already is — the pooled-pigment look of watercolour paint, which
settles darkest in the shadows and stays washed-out and pale in the
highlights. A documented approximation, not a port of Photoshop's own
algorithm (which also lays down a canvas texture this project doesn't
model, a documented scope cut). `Document::watercolor(id, brush_detail,
shadow_intensity)`: `brush_detail` (Photoshop's own `1..=14` range) is
inverted into a median radius, `15 − brush_detail`, so a high Brush
Detail (more of the original preserved) gives a small radius and a low
one gives heavy smoothing. `shadow_intensity` (Photoshop's own `0..=10`
range) scales a self-referential darkening term: `factor = 1 −
(shadow_intensity / 10) · (1 − luma / 255)`, using the *smoothed*
pixel's own ITU-R BT.601 luma, so a bright pixel keeps nearly all its
value while a dark one is pulled further toward black. Alpha is
untouched. A new **Watercolor…** dialog exposes Brush Detail and Shadow
Intensity sliders.

**Verified two ways.** Three new `document.rs` tests, cross-checked
against an independent Python script. Brush detail `14` gives radius `1`
— the same radius `dry_brush`'s own corner test already used on the
`ramped_3x3` fixture — so its three already-relevant points are
hand-computable: the clamped corner `(0, 0)` has median `20`, the centre
`(1, 1)` (whose whole 3×3 neighbourhood is in range, no clamp
duplication) has median `50`, and the bottom-right `(2, 2)` has median
`80`. At shadow intensity `0` (factor `1.0` everywhere) the output is
exactly the median, unchanged. At shadow intensity `5`, using each
point's own smoothed luma (`0.299 ×` the median, since green/blue are
flat `0` throughout this fixture): the corner's luma `5.98` gives factor
`0.5117 → 20 × 0.5117 = 10.235 → 10`; the centre's luma `14.95` gives
factor `0.5293 → 50 × 0.5293 = 26.466 → 26`; the bottom-right's luma
`23.92` gives factor `0.5469 → 80 × 0.5469 = 43.752 → 44` — none of
these land near a `.5` boundary. A second test confines the fixture to a
one-pixel selection. A third confirms an out-of-range brush detail or
shadow intensity and a locked/unknown layer all error. All three passed
on the first run, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous seven: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**516 Rust tests total** (513 → 516, 509 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 61 — Filter Gallery > Brush Strokes > Dark Strokes

The first Brush Strokes gallery filter, and a documented approximation
of Photoshop's real directional-stroke renderer — this is a per-pixel
luma-threshold split-tone instead, not a port — that pulls dark pixels
further toward black and light ones further toward white, the same
"widen the tonal spread" effect a hand-inked drawing's dark
strokes-on-light-strokes contrast produces. `Document::dark_strokes(id,
balance, black_intensity, white_intensity)`: `balance` (Photoshop's own
`0..=10` range) sets the luma split point, `threshold = balance / 10 ·
255`. A pixel whose own ITU-R BT.601 luma sits below `threshold` is
darkened: `t = (threshold − luma) / threshold` scaled by
`black_intensity` (`0..=10`) into a multiplier, `orig · (1 −
black_intensity / 10 · t)`. A pixel at or above `threshold` is instead
pulled toward white: `t = (luma − threshold) / (255 − threshold)` scaled
by `white_intensity` (`0..=10`) into `orig + (255 − orig) ·
(white_intensity / 10 · t)`. `balance` at either extreme (`0` or `10`)
puts every real pixel on one side of the split, degenerately turning off
the other intensity slider — a natural consequence of the formula, not
a special case. Alpha is untouched. A new **Dark Strokes…** dialog
exposes Balance, Black Intensity, and White Intensity sliders.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python script. On the `ramped_3x3` fixture (red
values `10`-`90`, green/blue flat `0`, so luma is `0.299 ×` red and
always tiny — at most `26.91`): balance `0` (threshold `0`) puts every
pixel at or above the threshold, so at white intensity `6` red `10`
(luma `2.99`) becomes `(12, 2, 2)`, red `50` (luma `14.95`) becomes
`(57, 9, 9)`, and red `90` (luma `26.91`) becomes `(100, 16, 16)`;
balance `10` (threshold `255`) puts every pixel below the threshold, so
at black intensity `6` the same three reds become `4`, `22`, and `42`
(green/blue stay `0` under multiplication regardless of the factor) —
none of these land near a `.5` boundary. A third test confines a flat
`128` grey to a one-pixel selection at balance `10`/black intensity `6`:
its luma is exactly `128`, giving `t = 0.498039`, factor `0.701176`, and
`128 × 0.701176 = 89.75 → 90` — a real, hand-verified change, not a
coincidental no-op, with the rest of the layer confirmed untouched. A
fourth confirms an out-of-range balance/black-intensity/white-intensity
and a locked/unknown layer all error. All four passed on the first run,
matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous eight: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**520 Rust tests total** (516 → 520, 513 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 62 — Filter Gallery > Brush Strokes > Ink Outlines

Pushes each pixel toward black in proportion to its own edge strength
and toward white in proportion to how *flat* it is, drawing dark ink
lines along detail while washing out everything in between — the same
[`sobel_at`]/[`extreme_at`] edge-and-dilate machinery
`colored_pencil`/`neon_glow`/`poster_edges` already use, combined into a
genuinely two-sided push (unlike `dark_strokes`'s luma threshold, the
split here is driven entirely by edge strength). A documented
approximation, not a port of Photoshop's own directional-stroke
renderer. `Document::ink_outlines(id, stroke_length, dark_intensity,
light_intensity)`: `stroke_length` (Photoshop's own `1..=50` range) is
scaled down into a dilation radius, `(stroke_length − 1) / 10`
(`0..=4`), since a literal 1:1 mapping onto `extreme_at`'s own
O(radius²) search would be needlessly slow at Photoshop's full range —
a documented scope simplification, not a faithful unit conversion.
`dark_intensity` and `light_intensity` (Photoshop's own `0..=50` ranges)
each scale their own side of the split: every colour channel becomes
`orig − orig · (dark_intensity / 50) · edge + (255 − orig) ·
(light_intensity / 50) · (1 − edge)`, where `edge` is the widened Sobel
magnitude over `255`. Alpha is untouched. A new **Ink Outlines…** dialog
exposes Stroke Length, Dark Intensity, and Light Intensity sliders.

**Verified two ways.** Four new `document.rs` tests, cross-checked
against an independent Python script, reusing the same bright/dark split
4×4 fixture `colored_pencil`/`neon_glow`/`poster_edges`'s own tests
already established (Sobel magnitude map `[0, 255, 255, 0]` across every
row). At stroke length `1` (radius `0`), dark intensity `50`, light
intensity `0`: the flat columns are untouched (`200`, `50`) and the edge
columns go fully black (`orig − orig × 1.0 × 1.0 = 0`) regardless of
their own shade. At dark intensity `0`, light intensity `50`: the flat
columns fully lighten to `255` and the edge columns stay untouched (`1 −
edge = 0` there). At dark intensity `25` (factor `0.5`): the edge
columns dim by exactly half, `200 × 0.5 = 100` and `50 × 0.5 = 25`,
clean integers with no rounding needed. A second test confirms stroke
length `11` (radius `1`) dilates the edge map across every column, so
every pixel goes black at full dark intensity. A third confines the
fixture to a one-pixel selection. A fourth confirms an out-of-range
parameter and a locked/unknown layer all error. All four passed on the
first run, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous nine: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**524 Rust tests total** (520 → 524, 517 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 63 — Filter Gallery > Brush Strokes > Spatter

Generalises `diffuse`'s own random-neighbour pick (its `Normal` mode
draws one uniformly random offset in `-1..=1` on each axis) into a
wider, seeded scatter radius, then averages several such draws per
pixel instead of keeping only one — more draws pull the average back
toward the local neighbourhood's own colour, reading as a smoother
spray rather than Diffuse's single-sample jitter. A documented
approximation of Photoshop's real spray-paint renderer, not a port.
`Document::spatter(id, spray_radius, smoothness, seed)`: `spray_radius`
(Photoshop's own `0..=25` range) is the scatter radius each draw's
`(dx, dy)` offset is drawn uniformly from (`0` makes every draw the
pixel itself, a no-op); `smoothness` (Photoshop's own `1..=15` range) is
literally how many such draws are averaged together per pixel — at `1`
this is exactly `diffuse`'s own `Normal` mode when `spray_radius` is
`1`, an algebraic identity, not a coincidence. Each draw is
edge-clamped, matching every other neighbourhood operation in this
file. The frontend sends a fresh `seed` on every apply, as with
Diffuse. A new **Spatter…** dialog exposes Spray Radius and Smoothness
sliders.

**Verified two ways.** Five new `document.rs` tests, cross-checked
against an independent Python port of the same `XorShift32` generator.
At spray radius `1`, smoothness `1`, seed `1` on the `ramped_3x3`
fixture, the output matches `diffuse`'s own already-verified `Normal`
mode output exactly (`[10, 40, 30, 10, 60, 80, 70, 60, 60]`), confirming
the algebraic identity rather than merely asserting it. A second test
raises smoothness to `2`: pixel `(0, 0)` consumes seed `1`'s first four
draws as two `(dx, dy)` pairs — the first clamps to the pixel's own
position (`10`), the second clamps to `(0, 1)` (`40`) — averaging to
`25.0` exactly. A third confirms spray radius `0` is a byte-for-byte
no-op (every draw resolves to the pixel itself regardless of
smoothness). A fourth confines the fixture to a one-pixel selection —
which, since `filter_pixels` skips the seeded draw entirely for
unselected pixels, makes the *selected* pixel the first to consume the
generator's own draws, landing on `10` rather than its own original
value `20`, a real change worked out by hand rather than assumed. A
fifth confirms an out-of-range spray radius or smoothness and a
locked/unknown layer all error. All five passed on the first run,
matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this phase,
for the same reason as the previous ten: this session's Xvfb instance
was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**529 Rust tests total** (524 → 529, 522 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 64 — Filter Gallery > Brush Strokes > Crosshatch

Builds Photoshop's Crosshatch as a repeated pass of two crossing
diagonal directional blurs. Each pass reuses `motion_blur_at` (the
same directional line-sampling helper `crystallize`'s neighbours and
this project's other directional filters share) twice per pixel, once
along each 45° diagonal (`(±1, ±1)` normalised by `FRAC_1_SQRT_2`), and
keeps the darker of the two per channel — the crossing strokes read as
hatching precisely because a bright spike gets pulled down by
whichever diagonal line happens to run through it, while the
diagonal that misses it stays untouched and wins the `min`.
`Document::crosshatch(id, stroke_length, sharpness, strength)`:
`stroke_length` (Photoshop's own `3..=50` range) sets the blur
half-length as `(stroke_length / 10).max(1)`; `strength` (Photoshop's
own `1..=3` range) repeats the whole crossing-diagonal pass that many
times, darkening further with each repetition since the previous
pass's own hatching becomes the next pass's input; `sharpness`
(Photoshop's own `0..=20` range) blends the fully-hatched result back
toward the untouched original by `sharpness / 20`, at `0` giving pure
hatching and at `20` giving back the original unchanged. A documented
approximation of Photoshop's real crosshatch-brush renderer, not a
port. The frontend's new **Crosshatch…** dialog exposes Stroke Length,
Sharpness, and Strength sliders.

**Verified two ways.** Five new `document.rs` tests, cross-checked by
hand and against an independent Python script. A dedicated 3×3 "spike"
fixture (flat grey `50` everywhere except a bright `200` at the
bottom-right corner) was chosen specifically so the two crossing
diagonals disagree, making the min-of-two combination meaningfully
testable — the more obvious `ramped_3x3` fixture was rejected because
its linear ramp makes both diagonals average to the same value
everywhere, never exercising the `min`. At stroke length `3` (half
`1`), sharpness `0`, strength `1`: the centre pixel's "\" diagonal
averages `(50+50+200)/3 = 100` while its "/" diagonal averages
`(50+50+50)/3 = 50` exactly, so `min(100, 50) = 50` leaves the centre
untouched by the spike; the spike corner itself (edge-clamped) sees
"\" average `(50+200+200)/3 = 150` against "/"'s `(50+200+50)/3 = 100`,
so `min(150, 100) = 100` — the corner darkens from `200` to `100`
exactly. A second test raises strength to `2`: the second pass runs
the same combination over the first pass's own output (corner now
`100`), giving "\" `(50+100+100)/3 = 83` against "/" `(50+100+50)/3 =
66`, so `min(83, 66) = 66`, confirming each pass compounds on the
last. A third keeps strength `1` but raises sharpness to `10` (blend
factor `0.5`): the single-pass hatched value `100` blends with the
original `200` exactly halfway to `150.0`, needing no rounding. A
fourth confines the fixture to a one-pixel selection covering only the
spike corner and confirms it still darkens to `100` while every
unselected pixel stays byte-for-byte at its original value. A fifth
confirms out-of-range stroke length, sharpness, and strength, plus a
locked/unknown layer, all error. All five passed on the first run,
matching the hand/Python-computed values exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eleven: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**534 Rust tests total** (529 → 534, 527 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 65 — Filter Gallery > Brush Strokes > Accented Edges

Highlights edges with a colour that can run from black ink to a bright,
light-struck white, reusing the same luma/Sobel/dilation pipeline
`ink_outlines` and `poster_edges` already share, plus an extra
`box_blur_at` smoothing pass over the edge map itself — the same
box-blur helper this project's other smoothing filters already use. A
documented approximation of Photoshop's real brush-accented edge
renderer, not a port. `Document::accented_edges(id, edge_width,
edge_brightness, smoothness)`: `edge_width` (Photoshop's own `1..=14`
range) dilates the measured Sobel edge map by `edge_width - 1`, so `1`
leaves it exactly as measured; `smoothness` (Photoshop's own `0..=15`
range) then box-blurs that (possibly dilated) edge map by the same
radius, softening the hard boundary between edge and non-edge before
it is used; `edge_brightness` (Photoshop's own `0..=50` range) picks
the colour edges are painted, linearly from black at `0` to white at
`50` (`255 * edge_brightness / 50`), and every pixel blends toward
that colour in proportion to its own (dilated, smoothed) edge
strength: `orig * (1 - e) + edge_colour * e`. Alpha is carried over
unchanged. Only the final blend respects the selection; the
edge-detection, dilation, and smoothing passes always see the whole
layer, the same scope cut `ink_outlines` and `poster_edges` already
make. A new **Accented Edges…** dialog exposes Edge Width, Edge
Brightness, and Smoothness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `colored_pencil`/`neon_glow`/`poster_edges`/
`ink_outlines` all already share (4×4, columns 0-1 solid `200`,
columns 2-3 solid `50`, Sobel magnitude `[0, 255, 255, 0]` across every
row), cross-checked against an independent Python script emulating
`f32` arithmetic exactly via `struct.pack`/`unpack` round-tripping. At
edge width `1` (no dilation) and smoothness `0` (no blur), brightness
`0` leaves the flat columns untouched (`200`, `50`) and drives the
full-magnitude edge columns fully to black (`0`); brightness `50`
leaves the flat columns untouched and drives the edge columns fully to
white (`255`); brightness `25` (edge colour `127.5`) blends the edge
columns fully to `127.5`, which both Rust's round-half-away-from-zero
and Python's round-half-to-even agree rounds to `128` (the nearer even
integer either way, so the two rounding rules happen to coincide here
rather than disagree). A second test raises edge width to `2`
(dilation radius `1`), spreading full edge strength across every
column of the 4-wide fixture, so at brightness `0` every pixel goes
fully black regardless of its own shade — the same dilation reasoning
`ink_outlines`'s own stroke-length test already established. A third
raises smoothness to `1` (box-blur radius `1`) with no dilation: since
the fixture is vertically uniform, the 3×3 blur window reduces to a
horizontal average of three columns each counted three times out of
nine samples, giving smoothed edge values of exactly `85`, `170`,
`170`, `85` (`765/9` and `1530/9`, both dividing evenly, so
`box_blur_at`'s integer truncating division introduces no rounding
ambiguity) — blending toward black at brightness `0` then gives
`133`, `67`, `17`, `33`, each hand-computed as a clean one-third or
two-thirds fraction of the original shade. A fourth confines the
fixture to a one-pixel selection and confirms only that pixel changes.
A fifth confirms out-of-range edge width, brightness, and smoothness,
plus a locked/unknown layer, all error. All five passed on the first
run, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twelve: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**539 Rust tests total** (534 → 539, 532 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 66 — Filter Gallery > Brush Strokes > Angled Strokes

Repaints each pixel with one of two diagonal `motion_blur_at` passes —
the same "\" and "/" strokes `crosshatch` already computes — chosen by
the *original* pixel's own luma against a threshold, rather than
combined by taking the darker of the two. `Document::angled_strokes(id,
direction_balance, stroke_length, sharpness)`: `direction_balance`
(Photoshop's own `0..=100` range) sets that threshold as `255 *
direction_balance / 100`: light pixels (luma at or above the threshold)
are painted with the "\" stroke, dark pixels with the "/" stroke, so
raising the balance shifts more of the image into the "/" camp — this
models Photoshop's own behaviour of angling strokes one way through
light areas and the other way through dark ones. `stroke_length`
(Photoshop's own `3..=50` range) scales down into each diagonal's own
half-length the same way `crosshatch`'s own stroke length does,
`(stroke_length / 10).max(1)`. `sharpness` (Photoshop's own `0..=10`
range) blends the chosen stroke back toward the original pixel, `orig *
(sharpness / 10) + stroke * (1 - sharpness / 10)`, the same blend-back
shape `crosshatch`'s own sharpness uses over its own range. A documented
approximation, not a port of Photoshop's real direction-aware renderer.
A new **Angled Strokes…** dialog exposes Direction Balance, Stroke
Length, and Sharpness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing
`crosshatch`'s own "spike" fixture (3×3, flat grey `50` except a bright
`200` at the bottom-right corner) and its already-verified diagonal
averages at stroke length `3` (half `1`): the centre `(1,1)` is `100`
along "\" and `50` along "/"; the spike corner `(2,2)` is `150` along
"\" and `100` along "/". At direction balance `50` (threshold `127.5`)
and sharpness `0`: the centre's own luma is `50`, below the threshold,
so it is painted with "/" (`50`) — unchanged from its original value;
the corner's own luma is `200`, at or above the threshold, so it is
painted with "\" (`150`) instead of "/" (`100`), a real,
direction-dependent change worked out by hand. A second test raises
direction balance to `90` (threshold `229.5`), now above the corner's
own luma of `200`, flipping it onto the "/" side and landing on `100`
instead of `150` — confirming the threshold actually moves. A third
keeps balance `50` but raises sharpness to `5` (blend factor `0.5`):
the corner's chosen stroke value `150` blends with the original `200`
exactly halfway to `175.0`, needing no rounding. A fourth confines the
fixture to a one-pixel selection covering the spike corner and confirms
only that pixel changes. A fifth confirms out-of-range direction
balance, stroke length, and sharpness, plus a locked/unknown layer, all
error. All five passed on the first run, matching the hand-computed
values exactly — no independent Python script was needed since every
value here reuses `crosshatch`'s own already Python-cross-checked
diagonal averages, and the remaining arithmetic (a threshold compare
and a linear blend) is simple enough to verify directly by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirteen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**544 Rust tests total** (539 → 544, 537 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 67 — Filter Gallery > Brush Strokes > Sprayed Strokes

A separable approximation of a directional, rectangular brush stroke,
built from two `motion_blur_at` passes at right angles to each other —
the same directional line-sampling helper `motion_blur` and
`crosshatch` already use. The first pass streaks the whole layer along
the chosen direction's own axis; the second re-blurs that streaked
result along the *perpendicular* axis, thickening each streak into a
stroke with some width rather than a single-pixel-wide line. Two 1-D
passes at right angles approximate, rather than exactly reproduce, a
true 2-D rectangular average — a documented simplification, not a port
of Photoshop's own spray-brush renderer.
`Document::sprayed_strokes(id, stroke_length, spray_radius,
direction)`: `direction` (Photoshop's own four-way dropdown) selects
the stroke axis — `0` Right Diagonal, `1` Horizontal, `2` Left
Diagonal, `3` Vertical; `stroke_length` (Photoshop's own `0..=20`
range) becomes the first pass's half-length, `stroke_length / 2`;
`spray_radius` (Photoshop's own `0..=25` range) becomes the second
pass's half-length, `spray_radius / 5` — scaled down the same way
`ink_outlines` and `crosshatch` both scale their own length
parameters. A new **Sprayed Strokes…** dialog exposes Stroke Length
and Spray Radius sliders plus a Stroke Direction dropdown.

**Verified two ways.** Five new `document.rs` tests on the `ramped_3x3`
fixture this file's `motion_blur` tests already established (a 3×3 red
ramp, 10 through 90), reusing that fixture's own already-verified
motion-blur arithmetic directly rather than deriving fresh numbers. At
direction `1` (Horizontal), stroke length `2` (first-pass half-length
`1`): the first pass is exactly `motion_blur`'s own zero-degree,
radius-1 pass, so each row streaks to the same values that test already
established (row 0 to `[13, 20, 26]`, row 1 to `[43, 50, 56]`, row 2 to
`[73, 80, 86]`, every one an integer-truncating division like `(10 + 10
+ 20) / 3 = 13`); at spray radius `0` the second pass is a no-op, so
that streaked grid is the final output. A second test raises spray
radius to `5` (second-pass half-length `1`), blurring that same
streaked grid vertically: column `0` (`13, 43, 73`) averages
top-to-bottom to `(23, 43, 63)`, column `1` (`20, 50, 80`) to `(30, 50,
70)`, column `2` (`26, 56, 86`) to `(36, 56, 76)` — all nine divisions
come out exactly even, no rounding ambiguity. A third confirms
direction `3` (Vertical) selects the vertical axis instead, reusing
`motion_blur`'s own already-verified ninety-degree column values
(`20, 40, 60`) directly. A fourth confines the fixture to a one-pixel
selection at `(0, 0)`, whose combined two-pass value (`23`) differs
from its own untouched original (`10`), a real, hand-verified change.
A fifth confirms out-of-range stroke length, spray radius, and an
unrecognised direction, plus a locked/unknown layer, all error. All
five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fourteen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**549 Rust tests total** (544 → 549, 542 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 68 — Filter Gallery > Brush Strokes > Sumi-e

Widens dark ink strokes by eroding each colour channel toward its own
darkest neighbour — the same `extreme_at` neighbourhood-extreme helper
`ink_outlines` and `poster_edges` already use for dilation, just
asking for the minimum instead of the maximum — then reapplies
`brightness_contrast`'s own contrast formula to push the widened
strokes toward saturated black-on-white, the flat, high-contrast look
of a sumi-e ink wash. A documented approximation, not a port of
Photoshop's own brush-and-wash renderer. `Document::sumi_e(id,
stroke_width, stroke_pressure, contrast)`: `stroke_width` (Photoshop's
own `3..=15` range) scales down into the erosion radius, `(stroke_width
/ 5).max(1)`, for the same reason `ink_outlines` scales its own stroke
length down; `stroke_pressure` (Photoshop's own `0..=15` range) blends
that eroded result back with the original, so `0` leaves ink strokes at
their original width and `15` is full erosion; `contrast` (Photoshop's
own `0..=40` range) is rescaled onto `brightness_contrast`'s own
`-255..=255` domain and fed through its exact same formula, pulling
every channel away from mid-grey. Alpha is carried over unchanged. This
is the last capability in the Brush Strokes gallery — all eight of its
filters (Accented Edges, Angled Strokes, Crosshatch, Dark Strokes, Ink
Outlines, Spatter, Sprayed Strokes, and Sumi-e) now ship. A new
**Sumi-e…** dialog exposes Stroke Width, Stroke Pressure, and Contrast
sliders.

**Verified two ways.** Six new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges` all already share (4×4, columns 0-1 solid `200`,
columns 2-3 solid `50`). At stroke width `5` (erosion radius `1`), full
stroke pressure (`15`, an identity blend), and contrast `0` (mapped
contrast `0`, a `factor` of exactly `259 * 255 / (255 * 259) = 1.0`,
also an identity): column `0`'s radius-1 neighbourhood is `(200, 200,
200)`, staying `200`; column `1`'s is `(200, 200, 50)`, eroding to `50`
as ink spreads in from column `2`; columns `2` and `3` are already `50`
and stay `50`. A second test confirms stroke pressure `0` round-trips
the whole fixture to its own original values exactly, since the erosion
pass then contributes nothing to the blend. A third raises contrast to
`40` (maximum), rescaling to `brightness_contrast`'s own domain as
`255` and giving `factor = 259 * 510 / (255 * 4) = 129.5` exactly:
`129.5 * (200 - 128) + 128 = 9452`, clamped to `255`, and `129.5 * (50
- 128) + 128 = -9973`, clamped to `0` — both so far past their clamp
boundary that no rounding rule could change the outcome. A fourth
raises stroke width to `10` (erosion radius `2`), wide enough that
every column's neighbourhood on this 4-wide fixture reaches a
50-valued column, eroding the whole row to `50`. A fifth confines the
fixture to a one-pixel selection. A sixth confirms out-of-range stroke
width, stroke pressure, and contrast, plus a locked/unknown layer, all
error. All six passed on the first run — no independent Python script
was needed since every value here reuses `extreme_at`'s and
`brightness_contrast`'s own already-verified arithmetic directly, with
the remaining combination (a linear blend and a formula already proven
by `brightness_contrast`'s own tests) simple enough to verify by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifteen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**555 Rust tests total** (549 → 555, 548 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 69 — Filter Gallery > Artistic > Smudge Stick

Smudges detail along a single "\" diagonal using `motion_blur_at` — the
same directional line-sampling helper `motion_blur`, `crosshatch`, and
`sprayed_strokes` already use — then brightens whichever pixels land in
the smudged result's own upper tonal range, the way a blended pastel
stick both smears detail together and leaves a lighter sheen where it
passes over what were already light areas. A documented approximation,
not a port of Photoshop's own pastel-stroke renderer.
`Document::smudge_stick(id, stroke_length, highlight_area, intensity)`:
`stroke_length` (Photoshop's own `0..=10` range) is used directly as
the smudge's half-length, small enough not to need the scaling-down
this project's longer-range stroke parameters use; `highlight_area`
(Photoshop's own `0..=20` range) sets the smudged pixel's own luma
threshold above which brightening applies, `255 * (1 - highlight_area
/ 20)`, so `0` disables brightening entirely and `20` makes every pixel
eligible; `intensity` (Photoshop's own `0..=10` range) scales how far
an eligible pixel travels toward white in proportion to how far above
the threshold it already sits, the same white-pull shape
`dark_strokes`'s own highlight side already uses. Alpha is carried
through the same motion-blur average as the colour channels. A new
**Smudge Stick…** dialog exposes Stroke Length, Highlight Area, and
Intensity sliders.

**Verified two ways.** Four new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e` all already share (4×4, columns 0-1 solid
`200`, columns 2-3 solid `50`, vertically uniform so a diagonal smudge
lands on the same columns a horizontal one would). At stroke length `1`
(half `1`): column `0` averages `(200, 200, 200)` to `200`; column `1`
`(200, 200, 50)` to `450 / 3 = 150`; column `2` `(200, 50, 50)` to `300
/ 3 = 100`; column `3` `(50, 50, 50)` to `50` — every one an exact
integer division. Highlight area `0` makes the threshold exactly `255`,
unreachable on this fixture, so the smudged row passes through
unchanged regardless of intensity. A second test raises highlight area
to `10` (threshold `127.5`) with intensity `10` (factor `1.0`): column
`0`'s smudged luma `200` clears the threshold, `t = (200 - 127.5) /
127.5 = 0.568627...`, pushing it to `200 + 55 * 0.568627 = 231.27 ->
231`; column `1`'s `150` clears it too, `t = 0.176471...`, pushing `150
+ 105 * 0.176471 = 168.53 -> 169`; columns `2` and `3` (`100` and `50`)
both fall below the threshold and pass through unchanged — cross-checked
against an independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping. A third confines the fixture to
a one-pixel selection. A fourth confirms out-of-range stroke length,
highlight area, and intensity, plus a locked/unknown layer, all error.
All four passed on the first run, matching the Python reference
exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixteen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**559 Rust tests total** (555 → 559, 552 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 70 — Filter Gallery > Artistic > Paint Daubs

Softens the layer into round, soft-edged daubs with `box_blur_at` — the
same neighbourhood-average helper `box_blur` and this project's other
smoothing filters already use — then blends that softened result back
toward the original by `sharpness`, the same blend-back shape
`dry_brush` already uses for its own Brush Detail slider. A documented
approximation, not a port of Photoshop's own six brush-type renderers
(Simple, Light/Dark Rough, Wide Sharp/Blurry, Sparkle) — Photoshop's
own Brush Type dropdown is a documented scope cut, this filter always
daubs the way "Simple" does. `Document::paint_daubs(id, brush_size,
sharpness)`: `brush_size` (Photoshop's own `1..=50` range) scales down
into the blur radius, `(brush_size / 5).max(1)`, for the same reason
`ink_outlines` scales its own stroke length down; `sharpness`
(Photoshop's own `0..=40` range) blends the blurred daubs back with the
original, so `0` is the softest daub and `40` restores the original
untouched. Alpha untouched. A new **Paint Daubs…** dialog exposes Brush
Size and Sharpness sliders.

**Verified two ways.** Four new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick` all already share (4×4,
columns 0-1 solid `200`, columns 2-3 solid `50`, vertically uniform so
the 3×3 box-blur window at brush size `5` (radius `1`) reduces to a
horizontal 3-tap average): column `0` `(200, 200, 200)` averages to
`200`; column `1` `(200, 200, 50)` to `450 / 9 = 150` (9 samples, 3 per
column since all 3 rows agree); column `2` `(200, 50, 50)` to `300 / 9
= 100`; column `3` `(50, 50, 50)` to `150 / 9 = 50` — every one an
exact integer division. At sharpness `0` the output is exactly that
blurred row; at sharpness `40` the blur contributes nothing and the
output is exactly the untouched original; at sharpness `20` each
column blends its own blurred and original values exactly halfway
(column `1`'s `150`/`200` to `175.0`, column `2`'s `100`/`50` to
`75.0`), both exact with no rounding needed. A second test raises
brush size to `10` (radius `2`, a 5×5 window reducing to a horizontal
5-tap average): the row becomes `170`, `140`, `110`, `80`, again all
exact integer divisions. A third confines the fixture to a one-pixel
selection. A fourth confirms out-of-range brush size and sharpness,
plus a locked/unknown layer, all error. All four passed on the first
run — no independent Python script was needed since every division
here comes out exactly even by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventeen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**563 Rust tests total** (559 → 563, 556 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 71 — Filter Gallery > Artistic > Palette Knife

Composes two operations this project already has, the same way
`poster_edges` does — `posterize` flattens colour into broad, flat
bands first, then a `box_blur_at` pass rounds off the hard band
boundaries into the soft-edged, broad-stroke look of paint applied
with a palette knife. A documented approximation, not a port of
Photoshop's own segmentation-based renderer.
`Document::palette_knife(id, stroke_size, stroke_detail, softness)`:
`stroke_detail` (Photoshop's own `1..=3` range) maps directly onto
`posterize`'s own `levels` parameter as `stroke_detail + 2` (`3..=5`),
fewer levels reading as broader, simpler strokes; `stroke_size`
(Photoshop's own `1..=50` range) scales down into a blur radius,
`(stroke_size / 10).max(1)`, the same way `ink_outlines` scales its
own stroke length down; `softness` (Photoshop's own `0..=10` range)
adds `softness / 2` more to that same radius rather than being a
separate pass. Because `posterize` is itself built on
`adjust_layer_pixels`, it already respects the selection on its own —
the same selection nuance `poster_edges`'s own doc comment already
notes — so an unselected pixel is left at its raw, unposterized,
unblurred original value, never partially processed. A new **Palette
Knife…** dialog exposes Stroke Size, Stroke Detail, and Softness
sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs` all already
share (4×4, columns 0-1 solid `200`, columns 2-3 solid `50`). At
stroke detail `1` (3 posterize levels, step `127.5`): `200` quantizes
to `round(1.5686) * 127.5 = 2 * 127.5 = 255`, and `50` quantizes to
`round(0.3922) * 127.5 = 0 * 127.5 = 0`, giving a posterized row of
`[255, 255, 0, 0]` — cross-checked against an independent Python
script emulating `f32` arithmetic via `struct.pack`/`unpack`
round-tripping. At stroke size `10` (blur radius `1`) and softness `0`,
that row blurs (vertically uniform, reducing to a horizontal 3-tap
average) to `255`, `510 / 3 = 170`, `255 / 3 = 85`, `0` — every one an
exact integer division. A second test raises stroke detail to `3` (5
levels, step `63.75`): `200` and `50` quantize to `191` and `64`, and
the resulting blurred row (`446 / 3 = 148`, `319 / 3 = 106`, truncated
this time rather than exact) matches `box_blur_at`'s own documented
integer-truncating division. A third raises softness to `2`, adding
`1` to stroke size `10`'s own radius for a combined radius of `2`: the
row becomes `1020 / 5 = 204`, `765 / 5 = 153`, `510 / 5 = 102`, `255 /
5 = 51`, confirming softness genuinely widens the radius. A fourth
confines the fixture to the whole of column `1` (all four rows, the
same reasoning `poster_edges`'s own selection test already uses to
keep the fixture vertically uniform), confirming the other three
columns are left completely untouched. A fifth confirms out-of-range
stroke size, stroke detail, and softness, plus a locked/unknown layer,
all error. All five passed on the first run, matching the Python
reference exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighteen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**568 Rust tests total** (563 → 568, 561 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 72 — Filter Gallery > Artistic > Plastic Wrap

Pulls every pixel toward white in proportion to its own edge strength —
the same push `neon_glow` already uses, just with the glow colour fixed
to white — measured on a dilated-then-smoothed Sobel edge map, the same
two-stage `extreme_at`-then-`box_blur_at` edge-map pipeline
`accented_edges` already established. The combination reads as a
glossy, plastic-coated sheen sitting along detail while leaving flat
areas untouched. A documented approximation, not a port of Photoshop's
own renderer. `Document::plastic_wrap(id, highlight_strength, detail,
smoothness)`: `detail` (Photoshop's own `0..=15` range) dilates the
measured edge map by `detail / 3`; `smoothness` (Photoshop's own
`1..=15` range) then box-blurs that edge map by `(smoothness / 3)
.max(1)`, always applying at least some smoothing since Photoshop's own
range never reaches `0`; `highlight_strength` (Photoshop's own `0..=20`
range) scales how far each pixel travels toward white, `orig + (255 -
orig) * (edge / 255) * (highlight_strength / 20)`. Alpha untouched.
Only the final push respects the selection; the edge-detection,
dilation, and smoothing passes always see the whole layer, the same
scope cut `accented_edges` already makes. A new **Plastic Wrap…**
dialog exposes Highlight Strength, Detail, and Smoothness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`
all already share (4×4, columns 0-1 solid `200`, columns 2-3 solid
`50`, raw Sobel magnitude `[0, 255, 255, 0]`). At detail `0` (no
dilation) and smoothness `3` (smoothing radius `1`), the edge map
reduces to `accented_edges`'s own already-verified smoothed row `[85,
170, 170, 85]`. At highlight strength `20` (maximum): column `0`
(`200`, edge `85/255 = 1/3`) pushes to `200 + 55/3 = 218.33 -> 218`;
column `1` (`200`, edge `2/3`) to `200 + 55*2/3 = 236.67 -> 237`;
column `2` (`50`, edge `2/3`) to `50 + 205*2/3 = 186.67 -> 187`; column
`3` (`50`, edge `1/3`) to `50 + 205/3 = 118.33 -> 118` — cross-checked
against an independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping. A second test halves the
highlight strength to `10`, halving the push accordingly. A third
raises detail to `6` (dilation radius `2`), wide enough on this 4-wide
fixture to spread the raw edge map to a uniform `255` everywhere
(smoothing a uniform value changes nothing), so at highlight strength
`20` every pixel pushes fully to white regardless of its own original
shade. A fourth confines the fixture to a one-pixel selection. A fifth
confirms out-of-range highlight strength, detail, and smoothness, plus
a locked/unknown layer, all error. All five passed on the first run,
matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous nineteen: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**573 Rust tests total** (568 → 573, 566 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 73 — Filter Gallery > Artistic > Fresco

Composes three operations this project already has rather than a new
low-level algorithm: `median_at` smoothing blended back toward the
original by `brush_detail`, the exact same shape `dry_brush` already
uses for its own Brush Detail slider, then `brightness_contrast`'s own
contrast formula applied at a fixed positive contrast driven by
`texture`, deepening the coarse, boldly-contrasted look of fresco paint
applied quickly onto wet plaster. A documented approximation, not a
port of Photoshop's own renderer. `Document::fresco(id, brush_size,
brush_detail, texture)`: `brush_size` (Photoshop's own `0..=10` range)
is used directly as the median radius, the same as `dry_brush`'s own
`brush_size`; `brush_detail` (Photoshop's own `0..=10` range) blends
the median result back with the original; `texture` (Photoshop's own
`1..=3` range) is rescaled onto `brightness_contrast`'s own
`-255..=255` domain as `texture * 30` (always positive, since Fresco
only ever boosts contrast) and fed through its exact same formula.
Alpha untouched. A new **Fresco…** dialog exposes Brush Size, Brush
Detail, and Texture sliders.

**Verified two ways.** Six new `document.rs` tests on the `ramped_3x3`
fixture (reusing `dry_brush`'s own already-verified radius-1 median
values: corner `(0,0)` medians to `20` from an original of `10`,
centre `(1,1)` to `50` from an already-`50` original, bottom-right
`(2,2)` to `80` from an original of `90`). At brush detail `0` (pure
smoothed) and texture `1` (contrast `30`, giving `brightness_contrast`'s
own formula a factor of `259 * 285 / (255 * 229) = 1.264064`): the
smoothed values `20`, `50`, `80` push to `-8.52 -> 0` (clamped), `29.40
-> 29`, and `67.32 -> 67`. A second test raises brush detail to `5`
(blend factor `0.5`): the centre is unaffected (already `50` both
smoothed and original), but the bottom-right's blend of smoothed `80`
and original `90` shifts the contrasted result to `74`. A third raises
texture to `3` (contrast `90`, factor `2.073442`), driving the corner's
and centre's smoothed values far enough below mid-grey to clamp fully
to `0`, while the bottom-right lands at `28` rather than `67`. A fourth
confirms brush size `0` skips the median pass entirely, applying the
contrast formula straight to the original ramp (`10`, `50`, `90`
becoming `0`, `29`, `80`). A fifth confines the fixture to a one-pixel
selection. A sixth confirms out-of-range brush size, brush detail, and
texture, plus a locked/unknown layer, all error. All six passed on the
first run, cross-checked against an independent Python script emulating
`f32` arithmetic via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty: this session's Xvfb
instance was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**579 Rust tests total** (573 → 579, 572 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 74 — Filter Gallery > Artistic > Rough Pastels

A third composition of operations this project already has, distinct
from both `paint_daubs` (box blur blended back, no contrast) and
`fresco` (median blended back, with contrast) — `box_blur_at` softens
the layer, `dry_brush`'s own blend-back shape mixes that with the
original by `stroke_detail`, and `brightness_contrast`'s own formula,
at a fixed positive contrast driven by `relief`, raises the contrast
the way pastel pigment catches the light on a textured, raised-relief
surface. Photoshop's own Texture (Brick/Canvas/Burlap/Sandstone),
Scaling, and Light Direction controls, which bump-map an actual
texture image, are a documented scope cut this project doesn't model —
the same kind of simplification `dry_brush` already makes for its own
canvas-grain Texture slider. `Document::rough_pastels(id,
stroke_length, stroke_detail, relief)`: `stroke_length` (Photoshop's
own `0..=40` range) scales down into the blur radius, `stroke_length /
10`, the same scaling shape `ink_outlines` uses for its own stroke
length; `stroke_detail` (Photoshop's own `1..=20` range) blends the
blurred result back with the original; `relief` (Photoshop's own
`0..=40` range) is rescaled onto `brightness_contrast`'s own
`-255..=255` domain as `relief * 2` and fed through its exact same
formula. Alpha untouched. A new **Rough Pastels…** dialog exposes
Stroke Length, Stroke Detail, and Relief sliders.

**Verified two ways.** Six new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap` all already share (4×4, columns 0-1 solid `200`,
columns 2-3 solid `50`), and reusing `paint_daubs`'s own already-
verified box-blur radius-1 and radius-2 rows (`[200, 150, 100, 50]`
and `[170, 140, 110, 80]`) directly. At stroke detail `20` (maximum,
blend factor `1.0`) and relief `0` (contrast factor `1.0`, an
identity), the whole fixture round-trips unchanged regardless of
stroke length. A second test drops stroke detail to `1` (blend factor
`0.05`, mostly blurred) at stroke length `10` (radius `1`): column `1`
blends `150 * 0.95 + 200 * 0.05 = 152.5 -> 153`; column `2` blends
`100 * 0.95 + 50 * 0.05 = 97.5 -> 98` — cross-checked against an
independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping. A third keeps that same blend
but raises relief to `20` (contrast `40`, factor `259 * 295 / (255 *
219) = 1.368162`), pushing the four already-blended values (`200`,
`152.5`, `97.5`, `50`) to `227`, `162`, `86`, and `21`. A fourth raises
stroke length to `20` (radius `2`), reusing `paint_daubs`'s own
radius-2 row and blending it the same way to `172`, `143`, `107`, `79`
(two of which, `143.0` and `107.0`, divide out exactly with no
rounding at all). A fifth confines the fixture to a one-pixel
selection. A sixth confirms out-of-range stroke length, stroke
detail, and relief, plus a locked/unknown layer, all error. All six
passed on the first run, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**585 Rust tests total** (579 → 585, 578 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 75 — Filter Gallery > Artistic > Underpainting

The fourth, and last, composition of `box_blur_at` and a blend-back
this project builds for the Artistic gallery's paint-and-canvas
filters, distinguished from `paint_daubs`, `fresco`, and
`rough_pastels` by a final multiplicative dim rather than a contrast
boost — the muted, duller-toned look of paint laid thinly over an
underlying canvas rather than a bold, textured one. Photoshop's own
Texture (Brick/Canvas/Burlap/Sandstone), Scaling, Light Direction, and
Invert controls, which bump-map an actual texture image, are a
documented scope cut this project doesn't model — the same
simplification `rough_pastels` and `dry_brush` already make for their
own canvas-grain controls. `Document::underpainting(id, brush_size,
texture_coverage)`: `brush_size` (Photoshop's own `0..=40` range)
scales down into the blur radius, `brush_size / 8`, and also sets the
dim factor, `1 - (brush_size / 40) * 0.3`, so a larger brush both
blurs more and mutes the result further, reading as thicker canvas
showing through thinner paint; `texture_coverage` (Photoshop's own
`0..=40` range) is repurposed, the same way `paint_daubs` and
`rough_pastels` already repurpose their own sliders for a documented
simplified formula, as the blend-back amount between the blurred pass
and the original. Alpha untouched. This is the last capability in the
Artistic gallery — all fifteen of its filters (Colored Pencil, Cutout,
Dry Brush, Film Grain, Fresco, Neon Glow, Paint Daubs, Palette Knife,
Plastic Wrap, Poster Edges, Rough Pastels, Smudge Stick, Sponge,
Underpainting, and Watercolor) now ship. A new **Underpainting…**
dialog exposes Brush Size and Texture Coverage sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels` all already share, and reusing
`paint_daubs`'s own already-verified box-blur radius-1 and radius-2
rows (`[200, 150, 100, 50]` and `[170, 140, 110, 80]`) directly. At
brush size `8` (radius `1`, dim `1 - (8/40)*0.3 = 0.94`) and texture
coverage `40` (blend factor `1.0`, discarding the blur entirely): `200
* 0.94 = 188.0` exactly and `50 * 0.94 = 47.0` exactly, both clean
with no rounding needed. A second test drops texture coverage to `0`
(pure blur) at the same brush size, dimming the radius-1 row to `188`,
`141`, `94`, `47` — all four exact. A third raises brush size to `16`
(radius `2`, deeper dim `0.88`), dimming the radius-2 row to `150`,
`123`, `97`, `70` (`170 * 0.88 = 149.6 -> 150`, etc.) — cross-checked
against an independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping. A fourth confines the fixture
to a one-pixel selection. A fifth confirms out-of-range brush size and
texture coverage, plus a locked/unknown layer, all error. All five
passed on the first run, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**590 Rust tests total** (585 → 590, 583 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 76 — Filter Gallery > Sketch > Stamp

Starts the Filter Gallery's Sketch category. Smooths the layer with
`box_blur_at` — the same neighbourhood-average helper `box_blur` and
this project's other smoothing filters already use — then
hard-thresholds the smoothed luma against `light_dark_balance`,
producing the flat black-or-white, simplified-stamp look of a
rubber-stamp graphic. A documented approximation, not a port of
Photoshop's own renderer. `Document::stamp(id, light_dark_balance,
smoothness)`: `smoothness` (Photoshop's own `1..=25` range) scales
down into the blur radius, `(smoothness / 5).max(1)`, the same shape
`plastic_wrap`'s own smoothness uses; `light_dark_balance` (Photoshop's
own `0..=25` range) sets the luma threshold, `light_dark_balance / 25 *
255`: a smoothed pixel at or above the threshold becomes pure white,
one below becomes pure black — so `0` renders the whole layer white
and `25` renders it black, with the balance point sliding between
them. Alpha untouched. A new **Stamp…** dialog exposes Light/Dark
Balance and Smoothness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting` all already share, and
reusing `paint_daubs`'s own already-verified box-blur radius-1 and
radius-2 rows (`[200, 150, 100, 50]` and `[170, 140, 110, 80]`, grey so
luma equals the channel value exactly) directly. At smoothness `5`
(radius `1`) and light/dark balance `10` (threshold `10 / 25 * 255 =
102`): columns `0` and `1` (`200`, `150`) clear the threshold and
become white, columns `2` and `3` (`100`, `50`) fall short and become
black. A second test confirms both balance extremes: `0` (threshold
`0`) renders the whole fixture white, and `25` (threshold `255`, which
no pixel in this fixture reaches) renders it black. A third raises
smoothness to `10` (radius `2`), reusing the radius-2 row: at the same
threshold, columns `0`, `1`, and `2` (`170`, `140`, `110`) all clear it
this time, while column `3` (`80`) stays black — a different pattern
from the radius-1 test, confirming smoothness genuinely widens the
blur before thresholding. A fourth confines the fixture to a one-pixel
selection. A fifth confirms out-of-range light/dark balance and
smoothness, plus a locked/unknown layer, all error. All five passed on
the first run — no independent Python script was needed since every
value here reuses `box_blur_at`'s own already-verified output directly,
and the threshold compare is simple enough to verify by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**595 Rust tests total** (590 → 595, 588 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 77 — Filter Gallery > Sketch > Photocopy

Hard-thresholds a dilated Sobel edge map to pure black or white, the
flat, high-contrast look of a photocopied line drawing where only
strong edges survive as black and everything else bleaches to white.
Reuses the same `sobel_at`/`extreme_at` edge-and-dilate machinery
`ink_outlines` and `poster_edges` already use, combined with `stamp`'s
own hard-threshold idea, just thresholding edge strength instead of
smoothed luma. A documented approximation — Photoshop's real Photocopy
also factors in each pixel's own original luminance directly, not
edges alone, which this project doesn't model — not a port of
Photoshop's own renderer. `Document::photocopy(id, detail, darkness)`:
`detail` (Photoshop's own `0..=24` range) dilates the measured edge
map by `detail / 5`; `darkness` (Photoshop's own `0..=50` range) sets
the threshold, `255 - darkness / 50 * 255`, so `0` requires
full-strength edges to turn black (bleaching everything else to white)
and `50` turns every edge, however faint, black. Alpha untouched. A
new **Photocopy…** dialog exposes Detail and Darkness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp` all already
share (4×4, columns 0-1 solid `200`, columns 2-3 solid `50`, raw Sobel
magnitude `[0, 255, 255, 0]`). At detail `0` (no dilation) and darkness
`0` (threshold `255`): only the full-magnitude edge columns (`1` and
`2`) turn black, while the flat columns (`0` and `3`, magnitude `0`)
stay white. A second test confirms darkness `50` (threshold `0`) turns
every column black, since every magnitude — including the flat
columns' own `0` — meets a threshold of `0`. A third raises detail to
`10` (dilation radius `2`), wide enough on this 4-wide fixture to
spread the raw edge map's `255`-magnitude columns across every column
(the same dilation reasoning `ink_outlines`'s and `plastic_wrap`'s own
width tests already use), so even at darkness `0` every column now
turns black. A fourth confines the fixture to a one-pixel selection. A
fifth confirms out-of-range detail and darkness, plus a locked/unknown
layer, all error. All five passed on the first run — no independent
Python script was needed since every value here reuses `sobel_at`'s
and `extreme_at`'s own already-verified output directly, and the
threshold compare is simple enough to verify by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**600 Rust tests total** (595 → 600, 593 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 78 — Filter Gallery > Sketch > Reticulation

Draws one seeded `XorShift32` value per pixel and thresholds it against
`density` to pick between two flat grey levels — the same per-pixel
seeded draw `film_grain` already uses, just thresholded into a stipple
of two tones rather than added to the original. A documented
approximation of Photoshop's own film-reticulation renderer, which
additionally gives the grain a cracked spatial structure this project
doesn't model. `Document::reticulation(id, density, foreground_level,
background_level, seed)`: `density` (Photoshop's own `0..=50` range)
sets the threshold, `density / 50`, as a fraction of the `0.0..=1.0`
draw: a pixel whose draw falls below it renders at `foreground_level`,
otherwise at `background_level` (both Photoshop's own `0..=50` range,
rescaled to `0..=255` as `level / 50 * 255`), so higher density means
more of the layer renders in the foreground tone. Alpha untouched. The
frontend sends a fresh `seed` on every apply, as with Film Grain. A
new **Reticulation…** dialog exposes Density, Foreground Level, and
Background Level sliders.

**Verified two ways.** Four new `document.rs` tests on a fresh 3×1 grey
fixture, reusing seed `1`'s own already-documented `XorShift32` sequence
(`270369`, `67634689`, `2647435461` out of `u32::MAX`, giving draw
fractions of roughly `0.0000629`, `0.015744`, and `0.616355`). At
density `1` (threshold `0.02`): pixels `0` and `1` fall below it and
render at foreground level `10` (`10/50*255 = 51.0` exactly); pixel `2`
clears it and renders at background level `40` (`40/50*255 = 204.0`
exactly) — the fixture's own non-`255` alpha is carried through
unchanged. A second test confirms both density extremes: `0`
(threshold `0.0`, which no strictly-positive draw can fall below)
renders every pixel at the background level, and `50` (threshold
`1.0`, which every one of these three draws clears) renders every
pixel at the foreground level. A third confines the fixture to a
one-pixel selection, confirming the same architectural fact `spatter`'s
own selection test already documents: `filter_pixels` skips the seeded
draw entirely for unselected pixels, so the selected pixel becomes the
first to consume the generator's own draws. A fourth confirms
out-of-range density, foreground level, and background level, plus a
locked/unknown layer, all error. All four passed on the first run — no
independent Python script was needed since every draw value here
reuses `XorShift32`'s own already-documented sequence directly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**604 Rust tests total** (600 → 604, 597 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 79 — Filter Gallery > Sketch > Note Paper

Nudges each pixel's own luma by a seeded `XorShift32` draw — the same
per-pixel seeded noise `film_grain` and `reticulation` already use,
just added to the source luma instead of standing alone — then
hard-thresholds the result to pure black or white by `image_balance`,
the same threshold idea `stamp` already uses. The grain breaks up the
threshold boundary into a mottled, hand-torn edge rather than a clean
line, reading as paper fibre. A documented approximation — Photoshop's
real Note Paper also embosses the result with a Relief slider this
project doesn't model — not a port of Photoshop's own renderer.
`Document::note_paper(id, image_balance, graininess, seed)`:
`graininess` (this project's own `0..=10` range, a documented
simplification of Photoshop's own dialog) scales the draw's spread,
`draw * (graininess / 10) * 128`, added to the pixel's own luma before
thresholding; `image_balance` (Photoshop's own `0..=50` range) sets the
threshold, `image_balance / 50 * 255`. Alpha untouched. The frontend
sends a fresh `seed` on every apply, as with Film Grain. A new **Note
Paper…** dialog exposes Image Balance and Graininess sliders.

**Verified two ways.** Four new `document.rs` tests on a fresh 3×1 grey
fixture (luma `100`), reusing seed `1`'s own already-documented
`XorShift32` sequence directly (`270369`, `67634689`, `2647435461` out
of `u32::MAX`, mapping to `next_unit` values of roughly `-0.999874`,
`-0.968505`, and `0.232808`). At graininess `10` (factor `1.0`,
spread `128`) and image balance `25` (threshold `127.5`): pixel `0`'s
offset (`-127.98`) and pixel `1`'s (`-123.97`) both push the luma of
`100` to a clamped `0`, well below the threshold, rendering black;
pixel `2`'s offset (`+29.80`) pushes it to `129.80`, clearing the
threshold with a clean margin and rendering white — cross-checked
against an independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping. A second test confirms
graininess `0` makes every offset exactly `0` regardless of the seeded
draw, so the threshold applies straight to the plain luma of `100`,
rendering every pixel black at the same balance. A third confines the
fixture to a one-pixel selection, confirming the same architectural
fact `spatter`'s own selection test already documents: `filter_pixels`
skips the seeded draw entirely for unselected pixels, so the selected
pixel becomes the first to consume the generator's own draws. A fourth
confirms out-of-range image balance and graininess, plus a
locked/unknown layer, all error. All four passed on the first run — no
independent Python script was needed beyond confirming the arithmetic,
since every draw value here reuses `XorShift32`'s own already-documented
sequence directly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**608 Rust tests total** (604 → 608, 601 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 80 — Filter Gallery > Sketch > Graphic Pen

Streaks the layer with a single directional `motion_blur_at` pass — the
same directional line-sampling helper `motion_blur`, `crosshatch`, and
`sprayed_strokes` already use, sharing `sprayed_strokes`'s own four-way
direction convention — then hard-thresholds the result to pure black
or white by `light_dark_balance`, the same threshold idea `stamp`
already uses. The directional streak reads as fine, hatched pen
strokes running one way rather than the isotropic smoothing a box blur
would give. A documented approximation, not a port of Photoshop's own
pen-and-ink renderer. `Document::graphic_pen(id, stroke_length,
light_dark_balance, direction)`: `direction` selects the stroke axis —
`0` Right Diagonal, `1` Horizontal, `2` Left Diagonal, `3` Vertical;
`stroke_length` (Photoshop's own `0..=15` range) is used directly as
the streak's half-length, small enough not to need this project's
usual scaling-down of longer-range stroke parameters; `light_dark_balance`
(Photoshop's own `0..=50` range) sets the threshold, `light_dark_balance
/ 50 * 255`. Alpha untouched. A new **Graphic Pen…** dialog exposes
Stroke Length and Light/Dark Balance sliders plus a Stroke Direction
dropdown.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp`/`photocopy` all
already share (4×4, columns 0-1 solid `200`, columns 2-3 solid `50`,
vertically uniform), and reusing `paint_daubs`'s own already-verified
box-blur radius-1 and radius-2 rows directly, since a horizontal
directional streak (`dy = 0`) reduces to the exact same 3-tap and
5-tap horizontal averages a box blur produces on this fixture. At
direction `1` (Horizontal), stroke length `1` (half `1`): the streak
reduces to `[200, 150, 100, 50]`; at light/dark balance `20`
(threshold `20 / 50 * 255 = 102`), columns `0` and `1` clear it and
render white, columns `2` and `3` fall short and render black — the
same pattern `stamp`'s own first test already established, reached
here through a directional streak instead of a box blur. A second test
raises stroke length to `2` (half `2`), reusing the radius-2 row
`[170, 140, 110, 80]`: at the same threshold, column `2` (`110`) now
clears it and renders white, confirming stroke length genuinely widens
the streak. A third confirms direction selection: direction `3`
(Vertical) streaks along the column instead, a no-op on this
vertically uniform fixture, leaving column `1` at its own unstreaked
original value of `200`; at light/dark balance `35` (threshold
`178.5`), that difference flips column `1`'s outcome — horizontal's
streaked `150` falls short and renders black, while vertical's
unstreaked `200` clears it and renders white. A fourth confines the
fixture to a one-pixel selection. A fifth confirms out-of-range stroke
length and light/dark balance, plus an unrecognised direction and a
locked/unknown layer, all error. All five passed on the first run — no
independent Python script was needed since every averaged value here
reuses `motion_blur_at`'s and `paint_daubs`'s own already-verified
output directly, and the threshold compare is simple enough to verify
by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-seven: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**613 Rust tests total** (608 → 613, 606 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 81 — Filter Gallery > Sketch > Chalk & Charcoal

A three-way threshold on smoothed luma, rather than the two-way
black/white split `stamp`, `photocopy`, and `graphic_pen` all already
use — the darkest pixels render pure black (charcoal), the lightest
pure white (chalk), and everything in between falls to a flat
mid-grey (the paper showing through). `box_blur_at` pre-smooths the
layer by `stroke_pressure`, the same neighbourhood-average helper
`box_blur` and this project's other smoothing filters already use. A
documented approximation, not a port of Photoshop's own
charcoal-and-chalk renderer, which also colours the result with the
foreground/background colours rather than fixed black/grey/white.
`Document::chalk_and_charcoal(id, charcoal_area, chalk_area,
stroke_pressure)`: `stroke_pressure` (this project's own `0..=5`
range, a documented simplification of Photoshop's own dialog) is used
directly as the blur radius; `charcoal_area` (Photoshop's own `0..=50`
range) sets the dark threshold, `charcoal_area / 50 * 255`: a smoothed
pixel at or below it renders black; `chalk_area` (Photoshop's own
`0..=20` range) sets the light threshold, `255 - chalk_area / 20 *
255`: a smoothed pixel at or above it renders white; anything between
the two thresholds renders mid-grey (`128`). Alpha untouched. A new
**Chalk & Charcoal…** dialog exposes Charcoal Area, Chalk Area, and
Stroke Pressure sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp`/`photocopy`/
`graphic_pen` all already share (4×4, columns 0-1 solid `200`, columns
2-3 solid `50`). At stroke pressure `0` (no blur), charcoal area `10`
(dark threshold `51`), chalk area `0` (light threshold `255`, never
reached): columns `0` and `1` (`200`) sit strictly between the
thresholds and render mid-grey; columns `2` and `3` (`50`) fall at or
below the dark threshold and render black. A second test confirms the
light side: charcoal area `0` (dark threshold `0`, never reached),
chalk area `10` (light threshold `127.5`): columns `0` and `1` clear
it and render white, columns `2` and `3` sit between the thresholds
and render mid-grey. A third reuses `paint_daubs`'s own already-
verified box-blur radius-1 and radius-2 rows (`[200, 150, 100, 50]`
and `[170, 140, 110, 80]`) at charcoal area `21` (dark threshold
`107.1`): stroke pressure `1`'s column `2` (`100`) falls at or below
the threshold and renders black, while stroke pressure `2`'s same
column (`110`) now clears it and renders mid-grey instead — confirming
stroke pressure genuinely widens the blur before thresholding. A
fourth confines the fixture to a one-pixel selection. A fifth confirms
out-of-range charcoal area, chalk area, and stroke pressure, plus a
locked/unknown layer, all error. All five passed on the first run — no
independent Python script was needed since every averaged value here
reuses `paint_daubs`'s own already-verified output directly, and the
three-way threshold compare is simple enough to verify by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-eight: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**618 Rust tests total** (613 → 618, 611 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 82 — Filter Gallery > Sketch > Plaster

Pre-smooths the layer with `box_blur_at` — the same neighbourhood-
average helper `box_blur` and this project's other smoothing filters
already use — then applies `emboss`'s own directional relief formula,
`128 + (away − toward)`, at a fixed one-pixel sample distance, reading
as a rounded, raised-plaster surface lit from a chosen compass
direction. A documented approximation, not a port of Photoshop's own
renderer. `Document::plaster(id, image_balance, smoothness,
light_direction)`: `smoothness` (Photoshop's own `1..=15` range)
scales down into the blur radius, `(smoothness / 5).max(1)`;
`light_direction` picks one of Photoshop's own eight compass
directions, converted to the same `angle` convention `emboss` uses (0°
from the right, increasing anticlockwise) — `0` Top, `1` Top Right,
`2` Right, `3` Bottom Right, `4` Bottom, `5` Bottom Left, `6` Left, `7`
Top Left; `image_balance` (Photoshop's own `0..=40` range) biases the
whole relief brighter or darker around its own neutral midpoint of
`20`, `(image_balance - 20) / 20 * 128`, added after the relief
computation. Alpha untouched. A new **Plaster…** dialog exposes Image
Balance and Smoothness sliders plus a Light Direction dropdown.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp`/`photocopy`/
`graphic_pen`/`chalk_and_charcoal` all already share, and reusing
`paint_daubs`'s own already-verified box-blur radius-1 row (`[200,
150, 100, 50]`) directly. At smoothness `5` (radius `1`), light
direction `2` (Right, `(dx, dy) = (1, 0)`), and image balance `20`
(the neutral midpoint, bias exactly `0`): column `0`'s edge-clamped
relief (`200 - 150 = 50`) gives `178`; column `1`'s (`200 - 100 =
100`) gives `228`; column `2`'s (`150 - 50 = 100`) gives `228`; column
`3`'s (`100 - 50 = 50`) gives `178`. A second test confirms image
balance biases that same relief: `40` (bias `+128`) clamps every value
at or above `178` to `255`; `0` (bias `-128`) pulls `178` down to `50`
and `228` down to `100`. A third confirms light direction `6` (Left)
swaps `toward` and `away` relative to Right, negating the relief —
column `1`'s value flips from `228` to `28`. A fourth confines the
fixture to a one-pixel selection. A fifth confirms out-of-range image
balance and smoothness, plus an unrecognised light direction and a
locked/unknown layer, all error. All five passed on the first run — no
independent Python script was needed since every averaged value here
reuses `paint_daubs`'s own already-verified output directly, and the
relief arithmetic is simple enough to verify by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous twenty-nine: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**623 Rust tests total** (618 → 623, 616 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 83 — Filter Gallery > Sketch > Water Paper

A pure composition of two operations this project already has,
delegating rather than reimplementing — `box_blur` simulates colour
bleeding into damp paper fibres, then `brightness_contrast` applies its
own already-verified tone-curve formula for the Brightness and
Contrast sliders. A documented approximation, not a port of
Photoshop's own fibre-bleed renderer. `Document::water_paper(id,
fiber_length, brightness, contrast)`: `fiber_length` (Photoshop's own
`3..=50` range) scales down into the blur radius, `(fiber_length /
10).max(1)`, the same shape `ink_outlines` scales its own stroke
length down; `brightness` and `contrast` (this project's own `0..=100`
range, centred on a neutral `50`, a documented simplification of
Photoshop's own dialog) are each rescaled onto `brightness_contrast`'s
own `-255..=255` domain as `(value - 50) / 50 * 255` before being
passed straight through to it. Alpha untouched (both delegated
operations already leave it alone). A new **Water Paper…** dialog
exposes Fiber Length, Brightness, and Contrast sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp`/`photocopy`/
`graphic_pen`/`chalk_and_charcoal`/`plaster` all already share, and
reusing `paint_daubs`'s own already-verified box-blur radius-1 and
radius-2 rows directly. At fiber length `10` (radius `1`) and both
brightness and contrast at `50` (this filter's own neutral midpoint,
mapping to `brightness_contrast`'s own `(0, 0)` — already proven a
no-op by that filter's own `brightness_contrast_of_zero_and_zero_is_a_no_op`
test): the output is exactly the blurred row `[200, 150, 100, 50]`,
unchanged by the delegated step. A second test raises brightness to
`60` (mapping to `51` on `brightness_contrast`'s own domain, contrast
staying neutral at factor `1.0`): `brightness_contrast`'s own formula
reduces to `v + 51`, giving `251`, `201`, `151`, `101` — all four exact
integers, confirming the delegation actually reaches
`brightness_contrast` rather than being silently skipped. A third
raises fiber length to `20` (radius `2`), reusing the radius-2 row
`[170, 140, 110, 80]` unchanged by neutral brightness/contrast. A
fourth confines the fixture to a one-pixel selection. A fifth confirms
out-of-range fiber length, brightness, and contrast, plus a
locked/unknown layer, all error. All five passed on the first run — no
independent Python script was needed since every value here reuses
`paint_daubs`'s and `brightness_contrast`'s own already-verified output
directly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**628 Rust tests total** (623 → 628, 621 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 84 — Filter Gallery > Sketch > Torn Edges

`box_blur_at`-smooths the layer, then adds a seeded `XorShift32` draw
to the smoothed luma before hard-thresholding to pure black or white —
combining `stamp`'s own blur-then-threshold shape with `note_paper`'s
own grain-before-threshold shape, applied to the blurred signal rather
than the raw one, so the grain breaks the boundary between black and
white into the ragged, torn-paper edge the filter is named for. A
documented approximation, not a port of Photoshop's own renderer.
`Document::torn_edges(id, image_balance, smoothness, contrast, seed)`:
`smoothness` (Photoshop's own `1..=15` range) scales down into the
blur radius, `(smoothness / 5).max(1)`, the same shape `plastic_wrap`'s
own smoothness uses; `contrast` (Photoshop's own `1..=25` range)
scales the draw's spread, `draw * (contrast / 25) * 128`, added to the
smoothed luma; `image_balance` (Photoshop's own `0..=25` range) sets
the threshold, `image_balance / 25 * 255`. Alpha untouched. The
frontend sends a fresh `seed` on every apply, as with Film Grain. A
new **Torn Edges…** dialog exposes Image Balance, Smoothness, and
Contrast sliders.

**Verified two ways.** Four new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp`/`photocopy`/
`graphic_pen`/`chalk_and_charcoal`/`plaster`/`water_paper` all already
share, reusing `paint_daubs`'s own already-verified box-blur radius-1
row (`[200, 150, 100, 50]`) and seed `1`'s own first four `XorShift32`
draws (`270369`, `67634689`, `2647435461`, `307599695` out of
`u32::MAX`, mapping to `next_unit` values of roughly `-0.999874`,
`-0.968505`, `0.232808`, and `-0.856787`) directly. At smoothness `5`
(radius `1`), contrast `25` (factor `1.0`, spread `128`), and image
balance `10` (threshold `102`): columns `0`, `1`, and `3` all fall
short of the threshold after their own grain offset and render black,
while column `2` (`100 + 29.80 = 129.80`) clears it and renders white
— cross-checked against an independent Python script emulating `f32`
arithmetic via `struct.pack`/`unpack` round-tripping. A second test
drops contrast to `1` (factor `0.04`, spread `5.12`): the shrunken
offsets flip columns `0` and `1` to white and column `2` to black,
confirming contrast genuinely scales the grain rather than being
ignored. A third confines the fixture to a one-pixel selection at
column `2` — a genuine test correction was needed here mid-design: an
initial draft assumed the selected pixel would still receive the
*third* draw (the one column `2` gets in an unselected run), when
`filter_pixels` actually skips the seeded draw entirely for unselected
pixels, making the selected pixel the *first* to consume the
generator's own draws instead, landing on black rather than white — the
same architectural fact `spatter`'s own selection test already
documents, caught this time before the phase landed rather than after.
A fourth confirms out-of-range image balance, smoothness, and
contrast, plus a locked/unknown layer, all error. All four passed
after that correction, matching the Python reference exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**632 Rust tests total** (628 → 632, 625 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 85 — Filter Gallery > Sketch > Bas Relief

`box_blur_at`-smooths the layer, computes standard-weighted luma of the
smoothed sample (the same weights `torn_edges` and `threshold` already
use), then reuses `emboss`'s own `away - toward` relief shape at a
fixed 1-pixel sample distance and `plaster`'s own 8-direction angle
table — but on a single grayscale channel instead of per-channel
colour, which is what makes the output a true grayscale relief the way
Photoshop's own Bas Relief is, rather than the tinted relief `plaster`
produces. A documented approximation, not a port of Photoshop's own
stone-carving renderer. `Document::bas_relief(id, detail, smoothness,
light_direction)`: `smoothness` (Photoshop's own `1..=15` range) scales
down into the blur radius, `(smoothness / 5).max(1)`, the same shape
`plaster` and `chalk_and_charcoal` already use; `light_direction`
(Photoshop's own `0..=7` range) selects one of 8 compass angles via the
identical table `plaster` already has (`0`=90° Top, `1`=45° Top Right,
`2`=0° Right, `3`=315° Bottom Right, `4`=270° Bottom, `5`=225° Bottom
Left, `6`=180° Left, `7`=135° Top Left); `detail` (Photoshop's own
`0..=15` range) linearly scales the relief's contribution from none at
`0` (a flat mid-grey plate) to double strength at `15` (`detail / 15.0
* 2.0`) — a documented simplification of Photoshop's own detail
control, which also sharpens fine edges rather than only scaling
contrast. Alpha untouched. A new **Bas Relief…** dialog exposes Detail,
Smoothness, and Light Direction controls, the last a `<select>` of the
same 8 compass options `plaster`'s own dialog already offers.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture `ink_outlines`/`poster_edges`/
`accented_edges`/`sumi_e`/`smudge_stick`/`paint_daubs`/`palette_knife`/
`plastic_wrap`/`rough_pastels`/`underpainting`/`stamp`/`photocopy`/
`graphic_pen`/`chalk_and_charcoal`/`plaster`/`water_paper`/`torn_edges`
all already share, reusing `paint_daubs`'s own already-verified
box-blur radius-2 row (`[170, 140, 110, 80]`) directly — since the
fixture is already grayscale (equal R, G, B in every pixel), its luma
equals the channel value exactly, so no separate luma arithmetic needed
verifying. At smoothness `10` (radius `2`), light direction `2` (Right,
`dx=1, dy=0`), and detail `15` (amount `2.0`): column `0`
(`128 + (170-140)*2 = 188`), column `1` (`128 + (170-110)*2 = 248`),
column `2` (`128 + (140-80)*2 = 248`), and column `3`
(`128 + (110-80)*2 = 188`) are all exact integers, no rounding
ambiguity. A second test drops detail to `6` (amount `0.8`), giving
column `1` a real, hand-computed `176` rather than `248`, and detail
`0` (amount `0.0`), flattening every column to the same neutral `128`
regardless of the underlying blur, confirming `detail` truly gates the
relief. A third flips light direction to `6` (Left, `dx=-1`), which
swaps which neighbour counts as "toward" and which as "away" relative
to direction `2`'s own test, producing the distinct pattern `[68, 8, 8,
68]` rather than `[188, 248, 248, 188]`. A fourth confines the fixture
to a full-column selection at column `1`, confirming (unlike
`torn_edges`) there is no generator-ordering subtlety to correct for
here — `bas_relief` draws no random numbers, so the precomputed
box-blur buffer always reads the whole, unmodified source regardless of
selection, the same approach `plaster` already established, and only
the selected column's own output (`248`) is written back. A fifth
confirms out-of-range detail, smoothness, and light direction, plus a
locked/unknown layer, all error. All five passed on the first run,
cross-checked against an independent Python script emulating `f32`
arithmetic via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**637 Rust tests total** (632 → 637, 630 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 86 — Filter Gallery > Sketch > Halftone Pattern

Recolours the layer as pure black ink on white paper, patterned by how
dark each `size`-pixel cell's own standard-weighted luma average is
(the same luma weights `bas_relief` and `torn_edges` already use).
`Document::halftone_pattern(id, size, contrast, pattern_type)` offers
two of Photoshop's own four pattern types, each reusing existing
machinery rather than inventing new pixel math: `pattern_type` `0`
(Line) divides the layer into vertical bands `size` pixels wide and
inks each one from its own left edge inward by a thickness proportional
to that band's own darkness (`cell * measure / 255`, rounded and
clamped to the band's own width); `pattern_type` `1` (Dot) reuses
`color_halftone`'s own exact `(dx² + dy²) · 255 ≤ r² · measure`
circular-area test, applied here to one grayscale measure instead of
three RGB channels, with a single un-offset screen rather than three
angled ones. Photoshop's own 45°-diagonal line screen and its Circle
pattern type (a variant too close to Dot to be worth a second, only
subtly different area formula) are both documented scope cuts.
`contrast` (Photoshop's own `0..=50` range) linearly amplifies each
cell's own darkness measure away from its own neutral midpoint `128`:
`128 + (measure_raw - 128) * (1.0 + contrast / 50.0)`, a scale of `1.0`
at `contrast=0` up to `2.0` at `contrast=50` — a documented
simplification standing in for Photoshop's own non-linear tone curve,
the same kind of scope cut `bas_relief`'s own linear `detail` scaling
already makes. `size` is Photoshop's own `1..=12` range. Alpha
untouched. A new **Halftone Pattern…** dialog exposes Size, Contrast,
and a Pattern Type dropdown (Line, Dot).

**Verified two ways.** Five new `document.rs` tests. The first reuses
the shared bright/dark cliff fixture (4x4, columns 0-1 solid 200,
columns 2-3 solid 50): at size `2` with Line type, band 0 (columns 0-1,
average luma `200`) works out to a measure of `55` and a thickness of
`round(2 * 55/255) = 0` — no ink, both columns white — while band 1
(columns 2-3, average luma `50`) works out to a measure of `205` and a
thickness of `round(2 * 205/255) = 2`, the full band width, both
columns ink. A second test uses a dedicated solid 4x1 fixture at luma
`145`, chosen so contrast `0` and contrast `50` land on opposite sides
of a rounding boundary at size `4` (a single whole-row band): contrast
`0` gives measure `110`, thickness `round(4 * 110/255) = 2`; contrast
`50` gives measure `92`, thickness `round(4 * 92/255) = 1` — a real,
hand-computed change, not a coincidental match. A third switches the
cliff fixture to Dot type at size `4`, making the whole 4x4 image one
cell: overall average luma `100` gives measure `155`, and with radius
`r = 2` centred at `(2, 2)`, the `(dx² + dy²) * 255 <= r² * 155 = 620`
test is satisfied everywhere except the four corners (row `0`'s every
column, and column `0` of rows `1`-`3`), producing a hand-traceable
white/ink pattern. A fourth confirms selection confinement: since every
band/cell average always reads the whole, unmodified source regardless
of selection (the same approach `plaster` and `bas_relief` already
establish), selecting only column `2` still produces the same ink
result the unselected run's own band 1 already gives. A fifth confirms
out-of-range size, contrast, and pattern type, plus a locked/unknown
layer, all error. All five tests passed on the first run, cross-checked
against an independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-three: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**642 Rust tests total** (637 → 642, 635 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 87 — Filter Gallery > Sketch > Chrome

Maps each pixel's own standard-weighted luma (the same weights
`bas_relief` and `halftone_pattern` already use) through a mirrored
triangular curve that peaks bright at the neutral midtone `128` and
falls off toward black at either extreme: `v = 255 - 2 * amount *
|luma - 128|`. A documented simplification standing in for Photoshop's
own gradient-map-based metallic sheen renderer, chosen specifically
because it's exactly hand-checkable rather than requiring a reflection
map. `Document::chrome(id, detail, smoothness)`: `smoothness`
(Photoshop's own `0..=10` range) scales down into a `box_blur_at`
pre-smoothing radius, `smoothness / 3` — allowed to be `0`, unlike
every earlier Sketch filter's own `.max(1)` floor, since Photoshop's
own Chrome smoothness starts at `0` rather than `1` and a `0` radius is
already a safe same-pixel sample; `detail` (Photoshop's own `0..=10`
range) linearly steepens the curve's slope, `amount = 1.0 + detail /
10.0` (`1.0` at `detail=0` up to `2.0` at `detail=10`), the same kind
of linear-scale scope cut `bas_relief`'s own `detail` parameter already
makes. Alpha untouched. A new **Chrome…** dialog exposes Detail and
Smoothness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture every other Sketch filter shares (4x4,
columns 0-1 solid 200, columns 2-3 solid 50). At smoothness `0`
(radius `0`, a same-pixel no-op) and detail `0` (amount `1.0`): luma
`200` gives `v = 255 - 2*1.0*72 = 111`, luma `50` gives `v = 255 -
2*1.0*78 = 99`, both exact integers. A second test raises detail to
`5` (amount `1.5`), giving `39` and `21` instead — a real, hand-
computed change, not a coincidental match. A third raises smoothness
to `3` (radius `1`), reusing `paint_daubs`'s own already-verified
radius-1 row (`[200, 150, 100, 50]`) directly: columns `0` and `3`
(edge columns whose blur still lands on their own original value)
match the unblurred test's own `111` and `99` exactly, while columns
`1` and `2` (blurred luma `150` and `100`, not their raw `200`/`50`)
give genuinely new values `211` and `199` — changes only the blur
could have produced, since without it columns `0`-`1` and `2`-`3`
share identical raw luma within their own pair. A fourth confines the
fixture to a full-column selection at column `2`. A fifth confirms
out-of-range detail and smoothness, plus a locked/unknown layer, all
error. All five tests passed on the first run, cross-checked against
an independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**647 Rust tests total** (642 → 647, 640 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 88 — Filter Gallery > Distort > Diffuse Glow

Pushes each pixel's own colour toward white, in proportion to how
bright it already is, so highlights bloom outward while shadows stay
comparatively clear — the first Filter Gallery filter since this
project started the Sketch gallery to keep colour rather than reduce
to grayscale. `Document::diffuse_glow(id, graininess, glow_amount,
clear_amount, seed)`: `graininess` (Photoshop's own `0..=10` range)
scales a seeded `XorShift32` draw added to each pixel's own standard-
weighted luma before the glow calculation, `draw * (graininess / 10.0
* 64.0)` — the same per-pixel draw `note_paper` and `reticulation`
already use; `glow_amount` and `clear_amount` (both Photoshop's own
`0..=20` range) combine into a single glow strength, `(glow_amount /
20.0) * (1.0 - clear_amount / 20.0) * (grained_luma / 255.0)`, clamped
to `0.0..=1.0` — `clear_amount` scales the overall strength down
rather than Photoshop's own more nuanced clipping of the glow's own
tone range, a documented simplification. Each RGB channel is pushed
toward white by that strength, `v + (255.0 - v) * strength`; alpha
untouched. Confined to the selection the same way every other seeded
filter in this project is. A new **Diffuse Glow…** dialog exposes
Graininess, Glow Amount, and Clear Amount sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture every Sketch filter shares (4x4, columns 0-1
solid 200, columns 2-3 solid 50). With graininess `0` (zeroing the
grain scale, making the seed irrelevant), glow amount `10` (amount
`0.5`), and clear amount `0`: luma `200` gives `strength = 0.5 *
200/255 = 0.392157`, `v = 200 + 55*0.392157 = 221.57` → `222`; luma
`50` gives `v = 50 + 205*0.098039 = 70.10` → `70`. A second test raises
clear amount to `10` (clear `0.5`), halving the `(1.0 - clear)` factor:
`211` and `60` instead — a real, hand-computed change, not a
coincidental match. A third raises graininess to `10` (grain scale
`64.0`) and reuses seed `1`'s own first four `XorShift32` draws
(`270369`, `67634689`, `2647435461`, `307599695` out of `u32::MAX`,
mapping to `next_unit` values of roughly `-0.999874`, `-0.968505`,
`0.232808`, and `-0.856787`) landing on row `0`'s four pixels in scan
order: column `0` grains to `136.01`, giving `215`; column `1` grains
to `138.02`, also rounding to `215`; column `2` grains to `64.90`,
giving `76`; column `3` grains to a clamped `0` offset, leaving `50`
unchanged. A fourth confines the fixture to a single-pixel selection at
`(1, 0)` — the same architectural fact `spatter`'s own selection test
already documents, since `filter_pixels` skips the draw entirely for
unselected pixels, making the sole selected pixel consume the *first*
draw rather than the *second* it would get unselected, landing on the
same `215` column `0`'s own unselected test computes, a real change
from its own original `200`. A fifth confirms out-of-range graininess,
glow amount, and clear amount, plus a locked/unknown layer, all error.
All five tests passed on the first run, cross-checked against an
independent Python script emulating `f32` arithmetic via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**652 Rust tests total** (647 → 652, 645 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 89 — Filter Gallery > Distort > Glass

Displaces each pixel by a seeded per-cell offset, resampled with
`sample_nearest` — the same resampling primitive `ripple`, `twirl`,
`pinch`, `spherize`, and every other Distort filter this project has
already built already use. A blocky stand-in for a real glass
texture's refraction, the same kind of simplification Photoshop's own
Texture types beyond "Blocks" (Canvas, Frosted, Tiny Lens) and its
Scaling and Invert controls are a documented scope cut around.
`Document::glass(id, distortion, smoothness, seed)`: `smoothness`
(Photoshop's own `1..=15` range) is used directly as the cell's own
side length in pixels, the same "cell = size" convention
`halftone_pattern`'s own `size` parameter already uses — every pixel
within a `smoothness`-pixel-square cell shares one seeded `(dx, dy)`
offset, two `XorShift32` draws per cell (drawn in the same row-major
cell order the pixels themselves are later visited in) scaled by
`distortion` (Photoshop's own `0..=20` range, the offset's own maximum
magnitude in pixels). Alpha is resampled along with colour, matching
every other `sample_nearest`-based Distort filter. Confined to the
selection: cell offsets are always drawn for the whole, unmodified
source regardless of selection (the same approach `plaster` and
`bas_relief` already establish for their own precomputed buffers), and
only the selected pixels' resampled output is written back. A new
**Glass…** dialog exposes Distortion and Smoothness sliders.

**Verified two ways.** Five new `document.rs` tests, introducing a
dedicated `column_stripes_fixture` (4x4, each column its own solid
grayscale value: `10`, `20`, `30`, `40`) specifically because the
shared bright/dark cliff fixture's own two values can't tell a genuine
pixel displacement apart from a coincidental no-op. With smoothness `4`
(the whole 4x4 image one cell) and distortion `2`, seed `1`'s own first
two `XorShift32` draws (`270369`, `67634689` out of `u32::MAX`,
`next_unit` roughly `-0.999874` and `-0.968505`) become every pixel's
shared offset (`dx = -1.999748`, `dy = -1.93701`): row `0`'s four
pixels resample at positions that round (clamped to the layer) to
`10, 10, 10, 20` — three of the four a genuine, hand-computed change
from their own original `20`, `30`, `40`. A second test confirms
distortion `0` is a true no-op regardless of the drawn offsets. A third
narrows smoothness to `2` (four `2×2` cells instead of one), so column
`2` now draws from the *third* and *fourth* `XorShift32` draws instead
of the first cell's own first two, resampling back to its own original
`30` unchanged — a real, hand-computed difference from the single-cell
test's own column `2` result of `10`, not a coincidental match. A
fourth confines the fixture to a single-pixel selection at column `1`.
A fifth confirms out-of-range distortion and smoothness, plus a
locked/unknown layer, all error. All five tests passed on the first
run, cross-checked against an independent Python script that
reproduces both the `XorShift32` draws and `sample_nearest`'s own
half-away-from-zero rounding.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**657 Rust tests total** (652 → 657, 650 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 90 — Filter Gallery > Distort > Ocean Ripple

Layers a seeded per-pixel jitter on top of `ripple`'s own two-axis
sine-wave displacement (`sx = x + amplitude · sin(k·y)`, `sy = y +
amplitude · sin(k·x)`, `k = 2π / wavelength`), resampled the same way
with `sample_nearest` — the jitter is what turns Ripple's own perfectly
periodic waves into Ocean Ripple's own more irregular, non-uniform
look, a documented approximation rather than a port of Photoshop's own
noise-based renderer. `Document::ocean_ripple(id, ripple_size,
ripple_magnitude, seed)`: `ripple_size` (Photoshop's own `1..=15`
range) scales into the wavelength, `wavelength = ripple_size * 4`;
`ripple_magnitude` (Photoshop's own `0..=20` range) linearly scales
both the sine wave's own amplitude (`ripple_magnitude * 0.5`) and the
jitter's own spread (`ripple_magnitude * 0.25`), so `0` is a true
no-op. Two `XorShift32` draws per pixel (`(dx, dy)`, in the same scan
order `filter_pixels` visits pixels in) are scaled by the jitter spread
and added to `sx`/`sy` independently. Alpha is resampled along with
colour, matching every other `sample_nearest`-based Distort filter.
Confined to the selection the same way every other seeded filter in
this project is. A new **Ocean Ripple…** dialog exposes Ripple Size and
Ripple Magnitude sliders.

**Verified two ways.** Five new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`) since a two-value fixture can't tell a
genuine pixel displacement apart from a coincidental no-op. At row `0`,
`k*y` is `0` for any wavelength, isolating the seeded jitter's own
effect: with ripple size `1` and magnitude `10` (amplitude `5.0`,
jitter `2.5`), seed `1`'s own first eight `XorShift32` draws (two per
pixel, in scan order) land row `0` at `[10, 30, 30, 20]` — two of the
four a genuine change from their own original `20` and `40`. A second
test confirms magnitude `0` is a true no-op. A third test raises the
sine term's own contribution at row `1` (magnitude `4`): ripple size
`1` gives wavelength `4` and lands column `0` at `30`, while doubling
to ripple size `2` (wavelength `8`) lands the same column at `20`
instead — a real, hand-computed difference caused only by the
wavelength change, since both runs share the identical ninth draw
(`next_unit` roughly `-0.066179`). A fourth confines the fixture to a
single-pixel selection at column `1`, confirming the same architectural
fact `spatter`'s own selection test already documents: the sole
selected pixel consumes the *first* two draws rather than the *third
and fourth* it would get unselected, landing on a genuinely different
result (`10`) than the unselected first test's own column `1` (`30`).
A fifth confirms out-of-range ripple size and magnitude, plus a
locked/unknown layer, all error. All five tests passed after one
mid-design correction: an initial draft of the wavelength-comparison
test mistakenly reused the very first `XorShift32` draw for row `1`'s
own jitter, forgetting that eight draws are already consumed by row
`0`'s own four pixels before row `1` begins — caught by recomputing the
draw sequence in an independent Python script rather than by a test
failure, and fixed before the test was finalized.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-seven: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**662 Rust tests total** (657 → 662, 655 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 91 — Filter > Stylize > Wind

Streaks each pixel toward one horizontal neighbour by blending it with
the one-directional average of the `length` pixels in that direction —
reusing `average_samples`, the same shared primitive `box_blur_at` and
`motion_blur_at` already build on, but with a one-sided `0..=length`
sample range instead of either of those two's own symmetric window,
which is what turns an ordinary blur into a directional streak. A
documented simplification standing in for Photoshop's own tonal-edge-
triggered, asymmetric streak renderer, and for its own Stagger method's
actual staggered offset pattern. `Document::wind(id, method,
direction)`: `method` (`0` Wind, `1` Blast, `2` Stagger, matching
Photoshop's own dialog radio buttons) selects a `(length, blend)` pair
— Wind `(3, 0.6)`, Blast `(8, 0.9)`, Stagger `(5, 0.75)` — with `blend`
the fraction of the one-directional average mixed into the original,
`v = orig * (1 - blend) + avg * blend`; `direction` (`0` streaks
rightward, `1` leftward) picks which neighbour side is averaged. Each
channel, alpha included, is streaked independently. Confined to the
selection the same way every `filter_pixels`-based filter already is.
A new **Wind…** dialog exposes Method and Direction as two radio-button
groups, matching Extrude's own radio-button convention.

**Verified two ways.** Five new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`), which makes a one-directional average
unambiguous. Method `0` (Wind, length `3`, blend `0.6`) streaking
rightward: column `0` averages `[10, 20, 30, 40]` (avg `25`,
truncating integer division, the same convention `average_samples`
already uses) into `v = 10*0.4 + 25*0.6 = 19`; the full row comes out
`[19, 27, 34, 40]`. A second test flips direction to leftward, giving
the mirror-image row `[10, 15, 22, 31]` — a real, hand-computed
difference, not a coincidental match. A third raises method to `1`
(Blast, length `8`, blend `0.9`): column `0` now averages nine samples
into `33`, giving `v = 10*0.1 + 33*0.9 = 30.7` → `31`, a real change
from Wind's own `19`. A fourth confines the fixture to a full-column
selection at column `1`. A fifth confirms out-of-range method and
direction, plus a locked/unknown layer, all error. All five tests
passed on the first run, cross-checked against an independent Python
script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-eight: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**667 Rust tests total** (662 → 667, 660 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 92 — Filter Gallery > Texture > Grain

Adds a seeded `XorShift32` draw to each pixel's own RGB channels
identically — the same monochromatic-grain shape real film grain has —
then reapplies `brightness_contrast`'s own already-verified tone-curve
formula (reimplemented inline, the same way `fresco` and
`rough_pastels` already do) to the grained result.
`Document::grain(id, intensity, contrast, seed)`: `intensity`
(Photoshop's own `0..=40` range) scales the draw's spread, `draw *
(intensity / 40.0 * 128.0)`, added to each channel before clamping;
`contrast` (Photoshop's own `0..=40` range) rescales onto
`brightness_contrast`'s own `-255..=255` domain as `contrast / 40.0 *
255.0` before being run through its exact factor formula with no
brightness offset — Photoshop's own Grain dialog has no separate
brightness control, only Intensity and Contrast. This project supports
only Photoshop's "Regular" grain type; the other nine (Soft, Sprinkles,
Clumped, Contrasty, Enlarged, Stippled, Horizontal, Vertical, Speckle)
each need their own distinct spatial patterning and are a documented
scope cut, the same kind of narrowing `halftone_pattern`'s own
Circle-vs-Dot cut and `glass`'s own texture-type cut already make.
Alpha untouched. Confined to the selection the same way every other
seeded filter in this project is. A new **Grain…** dialog exposes
Intensity and Contrast sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
bright/dark cliff fixture every Sketch filter shares (4x4, columns 0-1
solid 200, columns 2-3 solid 50). At intensity `40` (grain scale
`128.0`) and contrast `0` (factor `1.0`, isolating the grain's own
effect), seed `1`'s own first four `XorShift32` draws (`270369`,
`67634689`, `2647435461`, `307599695` out of `u32::MAX`, `next_unit`
roughly `-0.999874`, `-0.968505`, `0.232808`, and `-0.856787`) land row
`0` at `[72, 76, 80, 0]`. A second test raises contrast to `10`
(mapping to `63.75` on `brightness_contrast`'s own domain, factor
`1.6581`): the row becomes `[35, 42, 48, 0]`, the factor amplifying
each grained value's own distance from the neutral midpoint `128` — a
real, hand-computed change, not a coincidental match. A third confirms
intensity `0` and contrast `0` together are a true no-op. A fourth
confines the fixture to a single-pixel selection at `(1, 0)` — the
same architectural fact `spatter`'s own selection test already
documents, since `filter_pixels` skips the draw entirely for
unselected pixels, making the sole selected pixel consume the *first*
draw rather than the *second* it would get unselected, landing on a
genuinely different result (`72`) than the unselected first test's own
column `1` (`76`). A fifth confirms out-of-range intensity and
contrast, plus a locked/unknown layer, all error. All five tests
passed on the first run, cross-checked against an independent Python
script emulating `f32` arithmetic via `struct.pack`/`unpack`
round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous thirty-nine: this session's
Xvfb instance was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `npm
run build`) is fully green.

**672 Rust tests total** (667 → 672, 665 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 93 — Filter > Stylize > Tiles

Divides the layer into a grid of `tile_size`-pixel-square cells (the
same "cell = size" convention `halftone_pattern`'s own `size`
parameter and `glass`'s own `smoothness` already use) and slides each
cell's own content by a seeded `(dx, dy)` offset, two `XorShift32`
draws per cell (drawn in the same row-major cell order the pixels
themselves are later visited in) scaled by `max_offset` (Photoshop's
own `0..=99` percent range, of the cell's own side length) and rounded
to a whole pixel. Unlike `glass`, which always resamples via
`sample_nearest`'s own edge-clamping, a shifted tile only shows through
where its slid content still originates from *within that same cell's
own original footprint*; anywhere the shift would pull from outside
it, the pixel falls back to the layer's own unaltered original —
Photoshop's own "Unaltered Image" fill option, the only one of its
four fill choices (Background Color, Foreground Color, Inverse Image,
Unaltered Image) this project implements, a documented scope cut since
the other three need colour pickers or an inversion pass this dialog
doesn't otherwise call for. Alpha moves with its own pixel, matching
every other whole-pixel Distort/Stylize filter in this project.
Confined to the selection: cell offsets are always drawn for the
whole, unmodified source regardless of selection (the same approach
`plaster` and `bas_relief` already establish for their own precomputed
buffers), and only the selected pixels' output is written back. A new
**Tiles…** dialog exposes Tile Size and Maximum Offset sliders.

**Verified two ways.** Five new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`). At tile size `2` and maximum offset
`50` (offset max `1.0` pixel for this cell size), seed `1`'s own first
two `XorShift32` draws round to a shared cell-`(0, 0)` offset of
`(-1, -1)`: pixel `(0, 0)`'s source position `(1, 1)` is still inside
the cell's own `[0, 2) x [0, 2)` footprint, revealing column `1`'s own
value, `20` — a real change from `(0, 0)`'s own original `10` — while
`(1, 0)`, `(0, 1)`, and `(1, 1)` all fall outside their own cell's
footprint and fall back to their own unaltered originals. A second
test confirms maximum offset `0` is a true no-op. A third raises tile
size to `4` (the whole 4x4 image one cell), scaling offset max to
`2.0` pixels and landing pixel `(0, 0)` at `30` instead of `20` — a
real, hand-computed difference from the tile-size-`2` test's own
result, not a coincidental match. A fourth confines the fixture to a
single-pixel selection at `(0, 0)`. A fifth confirms out-of-range tile
size and maximum offset, plus a locked/unknown layer, all error. All
five tests passed on the first run, cross-checked against an
independent Python script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty: this session's Xvfb
instance was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `npm
run build`) is fully green.

**677 Rust tests total** (672 → 677, 670 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 94 — Filter Gallery > Texture > Mosaic Tiles

`mosaic`'s own per-cell flat-average grid (reimplemented inline, the
same `tile_size`-pixel-square cells and the same truncating integer-
division mean), overlaid with a solid grayscale "grout" border
`grout_width` pixels deep along every side of every cell — a pixel
counts as grout if it sits within `grout_width` pixels of any of its
own cell's four edges, so adjacent cells' own borders double up into
one grout line between them, the same as real ceramic tile.
`lighten_grout` (Photoshop's own `0..=10` range) sets the grout's own
grayscale value, `lighten_grout / 10.0 * 255.0` — `0` a black grout
line, `10` a white one — a documented simplification of Photoshop's
own default dark-grey grout tinted lighter, rather than a genuine
tint. `tile_size` (Photoshop's own `2..=100` range) and `grout_width`
(Photoshop's own `0..=15` range) are both validated. Alpha is averaged
into each cell's own mean the same way `mosaic` already does, and the
grout itself is fully opaque. Confined to the selection: cell means
and grout membership are always computed from the whole, unmodified
source regardless of selection, and only the selected pixels' output
is written back. A new **Mosaic Tiles…** dialog exposes Tile Size,
Grout Width, and Lighten Grout sliders.

**Verified two ways.** Five new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`). Tile size `4` makes the whole 4x4
image one cell, whose mean is `(10+20+30+40)/4 = 25` exactly; grout
width `1` marks every pixel within `1` pixel of the cell's own four
edges as grout, leaving only the interior `2x2` block as the mosaic
average: with lighten grout `0`, corner `(0, 0)` is grout (`0`) while
interior `(1, 1)` is the mean (`25`). A second test raises lighten
grout to `10`, mapping to `255` (a white grout line) — a real,
hand-computed change from the first test's own `0`, not a coincidental
match, while the untouched interior still reads `25`. A third confirms
grout width `0` marks no pixel as grout, matching `mosaic`'s own
already-tested single-cell average behaviour everywhere. A fourth
confines the fixture to a single-pixel selection at `(0, 0)`. A fifth
confirms out-of-range tile size, grout width, and lighten grout, plus
a locked/unknown layer, all error. All five tests passed on the first
run, cross-checked against an independent Python script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**682 Rust tests total** (677 → 682, 675 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 95 — Filter Gallery > Texture > Patchwork

`mosaic_tiles`'s own per-cell flat-average grid, given a closed-form
diagonal bevel shade reused verbatim from `extrude`'s own non-random
mode — each square's own luma stands in for its own bevel steepness,
the same way `extrude`'s own `random: false` factor does, and the same
`t = ((cell-1-lx) + (cell-1-ly)) / max_offset - 0.5` diagonal ramp
brightens the square's own top-left corner and darkens its own
bottom-right, mimicking a raised, lit square of fabric. `square_size`
(Photoshop's own dialog is a coarse `0..=10` steps control; this
project substitutes a direct `2..=100` pixel size, the same parameter
substitution `mosaic_tiles`'s own `tile_size` already makes) sets the
cell side length; `relief` (Photoshop's own `0..=25` range) scales the
bevel's own strength exactly as `extrude`'s own `depth` does. Alpha is
each cell's own averaged alpha, untouched by the bevel shade (matching
`extrude`'s own alpha handling). Confined to the selection: cell
averages and the bevel shade are always computed from the whole,
unmodified source regardless of selection, and only the selected
pixels' output is written back. A new **Patchwork…** dialog exposes
Square Size and Relief sliders.

**Verified two ways.** Five new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`). Square size `4` makes the whole 4x4
image one square, mean `(10+20+30+40)/4 = 25` exactly, factor
`25/255 = 0.098039`, max offset `(4-1)*2 = 6.0`. At relief `25`: corner
`(0, 0)` gives `t = 0.5`, shade `1.225`, `v = 26.225` → `26` (the
brightened top-left corner); corner `(3, 3)` gives `t = -0.5`, shade
`-1.225`, `v = 23.775` → `24` (the darkened bottom-right corner). A
second test confirms relief `0` collapses to `mosaic_tiles`'s own flat
average (`25`) everywhere. A third narrows square size to `2` (four
`2x2` squares instead of one): square `(0, 0)`'s own mean drops to
`15`, giving corner `(0, 0)` a real, hand-computed `16` at relief
`25` — a genuine difference from the square-size-`4` test's own `26`,
not a coincidental match. A fourth confines the fixture to a
single-pixel selection at `(3, 3)`. A fifth confirms out-of-range
square size and relief, plus a locked/unknown layer, all error. All
five tests passed on the first run, cross-checked against an
independent Python script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**687 Rust tests total** (682 → 687, 680 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 96 — Filter Gallery > Texture > Stained Glass

`crystallize`'s own jittered-site Voronoi cells (`jittered_sites`,
`nearest_site`, and `voronoi_site_averages` reused directly), with a
solid border drawn wherever a pixel sits within `border_thickness`
pixels of a cell boundary — detected, rather than by computing the
true distance to the second-nearest site, by checking whether the
pixel `border_thickness` away in each of the four cardinal directions
(edge-clamped) belongs to a *different* cell, a documented
approximation of the true geometric Voronoi edge that keeps the check
a handful of extra `nearest_site` lookups rather than a second
distance computation. `light_intensity` (Photoshop's own `0..=10`
range) scales the border's own brightness as a fraction of its cell's
own average, `avg * light_intensity / 10.0` — `0` a solid black leaded
border, `10` bright enough to be indistinguishable from the glass
itself — a documented simplification standing in for Photoshop's own
simulated light source shining through the glass. `cell_size`
(Photoshop's own `2..=50` range) and `border_thickness` (Photoshop's
own `1..=20` range) are both validated. Alpha is each cell's own
averaged alpha outside the border, and fully opaque within it.
Confined to the selection: sites, cell averages, and border membership
are always computed from the whole, unmodified source regardless of
selection (the same convention `crystallize` and `mosaic` already
establish). A new **Stained Glass…** dialog exposes Cell Size, Border
Thickness, and Light Intensity sliders.

**Verified two ways.** Five new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`). Cell size `2` and seed `1` produce four
jittered sites (via the same `jittered_sites` `crystallize` already
uses) at `(1,1)`, `(3,1)`, `(1,2)`, `(2,2)` — one per `2x2` grid square
— assigning every pixel to its own nearest site and averaging each
site's own pixels: site `0` averages `20`, site `2` averages `15`. At
border thickness `1`, pixels `(0, 0)` and `(0, 3)` have every 1-away
neighbour in their own site, so they show their own cell's raw average
untouched (`20` and `15`); `(3, 0)` and `(0, 2)` each border a
different site, so at light intensity `0` (a solid black border) they
become `0`. A second test raises light intensity to `5`, mapping
`(2, 0)`'s own border to `20 * 5/10 = 10` instead of `0` — a real,
hand-computed change, not a coincidental match. A third widens border
thickness to `2`, making `(0, 0)` check neighbours 2 pixels away and
reach a different site, flipping it from the first test's own interior
value of `20` to a border pixel (`0` at light intensity `0`) — a real
difference caused only by the wider border check. A fourth confines
the fixture to a single-pixel selection at `(0, 0)`. A fifth confirms
out-of-range cell size, border thickness, and light intensity, plus a
locked/unknown layer, all error. All five tests passed on the first
run, cross-checked against an independent Python script that
reproduces `jittered_sites`, `nearest_site`, and the border check
exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**692 Rust tests total** (687 → 692, 685 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 97 — Filter Gallery > Texture > Craquelure

Unlike `stained_glass`, which flattens every cell to a single average
colour, this leaves the source image untouched except along the cracks
themselves — reusing the exact same jittered-site membership check
(`jittered_sites`/`nearest_site`, fixed at a 1-pixel-thick crack rather
than a tunable border width) to find them. A crack pixel is darkened
by `crack_depth` and lightened by `crack_brightness`, `v = orig -
crack_depth / 10.0 * 128.0 + crack_brightness / 10.0 * 64.0`, clamped
— a documented simplification standing in for Photoshop's own embossed
crack relief with a directional highlight. `crack_spacing`
(Photoshop's own dialog is a coarse control; this project substitutes
a direct `2..=100` pixel cell size, the same parameter substitution
`mosaic_tiles`'s own `tile_size` already makes) sets the jittered-site
grid; `crack_depth` and `crack_brightness` are both Photoshop's own
`0..=10` ranges. Alpha untouched. Confined to the selection: sites and
crack membership are always computed from the whole, unmodified source
regardless of selection, the same convention `stained_glass` and
`crystallize` already establish. A new **Craquelure…** dialog exposes
Crack Spacing, Crack Depth, and Crack Brightness sliders.

**Verified two ways.** Five new `document.rs` tests, reusing the same
`column_stripes_fixture`, cell spacing `2`, and seed `1` as
`stained_glass`'s own tests, giving the identical jittered sites and
crack membership. Unlike `stained_glass`, non-crack pixels keep the
source untouched: `(0, 0)` and `(1, 0)` stay their own original `10`
and `20`. Crack pixels `(2, 0)` and `(3, 0)`, at crack depth `10`
(darken amount `128.0`) and crack brightness `0`, both clamp to `0`.
A second test raises crack brightness to `10` (lighten amount `64.0`)
with crack depth `0`, giving `94` and `104` instead — real,
hand-computed changes, not a coincidental match. A third confirms
crack depth `0` and crack brightness `0` together are a true no-op. A
fourth confines the fixture to a single-pixel selection at `(2, 0)`. A
fifth confirms out-of-range crack spacing, crack depth, and crack
brightness, plus a locked/unknown layer, all error. All five tests
passed on the first run, cross-checked against an independent Python
script that reproduces `jittered_sites`, `nearest_site`, and the crack
check exactly.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**697 Rust tests total** (692 → 697, 690 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 98 — Image > Adjustments > Selective Color

Nudges each channel toward or away from its own subtractive
complement — Cyan against Red, Magenta against Green, Yellow against
Blue — scaled by how much a pixel belongs to the "Neutrals" colour
range, using Photoshop's own Relative method.
`Document::selective_color(id, cyan, magenta, yellow, black)`: a
pixel's own Neutrals membership weight is `1 - |luma - 128| / 128`
(peaking at the neutral midtone, falling to `0` at pure black or
white); each of `cyan`/`magenta`/`yellow` (Photoshop's own `-100..=100`
range) is applied to its own channel as `v - weight * (slider / 100) *
v` when positive (removing that much of the channel, i.e. adding more
of its complementary ink) or `v - weight * (slider / 100) * (255 - v)`
when negative (adding back toward the channel's own headroom); `black`
is then applied identically to all three already-adjusted channels,
darkening or lightening them together. This project implements only
the Neutrals colour range and the Relative method; Photoshop's other
eight ranges (Reds, Yellows, Greens, Cyans, Blues, Magentas, Whites,
Blacks) each need their own distinct per-channel-dominance weighting
formula, and the Absolute method a different slider interpretation
entirely — both are a documented scope cut, the same kind of
partial-coverage narrowing `grain`'s own "Regular"-type-only cut and
`halftone_pattern`'s own Line/Dot-only cut already make. Alpha
untouched. A new **Selective Color…** dialog exposes Cyan, Magenta,
Yellow, and Black sliders, labelled "Selective Color (Neutrals)" to be
upfront about the scope cut.

**Verified two ways.** Six new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`), grayscale so luma equals the channel
value exactly. Neutrals weight for each column: `0.078125`, `0.15625`,
`0.234375`, `0.3125`. At cyan `100` (magenta and yellow both `0`, only
red touched): `r = v - weight*1.0*v` gives `9`, `17`, `23`, and `28`
(the last one landing exactly on a rounding half-boundary, `27.5`,
confirmed to round up matching Rust's own half-away-from-zero
`f32::round()`) — green and blue stay at their own original value
throughout. A second test flips cyan to `-100`: `r = v +
weight*(255-v)` gives `29`, `57`, `83`, and `107` — real, hand-computed
changes in the opposite direction from the first test's own `9`, `17`,
`23`, `28`, not a coincidental match. A third applies black `50` alone
to all three (still-unchanged) channels uniformly, keeping column `1`
gray at `18` instead of `20`. A fourth confirms all-zero sliders are a
true no-op. A fifth confines the fixture to a full-column selection at
column `1`. A sixth confirms out-of-range cyan, magenta, yellow, and
black, plus a locked/unknown layer, all error. All six tests passed on
the first run, cross-checked against an independent Python script
emulating `f32` arithmetic via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**703 Rust tests total** (697 → 703, 696 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 99 — Layer > Layer Style > Stroke

Paints a solid outline in `color` around the layer's own opaque
content, `size` pixels deep, in Photoshop's own "Outside" position,
baked in destructively — only a fully-transparent pixel with an opaque
neighbour within `size` pixels (Chebyshev distance, the same square-
neighbourhood shape `extreme_at` already uses, rather than a true
circular distance) becomes stroke; every already-opaque pixel is left
completely alone. `Document::stroke_outline(id, size, color, opacity)`
is named `stroke_outline` rather than `stroke` since that name is
already taken by the brush-path paint tool. `size` is Photoshop's own
`1..=250` range; `opacity` (Photoshop's own `0..=100` range) scales
the stroke's own alpha, `opacity / 100.0 * 255.0`. Photoshop's own
Inside and Center stroke positions, its Blend Mode control, and the
fact that a real layer style stays live and editable rather than
baking into the pixels are all documented scope cuts — this project's
layer model has no non-destructive style stack, the same one-shot-bake
stance every filter in this project already takes. A new **Stroke…**
dialog exposes Size, a colour picker, and Opacity.

**Verified two ways.** Five new `document.rs` tests, introducing a
dedicated `stroke_outline_fixture` (6x6, a solid opaque 2x2 block at
rows 2-3, columns 2-3, everywhere else fully transparent) — big enough
that a size-1 stroke doesn't reach every corner of the canvas, unlike
a 4x4 grid with a centred block would. Size `1` reaches `(1, 1)` (its
own 3x3 neighbourhood includes the block's own `(2, 2)`) but not the
far corner `(0, 0)` (whose own clamped neighbourhood never reaches row
`2`); the block's own `(2, 2)` is left completely alone. A second test
raises size to `2`, widening `(0, 0)`'s own neighbourhood far enough
to reach the block and stroke it — a real, hand-computed change from
the size-`1` test's own untouched result. A third drops opacity to
`50`, mapping to a rounded alpha of `128` instead of the full `255` —
real, not a coincidental match. A fourth confines the fixture to a
single-pixel selection at `(1, 1)`. A fifth confirms out-of-range
size and opacity, plus a locked/unknown layer, all error. All five
tests passed on the first run, hand-derived directly from the
Chebyshev-distance definition with no floating-point ambiguity beyond
the opacity test's own single half-boundary rounding.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**708 Rust tests total** (703 → 708, 701 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 100 — Layer > Layer Style > Color Overlay

The hundredth phase of this project's Photoshop-parity build. Blends
every already-opaque pixel's own RGB toward a solid `color` by
`opacity`, `v * (1.0 - frac) + target * frac` where `frac = opacity /
100.0` — Photoshop's own Normal blend mode, the only one of its
several blend-mode choices this project implements (a documented
scope cut, the same kind of narrowing `stroke_outline`'s own
Blend-Mode cut already makes). `opacity` is Photoshop's own `0..=100`
range. A fully-transparent pixel (alpha `0`) has nothing to overlay
onto and is left completely alone, matching `stroke_outline`'s own
treatment of the opposite case. Alpha itself is always untouched. A
new **Color Overlay…** dialog exposes a colour picker and an Opacity
slider.

**Verified two ways.** Six new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`), fully opaque. Opacity `60` toward red
(`255, 0, 0`) gives `R = v*0.4 + 153` (`157`, `161`, `165`, `169`) and
`G`/`B = v*0.4` (`4`, `8`, `12`, `16`), all exact integers. A second
test confirms opacity `100` collapses every pixel to exactly the
overlay colour regardless of its own original value. A third confirms
opacity `0` is a true no-op. A fourth reuses Stroke Outline's own
fixture (6x6, an opaque `2x2` block on an otherwise fully-transparent
canvas) to confirm a transparent pixel has nothing to overlay onto and
stays untouched, while the opaque block collapses fully to the overlay
colour at opacity `100`. A fifth confines the fixture to a full-column
selection at column `1`. A sixth confirms out-of-range opacity, plus a
locked/unknown layer, all error. All six tests passed on the first
run, with no separate Python cross-check needed this phase since the
formula is a plain linear blend with no seeded randomness or
trigonometry to emulate.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-seven: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**714 Rust tests total** (708 → 714, 707 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 101 — Layer > Layer Style > Gradient Overlay

`color_overlay`'s own blend-toward-a-target formula, but the target
colour is interpolated between `color1` and `color2` by the pixel's
own position along the layer: `t = col / (width-1)` for `direction`
`0` (horizontal, left `color1` to right `color2`) or `t = row /
(height-1)` for `direction` `1` (vertical, top `color1` to bottom
`color2`) — Photoshop's own arbitrary gradient angle and its Scale,
Style (Radial, Angle, Reflected, Diamond), and Dither controls are all
a documented scope cut in favour of these two hand-checkable
axis-aligned directions. `opacity` (Photoshop's own `0..=100` range)
scales the blend exactly as `color_overlay`'s own does. A
fully-transparent pixel is left completely alone, matching
`color_overlay`'s own treatment. Alpha untouched. A new **Gradient
Overlay…** dialog exposes two colour pickers, a Direction dropdown,
and an Opacity slider.

**Verified two ways.** Six new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`). Direction `0` (horizontal), black to
white, opacity `100` (a full replace): `t = col/3`, giving `0`, `85`
(`255/3` exactly), `170`, and `255` across the row, all exact
integers. A second test drops opacity to `50`, blending the target
50/50 with each column's own original value (`5`, `53`, `100`, `148`
— the `53` and `148` each landing on a `.5` rounding boundary,
confirmed to round away from zero) — real, hand-computed changes from
the opacity-`100` test's own row, not a coincidental match. A third
switches to direction `1` (vertical): column `0`'s own value (`10` in
every row) now sees a real gradient down the column (`0`, `85`,
`170`, `255`), a genuinely different pattern from the horizontal
test's own column `0`, which stays flat at `0` across every row since
`t` there depends on column, not row. A fourth reuses Stroke Outline's
own fixture to confirm a transparent pixel is left completely alone.
A fifth confines the fixture to a full-column selection at column
`1`. A sixth confirms out-of-range direction and opacity, plus a
locked/unknown layer, all error. All six tests passed on the first
run, with no separate Python cross-check needed for the arithmetic
itself, though the two `.5`-boundary roundings in the opacity test
were independently verified via a supplementary Python script before
being finalized.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-eight: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**720 Rust tests total** (714 → 720, 713 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 102 — Layer > Layer Style > Outer Glow

Extends `stroke_outline`'s own Chebyshev-distance-to-the-nearest-
opaque-pixel idea from a hard-edged stroke into a fading halo — a
transparent pixel whose own nearest opaque neighbour is `d` pixels
away (Chebyshev distance, `d < size`) becomes `color` at alpha `(1.0 -
d / size) * opacity / 100.0 * 255.0`, fading linearly from fully
visible right at the edge to fully transparent at `size` pixels out —
a documented simplification of Photoshop's own tunable Contour-curve
falloff, which defaults to roughly this linear shape anyway. A
transparent pixel with no opaque neighbour within `size`, and every
already-opaque pixel, are both left completely alone. `size` is
Photoshop's own `1..=250` range; `opacity` is its own `0..=100` range.
Photoshop's own Blend Mode, Technique (Precise vs. Softer), Range, and
Jitter controls are all a documented scope cut, the same kind of
narrowing `stroke_outline`'s own Blend-Mode cut already makes. A new
**Outer Glow…** dialog exposes Size, a colour picker, and Opacity.

**Verified two ways.** Six new `document.rs` tests, reusing Stroke
Outline's own fixture (6x6, an opaque 2x2 block at rows 2-3, columns
2-3, everywhere else transparent). Size `2`, opacity `100`, colour
green: pixel `(1, 1)`'s own nearest opaque neighbour is `1` pixel away
(`< 2`, reachable), giving alpha `(1 - 1/2)*255 = 127.5` → `128` (half
away from zero); pixel `(0, 0)`'s own nearest neighbour is `2` pixels
away, not `< 2`, so it's left completely unchanged. A second test
drops opacity to `50`, halving `(1, 1)`'s own alpha to `64` — a real,
hand-computed change, not a coincidental match. A third widens size to
`3`, now reaching `(0, 0)` at alpha `85` exactly — a real difference
from the size-`2` test's own untouched result. A fourth confirms the
opaque block's own pixel is left completely alone. A fifth confines
the fixture to a single-pixel selection at `(1, 1)`. A sixth confirms
out-of-range size and opacity, plus a locked/unknown layer, all error.
All six tests passed on the first run, hand-derived directly from the
Chebyshev-distance definition and independently verified via a
supplementary Python script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous forty-nine: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**726 Rust tests total** (720 → 726, 719 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 103 — Layer > Layer Style > Inner Glow

The mirror image of `outer_glow` — instead of fading a glow outward
from the edge into transparent space, this blends an already-opaque
pixel toward `color` in proportion to how close it sits to the
*nearest transparent pixel* (the same Chebyshev-distance search
`outer_glow` and `stroke_outline` already share, just looking for the
opposite alpha). A pixel whose own nearest transparent neighbour is
`d` pixels away (`d < size`) blends toward `color` by `(1.0 - d /
size) * opacity / 100.0`, reusing `color_overlay`'s own linear blend
shape with a distance-scaled fraction instead of a constant one;
deep-interior opaque pixels with no transparent neighbour within
`size` are left completely alone, and so is every already-transparent
pixel. `size` is Photoshop's own `1..=250` range; `opacity` is its own
`0..=100` range. Photoshop's own Blend Mode, Technique, Source (Center
vs. Edge), Choke, and Contour controls are all a documented scope cut,
the same kind of narrowing `stroke_outline`'s own Blend-Mode cut
already makes. A new **Inner Glow…** dialog exposes Size, a colour
picker, and Opacity.

**Verified two ways.** Six new `document.rs` tests, introducing a
dedicated `inner_glow_fixture` (6x6, a solid opaque 4x4 block at rows
1-4, columns 1-4, everywhere else fully transparent) — large enough
that its own centre pixels sit farther than a small `size` from the
nearest transparent pixel, giving these tests a genuine untouched-
interior case to contrast against near-edge blending. Size `2`,
opacity `100`, colour black: pixel `(1, 1)` (the block's own corner)
has a transparent neighbour `1` pixel away, `d=1 < 2`, blending its
own `(100, 150, 200)` halfway to black at `(50, 75, 100)`; pixel
`(2, 2)`, two pixels deep into the block, has no transparent neighbour
within radius `2`, so it's left completely untouched. A second test
drops opacity to `50`, halving the blend fraction at `(1, 1)` to give
`(75, 113, 150)` — real, hand-computed, not a coincidental match. A
third widens size to `3`, now reaching `(2, 2)` at `(67, 100, 133)` —
a real difference from the size-`2` test's own untouched result. A
fourth confirms a transparent pixel is left completely alone. A fifth
confines the fixture to a single-pixel selection at `(1, 1)`. A sixth
confirms out-of-range size and opacity, plus a locked/unknown layer,
all error. All six tests passed on the first run, hand-derived
directly from the Chebyshev-distance definition and independently
verified via a supplementary Python script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty: this session's Xvfb
instance was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The dialog's wiring was reviewed by hand instead. Every
other layer of this project's quality bar (hand/script-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `npm
run build`) is fully green.

**732 Rust tests total** (726 → 732, 725 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 104 — Layer > Layer Style > Drop Shadow

A solid-coloured copy of the layer's own alpha silhouette, offset by
`distance` pixels at `angle` (the same "0° from the right, increasing
anticlockwise" convention `emboss` and `plaster` already use) and
softened by averaging alpha over a `size`-pixel-radius window
(edge-clamped, truncating integer division — the same shape
`box_blur_at` uses, just restricted to the alpha channel alone), shown
only where the layer's own foreground is transparent — an
already-opaque pixel always shows its own foreground content
untouched, exactly the visual stacking order Photoshop's own Drop
Shadow has (the shadow sits behind the layer). `distance` (Photoshop's
own `0..=30`-ish range, though this project accepts up to `100`) and
`size` (Photoshop's own `0..=250` range) are both pixel counts;
`opacity` is Photoshop's own `0..=100` range, scaling the softened
alpha directly. A pixel whose own resulting shadow alpha rounds to `0`
is left byte-for-byte at its own original value rather than writing a
zero-alpha copy of `color`. Alpha blending, Blend Mode, Spread,
Contour, and Noise are all a documented scope cut, the same kind of
narrowing `stroke_outline`'s own Blend-Mode cut already makes — this
project's layer model also has no non-destructive style stack, so
like every other layer style here this bakes in directly rather than
staying live and editable. A new **Drop Shadow…** dialog exposes
Distance, Angle, Size, a colour picker, and Opacity.

**Verified two ways.** Five new `document.rs` tests, reusing Stroke
Outline's own fixture (6x6, an opaque 2x2 block at rows 2-3, columns
2-3, everywhere else transparent). Distance `1`, angle `0` (`dx=1,
dy=0`), size `0` (a single sample), opacity `100`: pixel `(2, 4)` is
transparent in the original and its own shadow-source position,
`(2, 3)`, lands on the block's own opaque pixel, giving shadow alpha
`255` — solid black; pixel `(0, 0)`'s own shadow-source position,
clamped to `(0, 0)`, is itself transparent, so its computed shadow
alpha rounds to `0` and it's left byte-for-byte at its own original
value. This test needed one mid-design correction: an initial draft's
own assertion for `(2, 4)` was mistakenly transcribed as the
unchanged-transparent value instead of the actual computed `255`,
caught immediately by the test itself failing rather than by a later
review, and fixed before the phase landed. A second test drops
opacity to `50`, halving `(2, 4)`'s own alpha to `128` — a real,
hand-computed change, not a coincidental match. A third raises size to
`1`, averaging alpha over a `3x3` window (four of the nine samples the
block's own opaque `255`, five transparent `0`) to a truncating
`1020/9 = 113` — a real difference from the unsoftened size-`0` test's
own `255`. A fourth confines the fixture to a single-pixel selection
at `(4, 2)`. A fifth confirms out-of-range distance, a non-finite
angle, out-of-range size, and out-of-range opacity, plus a
locked/unknown layer, all error. All five tests passed after the one
documented correction, cross-checked against an independent Python
script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**737 Rust tests total** (732 → 737, 730 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 105 — Layer > Layer Style > Pattern Overlay

`color_overlay`'s own blend-toward-a-target formula, but the target
alternates between `color1` and `color2` in a `scale`-pixel-square
checkerboard, `((row / scale) + (col / scale)) % 2`. Photoshop's own
Pattern Overlay fills with a user-supplied pattern asset (a saved
swatch, or one of its own built-in presets); this project has no
pattern-asset library or file-loading UI to draw from, so a procedural
two-colour checkerboard is a documented scope cut standing in for it,
the same kind of substitution `mosaic_tiles` and `stained_glass`
already make for their own procedural cell grids. `scale` is a pixel
cell size (Photoshop's own dialog is a `1..=100` *percent* scale of
the pattern asset's own size; this project substitutes a direct
`1..=250` pixel size, the same parameter substitution `mosaic_tiles`'s
own `tile_size` already makes); `opacity` is Photoshop's own `0..=100`
range, scaling the blend exactly as `color_overlay`'s own does. A
fully-transparent pixel is left completely alone, matching
`color_overlay`'s own treatment. Alpha untouched. A new **Pattern
Overlay…** dialog exposes Scale, two colour pickers, and Opacity. This
completes this project's own Layer Style category: Stroke, Color
Overlay, Gradient Overlay, Outer Glow, Inner Glow, Drop Shadow, and
Pattern Overlay are all now shipped (Bevel & Emboss, Contour, Texture,
and Satin remain a documented gap, each needing either a height-field
bevel model or a wavy alpha-intersection sheen this project hasn't
built the machinery for yet).

**Verified two ways.** Six new `document.rs` tests, reusing Glass's
own `column_stripes_fixture` (4x4, each column its own solid grayscale
value: `10`, `20`, `30`, `40`), fully opaque. Scale `2`, opacity `100`,
red/blue: row `0` (`row/2=0`) gives red across columns `0`-`1` and
blue across columns `2`-`3`; row `2` (`row/2=1`) flips the pattern,
blue then red. A second test drops opacity to `50`, blending column
`0`'s own original `10` halfway to red (`133, 5, 5`, the `133` landing
on a `.5` rounding boundary confirmed to round away from zero) — real,
hand-computed, not a coincidental match. A third narrows scale to `1`,
checkerboarding every single pixel (`red, blue, red, blue` across row
`0`) — a genuinely different pattern from the scale-`2` test's own
two-wide bands. A fourth reuses Stroke Outline's own fixture to
confirm a transparent pixel is left completely alone. A fifth confines
the fixture to a full-column selection at column `1`. A sixth confirms
out-of-range scale and opacity, plus a locked/unknown layer, all
error. All six tests passed on the first run, cross-checked against an
independent Python script.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**743 Rust tests total** (737 → 743, 736 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 106 — Layer > Layer Style > Bevel & Emboss

`bevel_emboss(id, size, light_direction, strength)`, Photoshop's own
"Inner Bevel" style only. Builds a per-pixel "height" field: `0` for an
already-transparent pixel, otherwise its own Chebyshev distance to the
nearest transparent pixel within a `size`-pixel search radius, capped
at `size` — the same brute-force distance search `inner_glow` and
`stroke_outline` already use, just returning a ramped distance instead
of a fading blend, and a deep-interior pixel with no transparent
neighbour anywhere within that radius sits at the flat "plateau"
height of `size` itself rather than its own true (larger) distance.
Each opaque pixel then samples this height field one pixel out in two
opposite directions along `plaster`'s own 8-direction `light_direction`
angle table (`0`=90°Top through `7`=135°TopLeft): `toward` on the
light's own near side, `away` on its far side, reusing `emboss` /
`plaster` / `bas_relief`'s own `away − toward` relief convention.
`shade = (away_height − toward_height) * (strength / 100.0)` is
*added* to each of the pixel's own RGB channels — not used to replace
them the way `bas_relief`'s own flattened grey relief does, since
Bevel & Emboss is meant to shade existing artwork, not flatten it —
then clamped to `0..=255`; alpha and every already-transparent pixel
pass through untouched. `size` is Photoshop's own `1..=250` Size
range; `light_direction` is `0..=7`; `strength` is this project's own
`0..=100` linear stand-in for Photoshop's own `1..=1000%` Depth
control, the same kind of range substitution `extrude`'s own `level`
parameter already makes. A new **Bevel & Emboss…** dialog exposes
Size, a Light Direction dropdown (the same eight compass options
`Plaster`/`Bas Relief` already use), and Strength. Photoshop's own
Outer Bevel, Emboss, Pillow Emboss, and Stroke Emboss styles, its
Technique (Smooth / Chisel Hard / Chisel Soft) and Direction (Up /
Down) toggle, Soften, Angle/Altitude 3-D lighting, Gloss Contour, and
Highlight/Shadow colour + blend-mode controls are all a documented
scope cut — Contour, Texture, and Satin remain the last unshipped
members of this project's own Layer Style category.

**Verified two ways.** Seven new `document.rs` tests, reusing Inner
Glow's own fixture (6x6, a solid opaque 4x4 block `(100, 150, 200,
255)` at rows 1-4, columns 1-4, everywhere else fully transparent).
Light direction `6` (180°, `dx=-1, dy=0`), size `2`, strength `100`:
pixel `(row 2, col 1)`, on the block's own left edge, has `toward =
height(2, 0) = 0` (a transparent pixel short-circuits to height `0`
regardless of size) and `away = height(2, 2) = 2` (its own nearest
transparent pixel sits a Chebyshev distance of `2` away, within the
size-`2` search radius), giving `relief = 2`, `shade = 2.0`, and
`(100, 150, 200) + 2 = (102, 152, 202)`; the mirror pixel `(row 2, col
4)` on the block's own right edge gives the opposite sign, `(98, 148,
198)`. A second test halves strength to `50`, halving the shade to
`1.0` for a real `(101, 151, 201)` — a genuine change, not a
coincidental match. A third flips light direction to `2` (0°, the
opposite compass point), swapping which sample is `toward` and which
is `away` at the very same pixel, size, and strength, landing on
`(98, 148, 198)` — the mirror image of the direction-`6` result. A
fourth narrows size to `1`, so the `away` sample's own true distance-2
neighbour falls outside the smaller radius-1 search window and it
plateaus at height `1` instead, giving a smaller `(101, 151, 201)` —
demonstrating a larger size senses a taller, truer height and so a
stronger shade. A fifth confirms a fully-transparent pixel is left
completely alone. A sixth confines the fixture to a single-pixel
selection at `(row 2, col 1)`. A seventh confirms out-of-range size,
light direction, and strength, plus a locked/unknown layer, all
error. All seven tests passed on the first run, cross-checked against
an independent Python script that emulates Rust's own `f32` rounding
via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**750 Rust tests total** (743 → 750, 743 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 107 — Layer > Layer Style > Inner Shadow

`inner_shadow(id, distance, angle, size, color, opacity)` is the mirror
image of `drop_shadow`, reusing its exact offset math (`dx`/`dy` from
`distance` and `angle`, the same "0° from the right, increasing
anticlockwise" convention `emboss`/`plaster` already use) and its exact
edge-clamped box-average-over-`size` window (truncating integer
division, the same shape `box_blur_at` uses). The two differences: the
window averages each sampled pixel's own *inverse* alpha (`255 -
alpha`, how much background shows through) instead of its alpha, and
the result is applied only to already-opaque pixels instead of only to
already-transparent ones. An opaque pixel whose own `(row - dy, col -
dx)` neighbourhood sits mostly outside the layer's own silhouette gets
a shadow blended onto its own colour in proportion to that averaged
transparency, scaled by `opacity`; a pixel whose neighbourhood is
fully opaque (deep interior, or on the edge facing toward the light)
gets a shadow alpha of `0` and is left byte-for-byte at its own
original value. Unlike `drop_shadow`, which replaces a transparent
pixel outright since there's nothing there to preserve, this blends
the shadow colour onto the pixel's own existing colour (`orig * (1 -
frac) + color * frac`), the same linear blend shape `inner_glow`
already uses — the pixel's own alpha always stays untouched. `distance`
and `size` are pixel counts (`distance` up to `100`, `size` up to
`250`, the same ranges `drop_shadow` already accepts); `opacity` is
Photoshop's own `0..=100` range. Blend Mode, Choke, Contour, and Noise
are all a documented scope cut, the same kind of narrowing
`drop_shadow`'s own scope cut already makes.

**Verified two ways.** Seven new `document.rs` tests, reusing Stroke
Outline's own fixture (6x6, a solid opaque 2x2 block `(100, 150, 200,
255)` at rows 2-3, columns 2-3, everywhere else fully transparent).
Distance `1`, angle `0` (`dx=1, dy=0`), size `0`, opacity `100`, colour
black: pixel `(row 2, col 2)`, the block's own left column, samples
inverse-alpha at `(2, 1)`, which is transparent, so its own shadow
alpha is `255` and it blends fully to black, `(0, 0, 0, 255)` — its own
alpha is preserved, unlike `drop_shadow`'s outright replacement; pixel
`(row 2, col 3)`, the block's own right column, samples `(2, 2)`,
which is opaque, so its own shadow alpha is `0` and it's left
byte-for-byte at its own original `(100, 150, 200, 255)` — the side
facing the light stays lit. A second test halves opacity to `50`,
giving shadow alpha `round(255*0.5) = 128` and a real, hand-computed
`(50, 75, 100)` instead of the opacity-100 test's own `(0, 0, 0)`. A
third flips angle to `180` (`dx=-1, dy=0`), swapping which column
darkens at the very same pixels — the mirror image of the angle-`0`
result. A fourth widens size to `1`, averaging inverse-alpha over a
3x3 window (`7` of `9` samples transparent) for a truncating average
of `1785/9 = 198`, blending `(100, 150, 200)` by `198/255 = 0.77647`
toward black to a real, more-softened `(22, 34, 45)` at column `2` and
`(45, 67, 89)` at column `3`, distinct from the crisp size-`0` result.
A fifth confirms a fully-transparent pixel is left completely alone. A
sixth confines the fixture to a single-pixel selection. A seventh
confirms out-of-range distance, a non-finite angle, out-of-range size
and opacity, plus a locked/unknown layer, all error. All seven tests
passed on the first run, cross-checked against an independent Python
script that emulates Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**757 Rust tests total** (750 → 757, 750 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 108 — Layer > Layer Style > Contour

`contour(id, size, light_direction, strength)` reuses `bevel_emboss`'s
own machinery almost entirely — the same per-pixel "height" field (now
factored into a shared private free function, `bevel_height_at`, so
both filters call the identical implementation rather than keeping two
copies of the same brute-force Chebyshev-distance search), the same
`plaster`-angle-table offset sampling, the same additive shading. The
one change is what Photoshop's own Contour panel actually does: it
remaps a bevel's shading ramp through a curve instead of using it
linearly. This project has no general curve editor for layer styles,
so it substitutes one specific, well-known preset — "Ring" — the same
kind of single-preset substitution `grain`'s "Regular" grain type and
`pattern_overlay`'s procedural checkerboard already make in place of
Photoshop's own fuller controls. Before differencing, each sampled
height is passed through `ring(h) = size - |2h - size|`: a triangular
curve that's `0` at the shape's own edge (`h = 0`), rises to a peak of
`size` at exactly half-depth (`h = size / 2`), and falls back to `0`
at the flat interior plateau (`h = size`) — the bright/dark ring right
at the bevel's own midline that gives the Ring preset its name.
`shade = (ring(away) - ring(toward)) * (strength / 100.0)` is added to
each colour channel exactly as `bevel_emboss` already does. `size`
(`1..=250`), `light_direction` (`0..=7`), and `strength` (`0..=100`)
share `bevel_emboss`'s own parameter ranges and meanings exactly. A
new **Contour…** dialog mirrors Bevel & Emboss's own dialog layout:
Size, the same eight-direction Light Direction dropdown, and Strength.

**Verified two ways.** Seven new `document.rs` tests, reusing Inner
Glow's own fixture (6x6, a solid opaque 4x4 block `(100, 150, 200,
255)` at rows 1-4, columns 1-4). Direction `2` (0°, `dx=1, dy=0`),
size `2`, strength `100`: pixel `(row 2, col 2)` has `toward =
height(2, 3) = 2` (the size-2 plateau, `ring(2) = 2-|4-2| = 0`) and
`away = height(2, 1) = 1` (exactly half of size `2`, the ring's own
peak, `ring(1) = 2-|2-2| = 2`), giving `relief = ring(away) -
ring(toward) = 2 - 0 = 2`, `shade = 2.0`, and a real `(102, 152, 202)`
— genuinely different from plain `bevel_emboss`'s own `(99, 149, 199)`
at these very same parameters, confirming Contour remaps the field
through the ring curve rather than differencing it directly. Pixel
`(row 2, col 4)` has both `toward` and `away` land on the ring's own
two zero-crossings (heights `0` and `2`), giving `relief = 0` and no
change — a real consequence of the ring's own non-monotonic shape, not
an oversight. A second test halves strength to `50`, halving the shade
to `1.0` for a real `(101, 151, 201)`. A third flips direction to `6`
(180°), swapping toward and away at the very same pixel for the mirror
`(98, 148, 198)`. A fourth narrows size to `1`, where both samples
land on the very same size-1 plateau height and so ring to the very
same value, cancelling to `0` change — a real, hand-computed
difference from the size-`2` test's own `(102, 152, 202)`, showing the
ring's own peak lands somewhere else entirely once size changes which
depths are reachable. A fifth confirms a fully-transparent pixel is
left completely alone. A sixth confines the fixture to a single-pixel
selection. A seventh confirms out-of-range size, light direction, and
strength, plus a locked/unknown layer, all error. All seven tests
passed on the first run, cross-checked against an independent Python
script that emulates Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**764 Rust tests total** (757 → 764, 757 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 109 — Layer > Layer Style > Texture

`texture(id, size, light_direction, strength, scale, depth)` completes
this project's own Layer Style category (Stroke, Color Overlay,
Gradient Overlay, Outer Glow, Inner Glow, Drop Shadow, Bevel & Emboss,
Pattern Overlay, Inner Shadow, Contour, and now Texture are all
shipped; only Satin remains unshipped, and stays a documented gap —
its own classic implementation needs a second offset-and-invert pass
whose exact blend this project doesn't yet have an authoritative
reference for, the same kind of fabrication risk already documented
for Color Lookup). Texture overlays a bump-map perturbation onto
`bevel_emboss`'s own height field before differencing, composing two
mechanisms this project already has rather than inventing a third:
`bevel_height_at` (the same shared height-field search `bevel_emboss`
and `contour` both already call) and `pattern_overlay`'s own `((row /
scale) + (col / scale)) % 2` checkerboard-cell formula, standing in
for Photoshop's own pattern-asset bump texture exactly the way
`pattern_overlay` itself already substitutes that same checkerboard
for a real pattern swatch. Each of the two sample points (`toward` and
`away`, at the identical `light_direction`-offset positions
`bevel_emboss` samples) adds `depth` on top of its own
`bevel_height_at` value whenever it falls on an even checkerboard
cell, `0` on an odd one, before `relief = away − toward` is taken — so
the bump only has a visible effect where the two sample points land on
*different* cells. Where a `light_direction`/`scale` combination puts
both samples on the same cell — `scale = 1` under any of this
project's eight compass directions, since the two samples sit a whole
2-pixel span apart and a 1-pixel checkerboard always returns to the
same parity two steps later — the bump cancels out of the difference
entirely and the result matches plain `bevel_emboss` exactly, a real
structural consequence of the design rather than a hidden bug. `shade
= relief * (strength / 100.0)` is added to each colour channel exactly
as `bevel_emboss` already does. `size` (`1..=250`), `light_direction`
(`0..=7`), and `strength` (`0..=100`) share `bevel_emboss`'s own
ranges; `scale` (`1..=250`) shares `pattern_overlay`'s own cell-size
range; `depth` (`0..=100`) is a pixel-unit bump height, a documented
linear stand-in for Photoshop's own `-100..=100%` Depth control — this
project's own version is additive only, so Photoshop's own Invert
toggle is a documented scope cut. A new **Texture…** dialog extends
Bevel & Emboss's own dialog layout with two more sliders, Scale and
Depth.

**Verified two ways.** Seven new `document.rs` tests, reusing Inner
Glow's own fixture. Direction `2` (`dx=1, dy=0`), size `2`, strength
`100`, scale `2`, depth `1`: pixel `(row 2, col 3)` has `toward`
sample `(2, 4)` at base height `1`, on checkerboard cell `((2/2) +
(4/2)) % 2 = 1` (odd, no bump), staying `1`; `away` sample `(2, 2)` at
base height `2`, on cell `((2/2) + (2/2)) % 2 = 0` (even, `+depth`),
becoming `3`. `relief = 3 - 1 = 2`, `shade = 2.0`, giving a real `(102,
152, 202)` — genuinely different from plain `bevel_emboss`'s own
`(101, 151, 201)` at these very same size/direction/strength, since
here the two sample points land on different cells. A second test
doubles depth to `2`, doubling the away sample's own bump for a real
`(103, 153, 203)`. A third narrows scale to `1`, putting both `(2, 4)`
and `(2, 2)` on the very same even cell so the bump cancels entirely,
landing exactly on plain `bevel_emboss`'s own `(101, 151, 201)` — a
real, hand-computed consequence of scale changing which cells the
samples fall on, not a coincidental match. A fourth halves strength to
`50` for a real `(101, 151, 201)`. A fifth confirms a fully-transparent
pixel is left completely alone. A sixth confines the fixture to a
single-pixel selection. A seventh confirms out-of-range size, light
direction, strength, scale, and depth, plus a locked/unknown layer,
all error. All seven tests passed on the first run, cross-checked
against an independent Python script that emulates Rust's own `f32`
rounding via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**771 Rust tests total** (764 → 771, 764 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 110 — Filter Gallery > Texture > Texturizer

`texturizer(id, scale, relief, light_direction, invert)` was
previously documented as a deferred gap ("needs a bump map this
project doesn't have the machinery for"), but the last three phases'
own height-field and checkerboard-cell work turned out to build
exactly that machinery. Unlike every Layer Style filter shipped so
far, Texturizer is a *global*, alpha-agnostic filter — Photoshop's own
version shades the whole layer's content regardless of transparency,
the same way `plaster`, `grain`, and `emboss` already apply globally
here rather than only near an alpha edge. The implementation composes
two ideas this project already has: a two-level checkerboard "height"
field using the exact `((row / scale) + (col / scale)) % 2` cell
formula `pattern_overlay` and `texture` already share (standing in for
Photoshop's own "Canvas" built-in texture, the same kind of
single-preset substitution `texture` itself already documents —
Photoshop's own Brick, Burlap, and Sandstone textures, and loading a
custom texture file, are a documented scope cut), and `emboss` /
`plaster`'s own "away − toward" relief convention, sampled one pixel
out along their shared 8-direction angle table. `shade = (height(away)
− height(toward)) * relief` is added to each colour channel (clamped),
exactly the same additive shape `bevel_emboss` already uses; alpha
always passes through untouched. `invert` swaps which checkerboard
cell counts as raised. `scale` (`1..=250`) shares `pattern_overlay`'s
own cell-size range; `relief` (`0..=50`, Photoshop's own dialog range)
is a per-channel intensity added directly, not a percent;
`light_direction` is `0..=7`. A new **Texturizer…** dialog exposes
Scale, Relief, the same eight-direction Light Direction dropdown, and
an Invert checkbox.

**Verified two ways.** Seven new `document.rs` tests, on a plain 4x4
solid grey `(100, 100, 100, 255)` layer rather than an alpha-based
fixture, since Texturizer's own shading depends only on position, not
on alpha or on the underlying colour's own value. Scale `2`, relief
`10`, direction `2` (`dx=1, dy=0`): the scale-2 checkerboard's own
height field across row `0` is `[1, 1, 0, 0]` (raised where `row/2 +
col/2` is even). Pixel `(row 0, col 1)` has `toward = height(0, 2) =
0` and `away = height(0, 0) = 1`, `relief = 1`, `shade = 10`, a real
`(110, 110, 110)`; pixel `(row 0, col 0)`, deep inside its own cell
once edge-clamped, has both neighbours at its own height, `relief =
0`, unchanged `(100, 100, 100)`; pixel `(row 2, col 1)`, one cell-row
down where the field flips, gives the opposite sign, a real `(90, 90,
90)`. A second test halves relief to `5` for a real `(105, 105, 105)`.
A third flips direction to `6` (180°), swapping toward and away for
the mirror `(90, 90, 90)`. A fourth sets `invert` true, which negates
the relief exactly like the direction flip does, landing on the very
same `(90, 90, 90)` through an entirely different mechanism — not a
coincidental match, a genuine consequence of inverting which cell
counts as raised. A fifth narrows scale to `1`, shrinking the
checkerboard to single pixels so pixel `(0, 1)`'s own two neighbours
both land on height `1` (the same as each other), cancelling the
relief to `0` — a real structural difference from the scale-`2` test's
own `(110, 110, 110)`, not just a smaller magnitude. A sixth confines
a single pixel to a selection. A seventh confirms out-of-range scale,
relief, and light direction, plus a locked/unknown layer, all error.
All seven tests passed on the first run, cross-checked against an
independent Python script that emulates Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-seven: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**778 Rust tests total** (771 → 778, 771 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 111 — Image > Adjustments > Auto Tone, Auto Contrast

`auto_tone(id)` and `auto_contrast(id)` both share a single private
`auto_stretch(id, shared)` implementation, following the same two-pass
sample-then-remap shape `equalize` already established: a first pass
samples the active selection (or the whole layer, with none — the same
sampling-region convention `equalize` already uses) to find each
channel's own minimum and maximum; a second pass linearly stretches
`out = (in - low) / (high - low) * 255`, clamped, the same per-channel
formula `levels` already applies with a user-typed `input_black`/
`input_white`, just with `low`/`high` computed automatically instead.
The one difference between the two filters is exactly what
distinguishes them in Photoshop itself: `auto_tone` (`shared = false`)
stretches each of the three channels using its own independently
sampled low/high, which can shift a colour cast; `auto_contrast`
(`shared = true`) takes the single darkest and lightest sampled values
across all three channels together and applies that one shared
low/high to every channel, which is exactly what keeps it from
shifting colour balance the way `auto_tone` can. A channel whose
sampled low equals its own high (or, for `auto_contrast`, whose shared
low equals the shared high) is left untouched rather than dividing by
zero, and a selection that samples nothing leaves the layer untouched
entirely. Alpha untouched throughout. Photoshop's own 0.5%-per-end
histogram clipping and Auto Color's own midtone-neutralizing "average
key" heuristic are both a documented scope cut — Auto Color in
particular is deferred rather than approximated, the same kind of
fabrication risk already documented for Color Lookup, since this
project doesn't have a defensible reference for Photoshop's own
proprietary neutralization formula.

**Verified two ways.** Five new `document.rs` tests, on a new
dedicated fixture (`varying_channels_fixture`, a 2x1 image: pixel 0 is
`(50, 100, 20)`, pixel 1 is `(150, 200, 220)`) chosen because each
channel has its own distinct range — a single-channel grayscale
fixture can't tell `auto_tone` and `auto_contrast` apart, since a
shared low/high over identical per-channel ranges is just that same
range. The shared low/high across all three channels and both pixels
is `20` (blue at pixel 0) to `220` (blue at pixel 1): `auto_contrast`
gives pixel 0 `R (50-20)/200*255 = 38.25 -> 38`, `G (100-20)/200*255 =
102`, `B (20-20)/200*255 = 0`, and pixel 1 `R 165.75 -> 166`, `G 229.5
-> 230` (rounding away from zero), `B 255` — neither R nor G reaches
full black/white, since the shared range is wider than either
channel's own. `auto_tone` on the identical fixture instead stretches
each channel using its own two sampled values as low/high exactly, so
every channel of both pixels reaches pure `0` or pure `255` — a real,
hand-computed difference confirming the actual property distinguishing
the two filters. A third test, on a genuinely flat `(128, 128, 128)`
solid layer (chosen deliberately over a per-channel-different flat
layer like `(10, 20, 30)` solid, which is flat for `auto_tone`'s own
per-channel view but is *not* flat for `auto_contrast`'s own shared
view, since its shared low `10` and high `30` still genuinely differ —
a real distinction this test's own comment documents, caught by the
test suite itself when an earlier draft used exactly that fixture and
failed), confirms both filters leave a uniformly grey layer completely
unchanged. A fourth confines sampling and remapping to a single-pixel
selection. A fifth confirms a locked or unknown layer errors for both
filters. All five tests passed (after that one caught and corrected
fixture choice), cross-checked against an independent Python script
that emulates Rust's own `f32` rounding via `struct.pack`/`unpack`
round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-eight: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. Both new one-click toolbar buttons (**Auto Tone**,
**Auto Contrast**, following the same parameter-free pattern
**Equalize**'s own toolbar button already uses) were reviewed by hand
instead. Every other layer of this project's quality bar
(hand/script-verified Rust tests, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings`, `npm run build`) is fully green.

**783 Rust tests total** (778 → 783, 776 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 112 — Image > Adjustments > Replace Color

`replace_color(id, target, fuzziness, hue, saturation, lightness)`
composes two mechanisms this project already has rather than
inventing a colour-range mask from scratch: `hue_saturation`'s own
`rgb_to_hsl`/`hsl_to_rgb` round trip, reused directly, and the
Chebyshev-distance shape (`max` of per-channel absolute differences)
already used throughout this project's edge and colour searches — here
measuring each pixel's own distance to a chosen `target` colour instead
of to a transparent or opaque neighbour. A pixel's own `strength` fades
linearly from a full shift at an exact match (`distance = 0`) to no
shift at all once `distance` reaches `fuzziness`: `strength = (1.0 -
distance / fuzziness).clamp(0.0, 1.0)`, standing in for Photoshop's own
soft-edged colour-range mask; at `fuzziness = 0` this collapses to a
hard threshold, only an exact match getting the full shift. The final
colour is a linear blend between the pixel's own original RGB and its
fully `hue_saturation`-shifted version by that `strength`, so a
partially-matching pixel is only partially recoloured rather than
snapping fully on or off. `fuzziness` is Photoshop's own `0..=200`
dialog range; `hue`/`saturation`/`lightness` share `hue_saturation`'s
own ranges and its saturating-rather-than-erroring clamp convention.
Alpha untouched. Photoshop's own interactive eyedropper-driven swatch
building (plus/minus sampling, a live mask preview) is a documented
scope cut — `target` here is a single colour chosen once through a
colour picker, not built up interactively.

**Verified two ways.** Five new `document.rs` tests, on 3-pixel test
rows built directly rather than a shared fixture, since each test needs
its own specific distances from its own target colour. Target `(100,
100, 100)`, fuzziness `50`, lightness `-100` (which always shifts to
pure black, since HSL lightness `0` is black regardless of hue or
saturation): pixel `0`, an exact match, fully replaces to `(0, 0, 0)`;
pixel `1`, `(130, 100, 100)`, sits a Chebyshev distance of `30` away,
giving `strength = 1 - 30/50 = 0.4` and a real, hand-computed `(78, 60,
60)`; pixel `2`, `(200, 100, 100)`, sits `100` away, past the
fuzziness-`50` cutoff, and is left byte-for-byte untouched. A second
test switches to target `(255, 0, 0)`, fuzziness `10`, hue `+120` (the
same shift `hue_shift_of_120_turns_pure_red_into_pure_green` already
verifies): an exact match fully shifts to `(0, 255, 0)`; `(255, 5, 5)`,
distance `5`, blends its own fully-shifted colour halfway with its own
original for a real `(130, 130, 5)`; `(255, 20, 20)`, distance `20`,
is untouched. A third test reuses the first test's own fixture and
target at fuzziness `0`, confirming only the exact match shifts at all
— a real, hand-computed difference from the fuzziness-`50` test's own
partial `(78, 60, 60)` blend, not a coincidental match. A fourth
confines the shift to a two-pixel selection. A fifth confirms
out-of-range fuzziness, plus a locked/unknown layer, all error. All
five tests passed on the first run, cross-checked against an
independent Python script that ports `rgb_to_hsl`/`hsl_to_rgb`/
`to_byte`/`to_unit` line-for-line and emulates Rust's own `f32`
rounding via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous fifty-nine: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new **Replace Color…** dialog (a target-colour
picker plus Fuzziness/Hue/Saturation/Lightness sliders, mirroring
Hue/Saturation's own dialog layout) was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**788 Rust tests total** (783 → 788, 781 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 113 — Filter > Blur > Radial Blur (Zoom method)

`radial_blur(id, amount, center_x, center_y)` implements Photoshop's
own Zoom method only, composing machinery this project already has:
[`sample_nearest`], the same nearest-neighbour edge-clamped resampling
primitive `ripple`/`twirl`/`pinch`/`spherize`/`glass` already share.
Each pixel takes exactly three samples along the line from
`(center_x, center_y)` through its own position, at scale factors
symmetric around `1.0` — `1.0 − blur`, `1.0`, and `1.0 + blur`, where
`blur = amount / 100.0` — pulling one sample inward toward the centre,
keeping one at the pixel's own position, and pushing one outward past
it, then averaging all three across all four channels (alpha included,
the same whole-pixel treatment `ripple`/`twirl` already give displaced
samples and the same per-channel averaging `box_blur_at`/
`motion_blur_at` already give blurred ones) — the classic "zoom trail"
look. A pixel sitting exactly at the centre has nothing to scale
(`dx = dy = 0`, every sample resolves to the same position) and stays
completely unchanged regardless of `amount`. `amount` is Photoshop's
own `0..=100` Amount range. Photoshop's own Spin method, its
Draft/Good/Best sample-count Quality dial (this project always takes
exactly three samples, a documented scope cut trading Photoshop's own
smoother many-sample average for a result a person can still verify by
hand), and its interactive on-canvas blur-center dial (`center_x`/
`center_y` are typed-in — well, slider-dragged — pixel coordinates
here, defaulting to the canvas centre the same way Lens Flare's own
Center X/Y sliders already default) are all a documented scope cut.

**Verified two ways.** Six new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture (3x3, R-only ramp 10 through
90 by tens, row-major). Centre `(1.0, 1.0)`, amount `50` (blur `0.5`,
scales `[0.5, 1.0, 1.5]`): pixel `(row 0, col 0)` (`dx = dy = -1`)
samples `(1, 1) = 50` at scale `0.5` (rounds there), itself, `(0, 0) =
10`, at scale `1.0`, and `(0, 0) = 10` again at scale `1.5` (rounds to
`(-1, -1)`, edge-clamped back to `(0, 0)`), averaging `70/3 = 23.33 ->
23`; pixel `(row 0, col 2)` (`dx = 1, dy = -1`) samples `60`, `30`, and
`30` for an exact `40`. A second test raises amount to `100` (blur
`1.0`, scales `[0.0, 1.0, 2.0]`) at the same pixel `(row 0, col 2)`:
the zero-scale sample now lands exactly on the centre, `(1, 1) = 50`,
giving `(50+30+30)/3 = 36.67 -> 37` — a real, hand-computed change from
the amount-`50` test's own `40`. A third moves the centre to the
opposite corner `(0.0, 0.0)` for pixel `(row 2, col 2)`, giving a real
`77` instead of that same pixel's own fully-clamped, unchanged `90`
when centred in the middle. A fourth confirms the centre pixel itself
stays exactly `50` even at amount `100`. A fifth confines a single
pixel to a selection. A sixth confirms out-of-range amount, a
non-finite centre coordinate (either axis), plus a locked/unknown
layer, all error. All six tests passed on the first run, cross-checked
against an independent Python script that emulates Rust's own `f32`
rounding via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty: this session's Xvfb
instance was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The new **Radial Blur…** dialog (Amount, Center X, Center
Y, the same layout shape Lens Flare's own dialog already uses) was
reviewed by hand instead. Every other layer of this project's quality
bar (hand/script-verified Rust tests, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings`, `npm run build`) is fully green.

**794 Rust tests total** (788 → 794, 787 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 114 — Filter > Sharpen > Smart Sharpen

`smart_sharpen(id, radius, amount, reduce_noise)` reuses
`unsharp_mask`'s own sharpening formula almost verbatim — `original +
(original - blurred) * amount`, clamped, with `blurred` still
`box_blur_at`'s own clamp-to-edge box-blur average — but drops the
Threshold gate entirely, matching Photoshop's own Smart Sharpen dialog,
which has no Threshold control at all. In its place, the sharpened
result blends back toward a `median_at`-denoised copy of the original
(the same median primitive `median` itself already uses) by
`reduce_noise` percent: `sharpened * (1.0 - frac) + denoised * frac`,
where `frac = reduce_noise / 100.0`. This is a documented, transparent
approximation of Photoshop's own proprietary noise-aware deconvolution
sharpening — composed entirely from two primitives this project
already has and has already verified independently, rather than
reverse-engineering Photoshop's own undocumented algorithm, the same
kind of honest substitution `chrome` and `glass` already make for
filters this project can't port exactly. `reduce_noise = 0` collapses
to plain `unsharp_mask` with no threshold; `reduce_noise = 100`
collapses to a pure median denoise, ignoring the sharpening pass
entirely — both are real, checkable identities, not just plausible-
sounding claims. The median denoise radius is fixed at `1`, a
documented simplification, since Photoshop's own Reduce Noise slider
has no separate radius control either. `radius` and `amount` share
`unsharp_mask`'s own ranges and error conditions; `reduce_noise` is
Photoshop's own `0..=100` dialog range. Alpha untouched.

**Verified two ways.** Six new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture. Radius `1`, amount `0.5`,
reduce noise `50`: pixel `(row 0, col 0)`'s own radius-1 box-blur
average is `210/9 = 23` (truncating), `diff = 10-23 = -13`, sharpened
`= 10 + (-13*0.5) = 3.5 -> 4`; its own radius-1 median, the middle of
the sorted window `[10,10,10,10,20,20,40,40,50]`, is `20`; blending
`4*0.5 + 20*0.5 = 12`. Pixel `(row 2, col 2)`'s own box-blur average is
`690/9 = 76`, sharpened `= 90 + 14*0.5 = 97`, median `80`, blending
`97*0.5 + 80*0.5 = 88.5 -> 89` (rounding away from zero). A second test
drops reduce noise to `0` at the first pixel, landing exactly on the
sharpened value alone, `4` — confirming the `reduce_noise = 0`
identity for real rather than by assertion. A third raises reduce
noise to `100`, landing exactly on the median value alone, `20` —
confirming the other identity. A fourth doubles amount to `1.0`,
pushing the sharpened half of the blend to a clamped `0` and the final
blend to `10`, a real, hand-computed change from the amount-`0.5`
test's own `12`. A fifth confines a single pixel to a selection. A
sixth confirms out-of-range radius, a non-positive or non-finite
amount, out-of-range reduce noise, plus a locked/unknown layer, all
error. All six tests passed on the first run, cross-checked against an
independent Python script emulating Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

While writing this filter's own doc comment, `cargo clippy`'s
`doc_lazy_continuation` lint caught a wrapped formula line beginning
with `* (reduce_noise / 100)` — the same class of false "unindented
markdown list item" `color_overlay`'s own doc comment hit back in
Phase 100 — fixed the same way, by rewording the formula so no line
starts with a bare `*`.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new **Smart Sharpen…** dialog (Amount, Radius,
and a Reduce Noise slider in place of Unsharp Mask's own Threshold,
mirroring its dialog layout otherwise) was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**800 Rust tests total** (794 → 800, 793 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 115 — Image > Adjustments > Match Color

`match_color(id, source_layer_id, fade)` is a standard mean/standard-
deviation colour transfer between two layers of the same document —
this project has no separate "open documents" concept a source image
could come from, so `source_layer_id` names another layer in the same
document instead, the same substitution `displace`'s own `map_layer_id`
already makes for Photoshop's own separate displacement-map file. A
new private free function, `channel_mean_std`, computes each of the
three RGB channels' own population mean and standard deviation across
an entire layer's pixels in two passes (means first, then the sum of
squared deviations from those means) — genuinely new machinery, since
nothing existing needed a whole-layer statistical summary before.

For each of the three channels independently: `normalized = (v -
target_mean) / target_std` expresses a target pixel's own channel
value as how many standard deviations it sits from its own layer's
mean; `matched = normalized * source_std + source_mean` re-expresses
that same relative position in the source layer's own distribution —
carrying over the source's brightness level and its contrast/
saturation "shape" together, without needing an image outside this
document. The final value blends the original toward `matched` by
`fade` percent, `orig * (1 - fade/100) + matched * (fade/100)`, so
`fade = 0` is the identity and `fade = 100` is a full match. A channel
whose own target standard deviation is `0` (every sampled pixel
identical) treats `normalized` as `0` rather than dividing by zero,
landing exactly on the source's own mean for that channel. Statistics
are always computed from each layer's own entire pixel data, matching
Photoshop's own default of measuring the whole source and target
images; only the final remap respects the target's own active
selection. This is a real, well-established statistical technique
(mean/standard-deviation transfer), not a guess at Photoshop's own
proprietary algorithm — Photoshop's own separate Luminance and Color
Intensity sliders, its Neutralize checkbox, and its Image Statistics
panel (loading saved source statistics rather than reading a live
layer) are all a documented scope cut, folded into this one `fade`
control. A new **Match Color…** dialog offers a Source Layer dropdown
(the same "pick another layer in this document" pattern Displace's own
Displacement Map dropdown already uses) and a Fade slider.

**Verified two ways.** Seven new `document.rs` tests, on a new
dedicated two-layer fixture (`two_layer_doc`, each a 2x2 layer with its
own R values, G and B fixed at `0` in both so those channels' own means
and standard deviations always match exactly and stay untouched,
keeping every test focused on R alone). Target R `[50, 50, 150, 150]`
(mean `100`, population std `50`); source R `[100, 100, 200, 200]`
(mean `150`, the identical std `50`, just shifted). At fade `100`,
every target pixel's own normalized position re-expressed in the
source's own distribution adds exactly the `50`-point mean shift:
`[100, 100, 200, 200]`. A second test halves fade to `50`, landing
exactly halfway between the shift and the original for a real `[75,
75, 175, 175]`. A third narrows the source to half the spread (`std
25`), scaling the target's own `±1` standard deviation down to `±25`
in the source's own narrower distribution for a real, hand-computed
compression, `[125, 125, 175, 175]`, distinct from the ratio-`1` test's
own plain shift. A fourth flattens the target channel to a single
value (`std 0`), landing every pixel exactly on the source's own mean
regardless of its own original value. A fifth flattens the *source*
channel instead, collapsing every target pixel onto the source's own
flat value — a real, distinct outcome from the flat-target case. A
sixth confines a single pixel to a selection. A seventh confirms
out-of-range fade, an unknown source layer, an unknown target layer,
and a locked target layer, all error. All seven tests passed on the
first run, cross-checked against an independent Python script that
emulates Rust's own `f32` rounding via `struct.pack`/`unpack`
round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's layer-picker wiring (mirroring
Displace's own already-working dropdown) was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**807 Rust tests total** (800 → 807, 800 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 116 — Filter > Noise > Reduce Noise (Basic mode)

`reduce_noise(id, strength, preserve_details)` reuses `median_at`'s
own denoise (the same primitive `median` and `smart_sharpen`'s own
Reduce Noise sub-control already use), fixed at radius `1`, blended
toward the original by a fraction driven by two sliders together:
`blend = (strength / 10.0) * (1.0 - preserve_details / 100.0)`, where
`strength` is Photoshop's own Basic-mode `0..=10` range and
`preserve_details` its own `0..=100` range. Raising `preserve_details`
pulls the effective blend back down regardless of `strength`, and
`strength = 0` or `preserve_details = 100` both collapse to the
identity. Photoshop's own Reduce Noise dialog genuinely exposes only
these two sliders in its default Basic mode — this isn't a narrowed
approximation of the real dialog, it's a direct port of that same
default view. Advanced mode's separate per-channel Strength dial,
Reduce Color Noise, and Sharpen Details sliders are all a documented
scope cut. Alpha untouched. A new **Reduce Noise…** dialog exposes
Strength and Preserve Details.

**Verified two ways.** Six new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture and its own already-verified
radius-1 median at pixel `(row 0, col 0)`, `20` (the same value
`smart_sharpen`'s own tests already derive for this pixel). Strength
`10` (maximum), preserve details `0`: `blend = 1.0`, landing exactly
on the median, `20`. A second test halves strength to `5`, landing
halfway between the original `10` and the median `20` for a real `15`.
A third instead keeps strength at `10` but raises preserve details to
`50`, reaching that very same `15` through a completely different
route — `1.0 * (1 - 0.5) = 0.5`, the identical blend fraction —
confirming the two sliders genuinely multiply together rather than one
silently overriding the other. A fourth confirms strength `0` is a
byte-for-byte identity. A fifth confines a single pixel to a
selection. A sixth confirms out-of-range strength and preserve
details, plus a locked/unknown layer, all error. All six tests passed
on the first run, cross-checked against an independent Python script
emulating Rust's own `f32` rounding via `struct.pack`/`unpack`
round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-three: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**813 Rust tests total** (807 → 813, 806 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 117 — Camera Raw Filter > Temperature/Tint

`temperature_tint(id, temperature, tint)` is a direct per-channel shift
standing in for Photoshop's own colour-science-based white-balance
model: `temperature` adds directly to red and subtracts from blue
(positive warms the image toward orange, negative cools it toward
blue, the same "blue versus yellow" axis Camera Raw's own slider
describes), while `tint` adds directly to green alone (the "green
versus magenta" axis), each clamped to `0..=255`. Both sliders share
Photoshop's own `-100..=100` Camera Raw range, clamped rather than
erroring on an out-of-range value, the same saturating convention
`brightness_contrast` already uses. Photoshop's own Temperature slider
works in absolute Kelvin relative to a raw file's own embedded native
white balance — a concept this project has no raw-metadata source
for — so this is a documented linear approximation rather than that
colour-science model, the same kind of honest substitution `chrome`
and `glass` already make elsewhere for filters this project can't port
exactly. Alpha untouched. A new **Temperature/Tint…** dialog exposes
both sliders.

**Verified two ways.** Eight new `document.rs` tests, entirely
integer arithmetic (every shift is a whole-number add or subtract, so
`.round()` is always a no-op — no Python cross-check needed for this
one, just direct hand arithmetic). Temperature `+50`, tint `-30` on
`(100, 150, 200)`: red `100+50=150`, green `150-30=120`, blue
`200-50=150`, giving `(150, 120, 150)`. A second test flips temperature
to `-50`, the mirror image at the very same original pixel: `(50, 150,
250)`. A third confirms clamping at both channel bounds:
`(240, 10, 240)` with temperature `+50` and tint `-50` gives
`(255, 0, 190)` (red saturates high, green saturates low). A fourth
confirms slider values past `±100` saturate at `±100` rather than
erroring, matching `500`/`-500` against `100`/`-100`'s own identical
result. A fifth confirms alpha stays untouched. A sixth confines the
shift to a two-pixel selection. A seventh and eighth confirm a locked
or unknown layer both error.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**821 Rust tests total** (813 → 821, 814 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 118 — Camera Raw Filter > Highlights/Shadows

`highlights_shadows(id, highlights, shadows)` reuses `color_balance`'s
own luma-based tonal-range weighting directly — `shadow_weight =
clamp((127.0 - luma) / 127.0, 0.0, 1.0)` and `highlight_weight =
clamp((luma - 128.0) / 127.0, 0.0, 1.0)` — but with a single uniform
shift per range instead of three independent per-channel sliders,
since Camera Raw's own Highlights and Shadows sliders don't retint,
they only brighten or darken. `highlight_weight * highlights +
shadow_weight * shadows` is added identically to all three RGB
channels, so colour balance is preserved exactly the way
`brightness_contrast` preserves it; a pure midtone pixel (luma
`127`/`128`) has both weights at `0` and passes through completely
untouched, tapering smoothly to a full shift at pure black
(`shadow_weight = 1`) or pure white (`highlight_weight = 1`). Both
sliders share Photoshop's own `-100..=100` Camera Raw range, clamped
rather than erroring on an out-of-range value. Alpha untouched. A new
**Highlights/Shadows…** dialog exposes both sliders.

**Verified two ways.** Nine new `document.rs` tests. Pure black `(0, 0,
0)` (100% shadow weight, 0% highlight weight) with shadows `+50`:
every channel lifts to `50`, highlights having no effect at all. Pure
white `(255, 255, 255)` (the mirror case) with highlights `-50`: every
channel darkens to `205`. A pure midtone pixel, `(128, 128, 128)`
(luma exactly `128.0`, since the BT.601 weights sum to `1.0`), has
both weights at `0` and stays completely untouched even at both
sliders' own maximum magnitude, `100`. A genuinely coloured shadow
pixel, `(10, 20, 30)` (luma `18.15`, `shadow_weight = 0.8571`), with
shadows `+100`: the identical shift, `85.71`, is added to every
channel, giving `(96, 106, 116)` — and critically, the original
step-of-`10` spacing between channels survives exactly
(`p[1]-p[0] = 10`, `p[2]-p[1] = 10`), confirming colour balance isn't
retinted the way three independent per-channel sliders could. A fifth
test confirms slider values past `±100` saturate rather than erroring.
A sixth confirms alpha stays untouched. A seventh confines the shift
to a two-pixel selection. An eighth and ninth confirm a locked or
unknown layer both error. All nine tests passed on the first run,
cross-checked against an independent Python script that ports the
exact luma/weight formulas and emulates Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

While writing this filter's own doc comment, `cargo clippy`'s
`doc_lazy_continuation` lint caught a wrapped formula line beginning
with `- luma) / 127, 0, 1)` — a line starting with `- ` reads as an
unindented markdown bullet, the same class of false positive
`smart_sharpen`'s own doc comment hit in Phase 114 and `color_overlay`'s
hit back in Phase 100 — fixed the same way, by rewording the formula so
no line starts with a bare `-` or `*`.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**830 Rust tests total** (821 → 830, 823 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 119 — Camera Raw Filter > Clarity

`clarity(id, amount)` reuses `unsharp_mask`'s own "subtract a blurred
copy, add the difference back in, amplified" shape and its very same
`box_blur_at` low-pass, but fixes the radius at a large `40` instead of
a user-adjustable one — Photoshop's own Clarity slider works this way
internally too, at a fixed large radius the dialog never exposes,
boosting *local* (midtone) contrast rather than fine edge detail the
way a small-radius sharpen does. Unlike `unsharp_mask`, `amount` here
is signed (Photoshop's own `-100..=100` Clarity range, clamped rather
than erroring): positive values boost local contrast exactly like a
sharpen; negative values soften it instead, blending a pixel toward
its own broad neighbourhood average — a "reverse sharpen"
`unsharp_mask`'s own positive-only `amount` can't express. `out =
original + (original - blurred) * (amount / 100.0)`, clamped, per RGB
channel; alpha untouched. A new **Clarity…** dialog exposes the single
signed slider.

**Verified two ways.** Eight new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture. Radius `40` vastly exceeds
the 3x3 canvas, so every pixel's own box-blur average clamps heavily
toward the grid's own edges: pixel `(row 0, col 0)`'s own radius-40
average comes out to `49`, pixel `(row 2, col 2)`'s own to `50`.
Amount `20` (frac `0.2`): pixel `(0, 0)`, `diff = 10-49 = -39`, `out =
10 + (-39*0.2) = 2.2 -> 2` — local contrast pulls this corner pixel
further from its own neighbourhood average; pixel `(2, 2)`, `diff =
90-50 = 40`, `out = 90 + 40*0.2 = 98` — the opposite direction, both
real and hand-computed. A second test halves amount to `10` for a real
`6` at the same pixel. A third flips to amount `-50`, pulling the
pixel toward its own neighbourhood average instead of away from it:
`10 + (-39*-0.5) = 29.5 -> 30` (rounding away from zero) — the reverse-
sharpen direction confirmed for real. A fourth confirms amount clamps
at `±100` rather than erroring. A fifth confirms alpha stays
untouched. A sixth confines a single pixel to a selection. A seventh
and eighth confirm a locked or unknown layer both error. All eight
tests passed on the first run, cross-checked against an independent
Python script emulating Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**838 Rust tests total** (830 → 838, 831 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 120 — Camera Raw Filter > Optics > Defringe

`defringe(id, amount)` desaturates pixels in proportion to how close
they sit to a high-contrast edge, composing machinery this project
already has: a luma buffer (BT.601 weights, the same as `threshold`
and `black_and_white` use), `sobel_at`'s own edge-magnitude convolution
(the same one `colored_pencil` and several Sketch-gallery filters
already run over a luma buffer), and `rgb_to_hsl`/`hsl_to_rgb` (the
same round trip `hue_saturation` already uses). A pixel's own
`edge_strength` — the luma buffer's own Sobel magnitude, `0..=255`,
scaled to `0.0..=1.0` — sets how much of `amount` actually applies
there: `desaturation = edge_strength * (amount / 100.0)` shrinks that
pixel's own HSL saturation by that fraction, leaving hue and lightness
alone. A flat area (no nearby edge) is left completely untouched
regardless of `amount`, and an edge pixel loses more saturation the
sharper that edge is.

This is a documented broadening of Photoshop's own Defringe, which
targets specifically purple- and green-hued fringing near edges with
separate Amount/Hue sliders for each colour — picking defensible
purple/green hue-range boundaries without a strong photographic
reference risks fabricating Photoshop's own exact thresholds, the same
fabrication risk already documented for Color Lookup and Auto Color.
Rather than inventing those boundaries, this desaturates near *any*
high-contrast edge instead of only purple/green ones — a broader but
honestly-scoped substitute. `amount` is Photoshop's own `0..=100`
per-colour Amount range, applied once rather than separately per
fringe colour. A new **Defringe…** dialog exposes the single slider.

**Verified two ways.** Five new `document.rs` tests, on a new
dedicated fixture (`defringe_fixture`, 4x4: columns 0-1 a saturated
pinkish `(150, 90, 90, 255)`, luma `108`; columns 2-3 a saturated green
`(30, 200, 30, 255)`, luma `130` — the luma cliff between columns 1 and
2 gives the Sobel magnitude a real, non-zero response right at that
boundary and `0` everywhere else). Amount `100`: pixel `(row 0, col
1)`, right at the boundary, has Sobel magnitude `88`, `edge_strength =
0.34510`; its own HSL saturation `0.25` shrinks to `0.16373`,
round-tripping to `(140, 100, 100)`. Pixel `(row 0, col 2)`, the
boundary's other side, has the identical Sobel magnitude, its own
saturation `0.73913` shrinking to `0.48406`, giving `(59, 171, 59)`.
Pixels `(row 0, col 0)` and `(row 0, col 3)`, each two columns from the
boundary, have a fully uniform 3x3 window (Sobel magnitude `0`) and are
left completely untouched. A second test halves amount to `50`,
giving real, less-desaturated `(145, 95, 95)` and `(45, 185, 45)` at
the same two boundary pixels. A third confirms amount `0` is a
byte-for-byte identity. A fourth confines a single pixel to a
selection. A fifth confirms out-of-range amount, plus a locked/unknown
layer, all error. All five tests passed on the first run, cross-checked
against an independent Python script that ports the exact luma/Sobel/
HSL formulas and emulates Rust's own `f32` rounding via
`struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-seven: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**843 Rust tests total** (838 → 843, 836 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 121 — Filter Gallery > Blur Gallery > Tilt-Shift

`tilt_shift(id, focus_row, half_height, blur_radius)` (horizontal band
only) is a gradient blur that keeps a horizontal band around
`focus_row` perfectly sharp and blurs everything else by `box_blur_at`
at up to `blur_radius`, ramping smoothly in between — the classic
"miniature diorama" look. A pixel's own vertical `distance` from
`focus_row` is compared against `half_height` (rows within that
distance stay fully sharp, `blend = 0`) and a `blur_radius`-row
transition beyond it (`blend = (distance - half_height) / blur_radius`,
clamped to `0.0..=1.0`, reaching a full blur at `blur_radius` rows past
the sharp band); the final colour is `original * (1 - blend) + blurred
* blend` per RGB channel, alpha untouched. Photoshop's own version lets
the sharp band run at any angle and gives each of its two feather
rings an independently draggable width, plus a separate Distortion
slider; here the band is always horizontal and the feather width is
tied directly to `blur_radius` — both documented scope cuts, along
with Field Blur and Iris Blur (Blur Gallery siblings with their own
arbitrary-point or elliptical falloff shapes, not this one's single
horizontal band). A new **Tilt-Shift…** dialog exposes Focus Row,
Sharp Band Half-Height, and Blur Radius, defaulting the focus row to
the canvas's own vertical centre when the dialog opens.

**Verified two ways.** Four new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture. Focus row `1`, half-height
`0` (only row `1` itself is fully sharp), blur radius `2`: row `1`'s
own distance from the focus row is `0`, so `blend = 0` and every pixel
in that row is left byte-for-byte at its own original value, `(40, 50,
60)`. Row `0` and row `2` each sit a distance of `1` from the focus
row, giving `blend = (1-0)/2 = 0.5`, blending each pixel halfway with
its own radius-2 box-blur average — row `0`'s own blurred row is `(34,
38, 42)`, halfway to its own original `(10, 20, 30)` giving `(22, 29,
36)`; row `2`'s own blurred row is `(58, 62, 66)`, halfway to its own
original `(70, 80, 90)` giving `(64, 71, 78)`. All six values
hand-computed and cross-checked in Python. A second test widens
half-height to `1`, now covering rows `0` and `2` as well, leaving the
entire image untouched — a real, hand-computed difference from the
half-height-`0` test's own blended rows. A third confines a single
pixel to a selection. A fourth confirms a zero blur radius, plus a
locked/unknown layer, all error. All four tests passed on the first
run, cross-checked against an independent Python script emulating
Rust's own `f32` rounding via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-eight: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**847 Rust tests total** (843 → 847, 840 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 122 — Filter Gallery > Blur Gallery > Iris Blur

`iris_blur(id, center_x, center_y, radius, blur_radius)` (circular
only) reuses `tilt_shift`'s own gradient-blur shape, but the sharp zone
is a circle around `(center_x, center_y)` instead of a horizontal
band — a pixel's own Euclidean `distance` from that centre is compared
against `radius` (within it, `blend = 0`, fully sharp) and a
`blur_radius`-pixel transition beyond it (`blend = (distance - radius)
/ blur_radius`, clamped to `0.0..=1.0`), blending toward a
`box_blur_at` average by that same fraction per RGB channel; alpha
untouched. Photoshop's own Iris Blur lets the ellipse be stretched and
rotated and gives it four independently draggable feather handles
rather than one uniform ring; this project's own circle-only, single-
radius version is a documented scope cut, the same kind of narrowing
`tilt_shift`'s own horizontal-only band already makes relative to
Photoshop's arbitrary-angle one. A new **Iris Blur…** dialog exposes
Center X, Center Y, Sharp Radius, and Blur Radius, defaulting the
centre to the canvas's own middle when the dialog opens.

**Verified two ways.** Four new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture. Centre `(1.0, 1.0)` (the
grid's own middle pixel), radius `0.0`, blur radius `2`: the centre
pixel `(1, 1)` sits at distance `0`, so `blend = 0` and it's left
byte-for-byte at its own original `50`. Pixel `(1, 0)` sits a Euclidean
distance of `1.0` away, giving `blend = 1.0/2 = 0.5`, blending its own
original `20` halfway with its own radius-2 box-blur average, `38`,
for a real `29`. Pixel `(0, 0)`, a diagonal distance of `sqrt(2) =
1.41421` away, gives `blend = 0.70711`, blending its own original `10`
with its own average `34` for a real `27`; pixel `(2, 2)`, the opposite
diagonal corner, blends its own original `90` with its own average
`66` for a real `73`. All four values hand-computed and cross-checked
in Python. A second test widens radius to `2.0`, now covering every
pixel in the 3x3 grid (the farthest corner sits only `sqrt(2) =
1.41421` away, under `2.0`), leaving the entire image untouched — a
real, hand-computed difference from the radius-`0` test's own blended
pixels. A third confines a single pixel to a selection. A fourth
confirms a zero blur radius, a non-finite centre coordinate, a negative
radius, and a locked/unknown layer, all error. All four tests passed
on the first run, cross-checked against an independent Python script
emulating Rust's own `f32` rounding via `struct.pack`/`unpack`
round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous sixty-nine: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**851 Rust tests total** (847 → 851, 844 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 123 — Filter Gallery > Blur Gallery > Field Blur

`field_blur(id, x1, y1, radius1, x2, y2, radius2)` (two pins only)
completes this project's own Blur Gallery trio alongside Tilt-Shift and
Iris Blur, but with a genuinely different mechanism: rather than
blending toward one *fixed* blur radius past a hard zone boundary, the
blur radius itself varies continuously across the whole image. A pixel
exactly at a pin's own position uses that pin's own `radius` outright;
every other pixel's own blur radius is an inverse-distance-weighted
average of both pins' radii — `weight = 1.0 / distance` to each pin,
`radius = (weight1 * radius1 + weight2 * radius2) / (weight1 +
weight2)`, rounded to the nearest whole pixel — and `box_blur_at` is
run at that pixel's own interpolated radius, its RGB channels alone
copied into the output (alpha untouched, the same convention
`tilt_shift` and `iris_blur` already keep). Photoshop's own Field Blur
accepts an arbitrary number of draggable pins with spline-smoothed
falloff between them; this project's own two-pin, inverse-distance-
weighted version is a documented scope cut trading Photoshop's own
richer interpolation for a simple, well-known, and exactly hand-
verifiable one. A new **Field Blur…** dialog exposes each pin's own
X/Y position and blur radius, defaulting the two pins to opposite
quarter-points of the canvas when the dialog opens.

**Verified two ways.** Four new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture. Pin 1 at `(0.0, 0.0)` with
radius `0` (no blur at all); pin 2 at `(2.0, 2.0)` with radius `4`.
Pixel `(row 0, col 1)` sits distance `1.0` from pin 1 and `sqrt(5) =
2.23607` from pin 2; `weight1 = 1.0`, `weight2 = 0.44721`, interpolated
radius `= (1.0*0 + 0.44721*4) / 1.44721 = 1.23607`, rounding to `1`;
its own radius-1 box-blur average is `30`. Pixel `(row 1, col 2)` sits
distance `1.0` from pin 2 and `sqrt(5)` from pin 1; interpolated radius
rounds to `3`, its own radius-3 average is `52`. Pixel `(0, 0)`, exactly
at pin 1's own position, uses radius `0` outright (the short-circuit,
not the IDW formula), leaving it byte-for-byte at its own original
`10`. Pixel `(2, 2)`, exactly at pin 2's own position, uses radius `4`
outright, giving `58`. All four hand-computed and cross-checked in
Python. A second test swaps the two pins' own radii, changing the
interpolated radius at both boundary pixels to real, distinct values
(`41` and `56`) — confirming the interpolation genuinely depends on
which pin holds which radius, not a coincidental match. A third
confines a single pixel to a selection. A fourth confirms a non-finite
pin coordinate, plus a locked/unknown layer, all error. All four tests
passed on the first run, cross-checked against an independent Python
script emulating Rust's own `f32` rounding via `struct.pack`/`unpack`
round-tripping.

While writing this filter, `cargo clippy`'s `manual_memcpy` lint caught
a hand-written three-iteration copy loop (`for c in 0..3 { layer.pixels
[dst+c] = blurred[c]; }`) that it could express as a single
`copy_from_slice` call instead; rewritten as suggested, copying only
the RGB slice and leaving alpha untouched.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**855 Rust tests total** (851 → 855, 848 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 124 — Filter Gallery > Blur Gallery > Spin Blur

`spin_blur(id, center_x, center_y, angle)` closes out this project's
own Blur Gallery quartet (Tilt-Shift, Iris Blur, Field Blur, and now
Spin Blur), and is a direct sibling of `radial_blur` (Phase 113) rather
than of the other three: it reuses `radial_blur`'s own three-sample
averaging shape exactly, but *rotates* each pixel's own offset from a
centre point instead of scaling it — the classic spinning-wheel motion
blur. A pixel's own `(dx, dy)` offset from `(center_x, center_y)` is
rotated by three angles symmetric around `0°` — `-angle/2`, `0°`, and
`+angle/2`, where `angle` is the total rotation span in degrees — each
rotated offset resampled via the same edge-clamped, nearest-neighbour
`sample_nearest` primitive `ripple`/`twirl`/`radial_blur` already
share, and the three samples averaged across all four channels, alpha
included, exactly as `radial_blur` already averages its own three zoom
samples. A pixel sitting exactly at the centre has a zero-length
offset, which rotation leaves at zero regardless of `angle`, so it
stays completely unchanged; `angle = 0°` collapses all three rotated
samples back onto the pixel's own position, the identity. `angle` is
validated against Photoshop's own `0..=360` Blur Angle range.
Photoshop's own Spin Blur also lets the blur ellipse be stretched
independently of rotation and offers a Strobe Effect option; this
project's own circular, three-sample version is a documented scope
cut, the same kind of narrowing `radial_blur`'s own fixed three-sample
count already makes. A new **Spin Blur…** dialog exposes the centre
X/Y position (defaulting to the canvas middle) and the angle as a
0-360 slider.

**Verified two ways.** Six new `document.rs` tests, reusing the
box-blur suite's own `ramped_3x3` fixture and its R-only ramp. Centred
at `(1.0, 1.0)` with `angle = 90` (half-angle `45°` either way), pixel
`(row 0, col 2)`'s own offset from the centre is `(1, -1)`: rotating by
`-45°` lands the sample at `(1, 0)` (value `20`); by `0°` at the
pixel's own position, `(2, 0)` (value `30`, itself); by `+45°` at
`(2, 1)` (value `60`). Average `(20 + 30 + 60) / 3 = 36.67`, rounding
to `37` — a real, hand-computed change from the pixel's own original
`30`. Raising `angle` to `180` (half-angle `90°`) rotates further,
giving a distinct, hand-computed `43` — a genuine change from the
angle-90 test's own `37`, not a coincidental match confirming the
angle genuinely scales the rotation rather than being ignored. A third
test confirms `angle = 0` is a byte-for-byte identity (all three
rotated samples collapse onto the same original position). A fourth
confirms the centre pixel itself, `(1, 1)`, stays exactly `50`
regardless of angle, since its own zero-length offset rotates to
itself. A fifth confines the same `90°` rotation to a single-pixel
selection, confirming unselected pixels stay byte-for-byte untouched.
A sixth confirms non-finite centre coordinates, an out-of-range angle
(both below `0` and above `360`), plus a locked/unknown layer, all
error. All six tests passed on the first run, cross-checked against an
independent Python script emulating Rust's own `f32` trigonometry and
rounding via `struct.pack`/`unpack` round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**861 Rust tests total** (855 → 861, 854 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 125 — Filter > Blur > Lens Blur

`lens_blur(id, max_radius, invert)` is a depth-of-field blur whose
radius varies per pixel, and the last of the Blur menu's own
depth-aware entries. Photoshop's own Lens Blur reads its depth map
from a separate input (a chosen alpha channel, a layer mask, or the
layer's transparency); this project's own version uses the layer's own
alpha channel directly as that depth map, so no second input needs to
be plumbed through. Each pixel's own blur radius is `round(depth / 255
* max_radius)`, where `depth` is the pixel's own alpha byte — or `255 -
alpha` when `invert` is set, matching Photoshop's own Invert checkbox
on the depth map — and the result is run through the same edge-clamped
`box_blur_at` primitive `box_blur`/`tilt_shift`/`iris_blur`/
`field_blur` already share. Only the RGB channels are written back;
alpha itself is left byte-for-byte untouched, since it *is* the depth
map driving the blur and must survive unchanged for the effect to mean
anything (the same alpha-untouched convention the three Blur Gallery
filters already keep for their own reasons). Fully transparent pixels
(depth `0`) always get radius `0` and stay exactly sharp; fully opaque
ones blur at the full `max_radius`. `max_radius` is validated against
Photoshop's own Lens Blur Radius range, `0..=100`. Photoshop's own
Lens Blur also shapes its blur kernel by an adjustable iris (blade
count, curvature, rotation), adds specular highlights past a
brightness threshold, and can layer a film-grain pass back in
afterward; this project's own flat, alpha-driven box blur is a
documented scope cut, trading that richer bokeh simulation for one
exactly hand-verifiable mechanism built entirely from an
already-tested primitive. A new **Lens Blur…** dialog exposes the
Radius slider and an Invert checkbox, with a one-line hint explaining
that the layer's own alpha is the depth map.

**Verified two ways.** Six new `document.rs` tests on a new
`depth_ramped_3x3` fixture: `ramped_3x3`'s own R-only ramp (`10` to
`90`), but with alpha standing in for depth — column `0` fully near
(`0`), column `1` halfway (`128`), column `2` fully far (`255`). With
`max_radius = 4`, pixel `(col 2, row 1)` (alpha `255`) gets radius
`round(255/255 * 4) = 4`, whose edge-clamped window over the 3x3 grid
averages (truncating, as `average_samples` does) to `52` — a real
change from its own original `60` — while every alpha byte survives
unchanged. A second test takes pixel `(col 1, row 0)` (alpha `128`,
original `20`) through two different maxima: `max_radius = 1` gives
`round(0.50196) = 1`, averaging to `30`; `max_radius = 4` gives
`round(2.0078) = 2`, averaging to `38` — two distinct values from the
same pixel, so the radius genuinely depends on `max_radius` rather
than coincidentally matching. A third confirms every column-`0` pixel
(alpha `0`, radius `0` at any maximum) is a byte-for-byte identity. A
fourth flips `invert`: pixel `(col 0, row 1)` now blurs from `40` to a
hand-computed `47` while pixel `(col 2, row 1)` (now depth `0`) stays
exactly `60`. A fifth confines the blur to a single-pixel selection,
checking both sides of the selected pixel stay untouched. A sixth
confirms a `101` radius, plus a locked/unknown layer, all error. All
six passed on the first run, cross-checked against an independent
Python script that emulates `box_blur_at`'s own clamp-and-truncate
averaging and Rust's own `f32` rounding — a script that, before being
trusted, was itself checked against the three radius-`1` values (`23`,
`50`, `76`) the box-blur suite had already hand-verified. That
cross-check earned its keep: an initial hand-derivation had assumed
any radius of `2` or more averages the whole 3x3 grid to `50` from any
pixel, which is only true from the centre — from an off-centre pixel
the clamped window repeats edge samples unevenly, and the script's
`52`/`38`/`47` corrected the assumption before it reached a test.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**867 Rust tests total** (861 → 867, 860 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 126 — Camera Raw Filter > Saturation

`camera_raw_saturation(id, saturation)` fills in the Camera Raw Basic
panel's own Saturation slider. Camera Raw pairs a Vibrance slider with
a Saturation slider exactly as Image > Adjustments > Vibrance does, and
this project's own `vibrance(id, vibrance, saturation)` already
implements both — so Camera Raw's Saturation is that same function
with its vibrance term held at `0`, which is an exact no-op on the
saturation (`s + 0 * (1 - s) = s`), leaving only the uniform `s * (1 +
saturation / 100)` HSL scale, clamped to `0..=1`, that `vibrance`'s
own second slider already applies. Every hue is scaled equally, which
is precisely what distinguishes Photoshop's own Saturation slider from
its Vibrance slider (which boosts the least-saturated colours most).
`saturation` is Photoshop's own `-100..=100` range, clamped rather
than erroring like `vibrance`'s own sliders. This is a preset over an
already-verified adjustment — the same kind of composition the
one-click Sharpen presets already make over `unsharp_mask` — and is
framed as such rather than as new colour math. A new **Saturation…**
button sits with the other Camera Raw Filter entries, opening a
single-slider dialog.

**Verified two ways.** Six new `document.rs` tests. `(200, 100, 100)`
is hue `0`, saturation `0.476190`, lightness `0.588235`; at `+50` the
saturation scales to `0.714286`, and back through `hsl_to_rgb` the
chroma is `(1 - |2l - 1|) * s = 0.588235` with `m = l - chroma/2 =
0.294118`, so `r = 0.882353` → `225` and `g = b = 0.294118` → `75`:
`(225, 75, 75)`. At `-50` the saturation halves to `0.238095`, chroma
`0.196078`, `m = 0.490196`, so `r = 0.686275` → `175` and `g = b` →
`125`: `(175, 125, 125)`, a hand-computed pull toward grey. A third
test confirms a neutral grey is unchanged at `+100` (zero saturation
scaled by anything is still zero). A fourth confirms, byte-for-byte on
the `ramped_3x3` fixture, that the preset equals `vibrance(id, 0, 50)`
exactly and that `9999` clamps to the same result as `100`. A fifth
confines the shift to a one-pixel selection. A sixth confirms a
locked/unknown layer errors. All six passed on the first run, the two
colour values cross-checked against the independent Python
`rgb_to_hsl`/`hsl_to_rgb` port (emulating Rust's own `f32` arithmetic)
already used to verify Replace Color and Defringe.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**873 Rust tests total** (867 → 873, 866 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 127 — Camera Raw Filter > Histogram

`histogram(id)` is this project's first read-only analysis command: the
per-channel distribution of a layer's own pixel values, 256 bins for
each of R, G, and B, sampled over the active selection or the whole
layer with none — Camera Raw's own Histogram panel (and Window >
Histogram's own RGB view). It is exactly the sampling `equalize` has
built its own remap table from since Phase 28, factored out of
`equalize` into a shared private `layer_histogram` helper (which also
returns the sampled-pixel count `equalize` needs) rather than
re-derived, so the two can never disagree about what counts as a
sampled pixel. Nothing is modified, so the command works on a locked
layer too; only an unknown layer errors. Every sampled pixel counts
once regardless of its alpha, the same convention `equalize` already
keeps; weighting or excluding pixels by transparency is a documented
scope cut, as are Camera Raw's own luminance overlay and its
shadow/highlight clipping warnings. The Tauri command hands the counts
back as nested `Vec`s only because serde has no serializer for
256-element arrays. A new **Histogram…** button with the other Camera
Raw entries opens a dialog drawing the three channels as overlaid,
translucent filled curves in an SVG, each scaled against the tallest
bin across all three channels, with a one-line hint explaining what is
being counted.

**Verified two ways.** Four new `document.rs` tests on the box-blur
suite's own `ramped_3x3` fixture, whose values are known by
construction: its R channel holds `10, 20, …, 90` exactly once each
(so those nine bins each read `1` and every other R bin `0`, summing to
`9`), while its G and B channels hold `0` nine times (bin `0` reads `9`
in each, summing to `9`). Selecting column `2` alone leaves R bins
`30`, `60`, `90` at `1` each and the other six ramp bins at `0`, with G
and B bin `0` at `3`. A locked layer still yields its histogram (R bin
`50` reads `1`) and is left byte-for-byte untouched; an unknown layer
errors. The refactor of `equalize` onto the shared helper is guarded by
`equalize`'s own seven pre-existing tests, all still green. Counting
values in a fixture whose contents are written out literally needs no
arithmetic to cross-check, so no Python script was written this phase —
the "second way" is the fixture's own source listing.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-four: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring and SVG path construction
were reviewed by hand instead. Every other layer of this project's
quality bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings`, `npm run build`) is fully green.

**877 Rust tests total** (873 → 877, 870 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 128 — Camera Raw Filter > RGB Levels

`layer_pixel(id, x, y)` is the second read-only query, the natural
companion to Phase 127's histogram: the RGBA8 value of a layer's own
pixel at `(x, y)` — the per-channel readout Camera Raw shows under its
histogram for the pixel beneath the pointer. Unlike the eyedropper's
`sample_color`, which reads the flattened composite, this reads the one
layer's own stored bytes, alpha included, so what it reports is exactly
what `histogram` is counting for that layer. It is read-only (a locked
layer is fine) and errors on an unknown layer or a point outside the
canvas. On the frontend, moving the pointer over the canvas with a layer
selected now asks for the pixel under it and shows `R G B A` in the
status bar, cleared when the pointer leaves. Two small guards keep the
readout honest and cheap: the request is skipped while the pointer stays
on the same document pixel of the same layer (one round-trip per pixel
crossed, not per mouse event), and that memo is reset whenever the
composite is redrawn, so a repaint under a stationary pointer refreshes
the value instead of showing the pre-edit byte until the pointer moves.
Camera Raw's own readout is shown as percentages in its Lab/percentage
modes and can be pinned per colour sampler; this project's own single
live 8-bit readout is a documented scope cut.

**Verified two ways.** Four new `document.rs` tests, again on fixtures
whose contents are written out literally. `ramped_3x3`'s `(col 2, row
1)` reads `[60, 0, 0, 255]`, `(0, 0)` reads `[10, 0, 0, 255]`, `(1, 2)`
reads `[80, 0, 0, 255]`. Phase 125's `depth_ramped_3x3` shows the
alpha-not-composite distinction directly: its fully transparent `(0, 1)`
reads the layer's own stored `[40, 0, 0, 0]`, and `(1, 1)` reads
`[50, 0, 0, 128]`, where a composited sample would have shown the
canvas behind them. `(3, 0)` and `(0, 3)` — one past each edge of the
3x3 canvas — error while the corner `(2, 2)` still reads
`[90, 0, 0, 255]`. A locked layer still answers (`[50, 0, 0, 255]` at
its centre) and an unknown layer errors. As with the histogram, the
second verification is the fixture listing itself; no arithmetic is
involved.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-five: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The pointer-move wiring and the per-pixel memo were
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**881 Rust tests total** (877 → 881, 874 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 129 — Camera Raw Filter > Shadow Clipping

`shadow_clipping(id)` completes the Camera Raw histogram panel's own
trio of readouts: the per-channel count of sampled pixels whose value
is clipped to `0`, over the active selection or the whole layer with
none. It is nothing more than bin `0` of each channel of the shared
`layer_histogram` helper Phase 127 factored out of `equalize` — the
same sampling, so the counts are guaranteed to agree with what the
histogram dialog is already drawing — and it inherits that helper's
conventions: read-only (a locked layer is fine), only an unknown layer
errors, every sampled pixel counts regardless of alpha. Camera Raw's
own shadow-clipping indicator is the small triangle above the left end
of its histogram, which lights in the colour of whichever channel is
clipping (white when all three do); the Histogram dialog now shows the
same information as three per-channel chips under the curves, each lit
when its count is non-zero and showing the count itself, fetched
alongside the histogram in one `Promise.all`. Camera Raw's own
click-to-toggle blue overlay painting the clipped pixels onto the
preview itself is a documented scope cut — it needs a compositing-time
overlay rather than a read-only query — as is the matching highlight
clipping indicator, which is not a separately tracked capability in
`docs/PHOTOSHOP_PARITY.md` but would be bin `255` by the identical
mechanism whenever it is wanted.

**Verified two ways.** Four new `document.rs` tests on fixtures whose
contents are written out literally. `ramped_3x3` has no R value at `0`
but G and B at `0` in all nine pixels, so it reads `[0, 9, 9]`. A
purpose-built two-pixel row — `(0, 0, 0, 255)` beside `(10, 0, 0,
255)` — reads `[1, 2, 2]`: one pixel clipped in R, both in G and B.
Selecting column `2` of `ramped_3x3` narrows the same counts to
`[0, 3, 3]`. A locked layer still answers and an unknown layer errors.
Every expected value is read directly off the fixture listing, so the
second verification is again the listing itself.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's chip wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**885 Rust tests total** (881 → 885, 878 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 130 — Camera Raw Filter > Curve > Point Curve

`camera_raw_point_curve(id, points)` fills in the Camera Raw Curve
panel's own Point Curve mode. Camera Raw's Point Curve is the same
point-driven RGB tone curve Image > Adjustments > Curves applies —
draggable points on an input/output graph, applied identically to all
three channels — so this is Phase 11's `curves` exposed under its
Camera Raw name: the same five fixed input positions (`0`, `64`,
`128`, `192`, `255`) with independently adjustable outputs, the same
straight-segment interpolation, and the same documented scope cuts (no
per-channel Red/Green/Blue curves, no spline). It is an exact preset,
the same relationship `camera_raw_saturation` has to `vibrance`, and
is framed as such. A new **Point Curve…** button with the other Camera
Raw entries opens a dialog with the same five output sliders and Reset
button the Curves dialog has, kept as its own state so the two dialogs
don't share a draft.

**Verified two ways.** Four new `document.rs` tests on `ramped_3x3`.
Points `[0, 96, 128, 192, 255]` raise the input-`64` point from `64` to
`96`, so every input below `64` scales by exactly `1.5`: `30 → 45`,
`50 → 75`, `60 → 90`; input `70` sits `6/64 = 0.09375` of the way from
`64` (now `96`) to `128` (still `128`), giving `96 + 0.09375 × 32 =
99`. A second test confirms the preset equals `curves` byte-for-byte on
a deliberately non-monotonic curve (`[0, 40, 200, 100, 255]`). A third
confirms the identity curve is a byte-for-byte no-op. A fourth confines
the lift to a one-pixel selection and confirms a locked/unknown layer
errors. All four passed on the first run, the four curve values
cross-checked by a five-line Python port of the segment interpolation
(which also confirmed the identity curve reproduces all 256 inputs).

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-seven: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**889 Rust tests total** (885 → 889, 882 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 131 — Camera Raw Filter > Color Grading

`color_grading(id, shadows, midtones, highlights)` tints a layer's
shadows, midtones, and highlights toward three independently chosen
hues, each by its own saturation — Camera Raw's three colour wheels,
reduced to a `[hue, saturation]` pair per wheel. It is built entirely
on Phase 11's `color_balance`: each wheel's pair is turned into the
three per-channel Color Balance sliders for that tonal range, and Color
Balance's own luma-weighted blending (`shadow_weight = clamp((127 -
luma) / 127, 0, 1)`, `highlight_weight = clamp((luma - 128) / 127, 0,
1)`, midtones the remainder) does the rest, so no new blending math was
written. The conversion is `tint = hsl_to_rgb(hue, 1.0, 0.5)` — the
fully saturated hue at mid lightness — and, per channel, `slider =
round((tint / 255 - 0.5) × 2 × saturation)`, so a pure hue's own channel
is pushed by `+saturation`, its two opposite channels by `-saturation`,
and a secondary hue's middle channel lands in between: hue `0` at
saturation `50` is `(+50, -50, -50)`, hue `30` (orange, tint `(255,
128, 0)`) is `(+50, 0, -50)` because `round((128/255 - 0.5) × 100) =
0`. `hue` wraps modulo `360` and `saturation` is Camera Raw's own
`0..=100`, clamped rather than erroring, matching `color_balance`'s own
convention. Camera Raw's own per-wheel Luminance sliders, its fourth
Global wheel, and its Blending and Balance controls are a documented
scope cut. A new **Color Grading…** dialog exposes a hue and a
saturation slider for each of the three ranges.

**Verified two ways.** Five new `document.rs` tests. Highlights at hue
`0`, saturation `50`, on grey `200` (luma `200`, highlight weight
`(200 - 128) / 127 = 0.566929`, shadow weight `0`): shift `±28.35`,
giving `(228, 172, 172)`, while grey `40` beside it has no highlight
weight and stays untouched. Shadows at hue `240`, saturation `50`, on
grey `40` (shadow weight `(127 - 40) / 127 = 0.685039`): shift `∓34.25`,
giving `(6, 6, 74)`, while grey `200` stays untouched. Midtones at hue
`120`, saturation `40`, on grey `128` (midtone weight exactly `1.0`):
`(88, 168, 88)`. A fourth test confirms the composition is exact: hue
`420` (wrapping to `60`, yellow), an out-of-range saturation `999`
(clamping to `100`) at hue `240`, and hue `30` at `50` produce
byte-for-byte the same `ramped_3x3` result as `color_balance` called
directly with the hand-derived triples `(+50, +50, -50)`, `(-100,
-100, +100)`, `(+50, 0, -50)`. A fifth confines the midtone tint to a
one-pixel selection and confirms a locked/unknown layer errors. All
five passed on the first run, the slider derivation and all three
pixel results cross-checked against an independent Python script that
reuses the existing `hsl_to_rgb` port and emulates `color_balance`'s
own `f32` luma and weight arithmetic via `struct.pack`/`unpack`
round-tripping.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-eight: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**894 Rust tests total** (889 → 894, 887 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 132 — Camera Raw Filter > Color Mixer

`color_mixer(id, range, hue, saturation, luminance)` brings Camera
Raw's HSL panel to this project: `hue_saturation`'s own hue shift,
saturation scale, and lightness offset — the same three formulas (`h +
shift` modulo `360`, `s × (1 + saturation/100)`, `l + luminance/100`,
each clamped) and the same `-180..=180` / `-100..=100` clamped ranges —
applied to only one of Camera Raw's eight named hue ranges at a time.
`range` is `0` Reds, `1` Oranges, `2` Yellows, `3` Greens, `4` Aquas,
`5` Blues, `6` Purples, `7` Magentas, each defined by the HSL hue of its
own named colour (`0`, `30`, `60`, `120`, `180`, `240`, `270`, `300`
degrees); a pixel belongs to whichever centre is nearest its own hue
measured around the colour wheel (so hue `350` is a Red, not a
Magenta; a hue exactly midway between two centres goes to the
lower-indexed one), and achromatic pixels — saturation `0`, which
`rgb_to_hsl` reports with a placeholder hue of `0` — belong to no range
and are never touched, so a neutral grey can't be dragged into the Reds
and brightened by a Luminance slider meant for red pixels. Camera Raw's
own eight ranges overlap with feathered edges, so that a hue between two
centres is partly affected by both sliders; this project's own hard
nearest-centre partition is a documented simplification, chosen over
guessing Camera Raw's own exact falloff widths. Any other `range`
errors. A new **Color Mixer…** dialog exposes a Range dropdown and
Hue/Saturation/Luminance sliders, matching Camera Raw's own per-range
three-slider layout one range at a time.

**Verified two ways.** Six new `document.rs` tests. The partition
itself is pinned directly: hues `0`, `14`, `15` (the Red/Orange
midpoint, going to the lower index), `331`, and `359` are Reds; `16`
and `45` Oranges; `46` Yellows; `150` Greens; `210` Aquas; `255` Blues;
`256` and `285` Purples. On a three-pixel row — `(200, 100, 100)` at
hue `0`, `(200, 150, 100)` at hue `30`, `(100, 100, 200)` at hue `240`,
all saturation `0.476190` and lightness `0.588235` — shifting the Reds
by `+120` turns only the first into its hue-`120` counterpart `(100,
200, 100)`; then shifting the Oranges by `+120` turns only the second
into `(100, 200, 150)`, leaving the Blue pixel untouched throughout.
Saturation `-50` on the red pixel halves `s` to `0.238095` and gives
`(175, 125, 125)` — the same bytes `camera_raw_saturation` produced for
that pixel in Phase 126, as it must, since the formula is shared — and
luminance `+20` lifts `l` to `0.788235`, giving `(227, 175, 175)`. A
grey `(128, 128, 128)` survives a Reds shift of `+120` hue, `+100`
saturation, `+20` luminance byte-for-byte. A one-pixel selection
confines the shift, and range `8`, a locked layer, and an unknown layer
all error. All six passed on the first run, the partition table and all
four colour results cross-checked against an independent Python script
reusing the existing `rgb_to_hsl`/`hsl_to_rgb` port.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous seventy-nine: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**900 Rust tests total** (894 → 900, 893 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 133 — Camera Raw Filter > Point Color

`point_color(id, target, range, hue, saturation, luminance)` fills in
Camera Raw's Point Color panel. Camera Raw's Point Color is the same
adjustment Image > Adjustments > Replace Color makes — pick a colour,
then shift the hue, saturation, and luminance of every pixel within
some range of it, fading out toward the edge of that range — so this
is Phase 112's `replace_color` exposed under its Camera Raw name: the
picked colour is `replace_color`'s `target`, Point Color's Range slider
is `replace_color`'s Chebyshev-distance `fuzziness` (`0..=200`, erroring
above), and the three adjustment sliders are `hue_saturation`'s own,
applied through the same linear `strength = clamp(1 - distance /
range, 0, 1)` blend. An exact preset, the same relationship
`camera_raw_point_curve` has to `curves`, and framed as such. Camera
Raw's own Point Color also lets the hue, saturation, and luminance
range widths be set independently and offers a Visualize Range
overlay; both are a documented scope cut. A new **Point Color…**
dialog exposes the colour picker, Range, and the three sliders.

**Verified two ways.** Four new `document.rs` tests, reusing Replace
Color's own already hand-verified fixtures and values. Target `(100,
100, 100)`, range `50`, luminance `-100`: an exact match goes to `(0,
0, 0)`; `(130, 100, 100)`, at Chebyshev distance `30` (strength `0.4`),
blends 40% toward black to `(78, 60, 60)`; `(200, 100, 100)`, at
distance `100`, is beyond the range and untouched. Target `(255, 0,
0)`, range `10`, hue `+120`: the exact match becomes `(0, 255, 0)`,
`(255, 20, 20)` at distance `20` is untouched, and `(255, 5, 5)` at
distance `5` (strength `0.5`) lands halfway at `(130, 130, 5)`. A third
test confirms the preset equals `replace_color` byte-for-byte on
`ramped_3x3` with all three sliders non-zero, and a fourth confirms a
range of `201`, a locked layer, and an unknown layer all error. All
four passed on the first run; the six colour values are the ones
Phase 112's independent Python port already produced, re-run for this
phase.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty: this session's Xvfb
instance was already confirmed, through a control test and a full
Xvfb-and-application restart in Phase 52, to have stopped delivering
synthetic `xdotool` pointer clicks to the webview entirely, and
re-running that diagnostic again was judged unlikely to produce new
information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**904 Rust tests total** (900 → 904, 897 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 134 — Camera Raw Filter > Curve > Parametric Curve

`parametric_curve(id, highlights, lights, darks, shadows)` completes
the Camera Raw Curve panel alongside Phase 130's Point Curve. Its four
sliders — Highlights, Lights, Darks, Shadows, each `-100..=100` in
Camera Raw's own top-to-bottom order — each lift or lower one quarter
of the tonal range without moving its neighbours. The tone curve is
piecewise linear through nine knots at inputs `0, 32, 64, …, 224, 255`;
the five knots at `0`, `64`, `128`, `192`, `255` are the four bands'
fixed boundaries, so black, white, and the three splits never move and
the curve is guaranteed monotonic whatever the sliders say, and each
slider moves its own band's centre knot by `slider / 100 × 32` —
Shadows the knot at `32`, Darks at `96`, Lights at `160`, Highlights at
`224` — so `+100` raises a band's centre all the way up to its upper
boundary and `-100` lowers it to its lower one. The curve is applied
identically to all three channels with the same straight-segment
interpolation `curves` uses (`y0 + t × (y1 - y0)`, rounded). Camera
Raw's own parametric curve blends its four regions with smooth,
overlapping falloffs and lets the three split points be dragged; this
project's own fixed quarter splits and tent-per-band linear shape are a
documented simplification, the same trade `curves` already made against
Photoshop's spline, chosen over guessing Camera Raw's own falloff
widths. Sliders clamp rather than error, like Color Balance's. A new
**Parametric Curve…** dialog exposes the four sliders and a Reset
button.

**Verified two ways.** Five new `document.rs` tests. Shadows `+50`
moves the knot at `32` to `48`, so on `ramped_3x3` every value below
`32` scales by `1.5` (`10 → 15`, `20 → 30`, `30 → 45`), values between
`32` and the fixed `64` interpolate from `48` to `64` (`40 → 48 + 8/32 ×
16 = 52`, `50 → 57`, `60 → 62`), and `70`, `80`, `90` — in the
untouched Darks band and above — reproduce exactly, all nine pixels
checked at once. Highlights `-100` moves the knot at `224` down to `192`,
flattening `192..224` onto `192` (`200 → 192`, `224 → 192`) and
stretching `224..255` from `192` back up to the fixed `255` (`240 → 192
+ 16/31 × 63 = 224.5 → 225`), while `190` below the band is untouched.
With every slider at `+100`, a row of the eight knot inputs `32, 64, 96,
128, 160, 192, 224, 255` comes back as `64, 64, 128, 128, 192, 192, 255,
255` — each centre reaching its upper boundary, each boundary
unmoved. All-zero sliders are a byte-for-byte identity, and `9999`
clamps to the same result as `100`. A one-pixel selection confines the
lift and a locked/unknown layer errors. All five passed on the first
run, every value cross-checked against an independent Python port of
the nine-knot interpolation emulating Rust's `f32` arithmetic and
rounding via `struct.pack`/`unpack` round-tripping — which also
confirmed the zero curve reproduces all 256 inputs and the all-`+100`
curve is monotonic across the whole range.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**909 Rust tests total** (904 → 909, 902 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 135 — Filter > Camera Raw Filter (the whole dialog as one edit)

With thirteen Camera Raw panels shipped one at a time over Phases
117–134, `camera_raw_filter(id, settings)` finally fills in the menu
entry itself. Camera Raw applies every panel's settings together in a
fixed internal order and commits them as a single step; this does the
same with the panels this project has built, running the
already-verified per-panel adjustments in sequence on the same layer —
white balance (`temperature_tint`), then tone (`highlights_shadows`),
then `clarity`, then `camera_raw_saturation`, then the Curve panel
(`parametric_curve` followed by `camera_raw_point_curve`), then Optics
(`defringe`) — so that one call is byte-for-byte the same as making
those calls yourself in that order, but lands as one undo step. The
settings travel as a new `CameraRawSettings` struct (serde, camelCase)
whose `Default` is the neutral dialog: every slider at `0`, an identity
point curve, no defringing. A panel left at its neutral value is
skipped outright rather than run as a no-op, so an untouched Optics
panel never round-trips every pixel through HSL and all-default
settings are an exact identity that reports nothing touched. Each
stage's own clamping or erroring rules are unchanged (a `101`
Defringe still errors, and nothing earlier in the sequence is applied
when it does, since the checkpointed edit is discarded as a whole).
Camera Raw's own remaining panels — Detail, Effects, Calibration,
Geometry, and the local-adjustment masks — and its exact internal
pipeline order are a documented scope cut. A new **Camera Raw
Filter…** dialog presents Basic, Curve (Parametric and Point), and
Optics sections with a Reset button, in a wider scrolling modal.

**Verified two ways.** Four new `document.rs` tests on a new
`camera_raw_fixture` — a 2x2 layer of four distinct, fully chromatic
colours so every stage has something to change. With every panel
non-neutral (temperature `20`, tint `-10`, highlights `30`, shadows
`-20`, clarity `40`, saturation `25`, parametric `[10, -10, 20, -20]`,
point curve `[0, 80, 128, 192, 255]`, defringe `30`), the composite
equals the seven per-panel calls made in that order byte-for-byte, and
genuinely differs from the untouched fixture. All-default settings are
a byte-for-byte identity and report `None` touched. With only
Saturation moved to `+50`, the result equals `camera_raw_saturation(50)`
alone, and pixel `(0, 0)` — `(200, 100, 100)` — is the `(225, 75, 75)`
Phase 126 already hand-verified. A one-pixel selection confines the
edit, a `101` Defringe errors, and a locked or unknown layer errors
even at all-default settings. All four passed on the first run. The
"second way" here is structural rather than numeric: every number the
composite produces is one an earlier phase already hand-computed and
cross-checked, and the tests pin that the composite reproduces exactly
those bytes.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**913 Rust tests total** (909 → 913, 906 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 136 — Edit > Transform > Rotate (any angle)

`rotate(id, degrees)` is the first of the Edit > Transform entries
beyond the fixed 90°/180° turns and flips of Phase 17: it rotates a
layer's pixels by any angle, positive clockwise on screen (Photoshop's
own sign convention), about the canvas centre `((width - 1) / 2,
(height - 1) / 2)` — chosen so that a square canvas rotated by a
multiple of `90` lands exactly back on the pixel grid. The rotation is
inverse-mapped: each output pixel looks up where it came from, `source
= centre + R(-degrees) × (pixel - centre)` (in image coordinates,
`sx = cx + cos·dx + sin·dy`, `sy = cy - sin·dx + cos·dy`), and takes
the nearest source pixel, with the same `round()` nearest-neighbour
rounding `sample_nearest` and the Distort filters already use — except
that a source position falling outside the canvas yields a fully
transparent pixel instead of clamping to the edge, since a rotated
layer genuinely has nothing there. The canvas itself does not grow, so
corners that rotate past its edges are clipped; Photoshop's own Free
Transform keeps them by letting a layer extend beyond the canvas,
which this project's document-sized layers can't, a documented scope
cut alongside the nearest-neighbour (rather than bicubic) resampling.
With a selection, only the selected pixels are rewritten, though the
rotation still reads from the whole layer. A non-finite angle errors.
A new **Rotate…** dialog takes the angle in degrees.

**Verified two ways.** Six new `document.rs` tests on `ramped_3x3`,
read back through a small `red_channel_grid` helper so a whole 3x3
result can be asserted at once. A `90°` turn makes the top row `10 20
30` the right column read top to bottom — `[[70, 40, 10], [80, 50,
20], [90, 60, 30]]` — and `-90°` the mirror `[[30, 60, 90], [20, 50,
80], [10, 40, 70]]`. `180°` equals the existing `rotate_layer_180`
command byte-for-byte. At `45°`, output `(0, 0)` has offset `(-1, -1)`, so its
source is `x = 1 + 0.7071·(-1) + 0.7071·(-1) = -0.414`, rounding to
`0`, and `y = 1 - 0.7071·(-1) + 0.7071·(-1) = 1.0`: it reads `(0, 1) =
40`; the full grid by the same arithmetic is `[[40, 10, 20], [70, 50,
30], [80, 90, 60]]`, and — a genuine property of a 3x3, not an
accident of the test — no corner rounds outside the canvas. A `3x1`
row turned `90°` about its centre `(1, 0)` shows the transparency
rule: both ends now come from `y = ±1`, outside the canvas, and read
`(0, 0, 0, 0)`, while the centre reads itself. `0°` and `360°` are
byte-for-byte identities. A one-pixel selection confines the rewrite
(the selected top-right pixel becomes `10`, everything else untouched),
and `NaN`, a locked layer, and an unknown layer all error. All six
passed on the first run, every grid cross-checked against an
independent Python port of the inverse mapping emulating Rust's `f32`
trigonometry and half-away-from-zero rounding — which was also how the
sign convention was pinned down before any Rust was written, by
checking which of the two candidate inverse matrices turned the top
row into the right column rather than the left.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**919 Rust tests total** (913 → 919, 912 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 137 — Edit > Transform > Scale

`scale(id, width_percent, height_percent)` is Rotate's sibling:
Phase 136's inverse-mapping scheme with division in place of rotation.
It resizes a layer's pixels to `width_percent` × `height_percent` of
their current size about the same canvas centre `((width - 1) / 2,
(height - 1) / 2)`; each output pixel reads the nearest source pixel at
`centre + (pixel - centre) / factor`, rounding half-away-from-zero with
`f32::round` exactly as `rotate` and `sample_nearest` do, and is fully
transparent wherever that source position falls outside the canvas —
so shrinking leaves a transparent border and enlarging pushes the edges
off a canvas that does not grow, the same documented clipping and
nearest-neighbour (rather than bicubic) scope cuts as `rotate`. The two
axes are independent. Both percentages must be finite and positive:
Photoshop's own dialog lets a negative percentage flip the layer, but
that is already Edit > Transform > Flip here, so a zero or negative
factor errors rather than silently mirroring. A new **Scale…** dialog
takes the two percentages with a Reset button.

**Verified two ways.** Six new `document.rs` tests, the first four
asserting whole grids through `red_channel_grid`. A new `ramped_4x4`
fixture (`10` to `160` in reading order) puts the canvas centre at
`(1.5, 1.5)`, between pixels, which makes doubling exact: at `200%`,
output `x = 0` reads `1.5 + (0 - 1.5) / 2 = 0.75 → 1`, `x = 1` reads
`1.25 → 1`, `x = 2` reads `1.75 → 2`, `x = 3` reads `2.25 → 2`, so the
centre `2x2` (`60, 70 / 100, 110`) fills the canvas as `2x2` blocks. At
`50%`, `x = 1` reads `0.5 → 1` and `x = 2` reads `2.5 → 3` while `x = 0`
and `x = 3` read `-1.5` and `4.5`, off the canvas: the centre `2x2`
survives as `60, 80 / 140, 160` inside a fully transparent border
(alpha checked, not just red). Width `50%` at height `100%` on
`ramped_3x3` keeps only the middle column, each row its own value. And
`ramped_3x3` at `200%` — an odd canvas whose centre is on a pixel —
gives `[[50, 50, 60], [50, 50, 60], [80, 80, 90]]`, leaning toward the
bottom-right rather than symmetric, because `x = 0` reads `1 + (0 - 1)
/ 2 = 0.5`, which `f32::round` sends away from zero to `1`, while `x =
2` reads `1.5 → 2`; the test pins that tie-breaking behaviour rather
than hiding it. `100%` is a byte-for-byte identity. A one-pixel
selection confines the rewrite, and `0%`, a negative percentage, `NaN`,
a locked layer, and an unknown layer all error. All six passed on the
first run, every grid cross-checked against an independent Python port
of the inverse mapping emulating Rust's `f32` division and
half-away-from-zero rounding.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**925 Rust tests total** (919 → 925, 918 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 138 — Edit > Transform > Skew

`skew(id, horizontal_degrees, vertical_degrees)` is the third of the
Transform family's inverse-mapped resamplers, and the two-hundredth
capability shipped. It shears a layer by `horizontal_degrees` — each
row slides sideways in proportion to its distance from the centre row,
by `tan(angle)` pixels per pixel — and then by `vertical_degrees`, each
column sliding up or down likewise, about the same canvas centre
`rotate` and `scale` use, with their own nearest-neighbour rounding and
transparent fill wherever a source position falls off the canvas. The
two shears are applied one after the other, horizontal first, rather
than as Photoshop's single simultaneous affine: each has determinant
`1`, so no combination is degenerate, whereas Photoshop's own combined
skew folds the layer flat whenever `tan(h) × tan(v) = 1` (both at
`45°`, say) — the first draft of this phase's Python model used the
simultaneous form and divided by zero at exactly that pair, which is
what prompted the sequential definition. This is a documented
difference in how the two angles compose, not an approximation of an
intermediate result. Each angle must be finite and strictly inside
`-90..90`, Photoshop's own `-89..=89` skew range rounded out to where
`tan` stops being finite. A new **Skew…** dialog exposes the two
angle sliders and a Reset button.

**Verified two ways.** Six new `document.rs` tests, four asserting
whole grids. On `ramped_3x3`, horizontal `45°` (`tan = 1`) makes the
top row (`dy = -1`) read one pixel to its right, the middle row read
itself, and the bottom row read one to its left, vacated ends
transparent: `[[20, 30, 0], [40, 50, 60], [0, 70, 80]]`; `-45°` is the
mirror `[[0, 10, 20], [40, 50, 60], [80, 90, 0]]`; vertical `45°` does
the same to columns, `[[40, 20, 0], [70, 50, 30], [0, 80, 60]]`. Both
at `45°` — the pair that would be singular in Photoshop's form — reads
source `y = row - (col - 1)` then source `x = col - (y - 1)` and gives
`[[40, 30, 0], [0, 50, 0], [0, 70, 60]]`. On the even `ramped_4x4`,
horizontal `45°` puts the top row `dy = -1.5` from the centre so it
reads `x + 1.5`, rounding away from zero to `x + 2`: `[[30, 40, 0, 0],
[60, 70, 80, 0], [0, 100, 110, 120], [0, 0, 140, 150]]`. Zero skew is a
byte-for-byte identity. A one-pixel selection confines the rewrite, and
`90°`, `-90°`, `NaN`, a locked layer, and an unknown layer all error.
All six passed on the first run, every grid cross-checked against an
independent Python port of the sequential inverse mapping emulating
Rust's `f32` `tan` and half-away-from-zero rounding.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**931 Rust tests total** (925 → 931, 924 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 139 — Edit > Free Transform

`free_transform(id, transform)` gathers the three Transform entries of
Phases 136–138 plus a move into Photoshop's own Free Transform: one
dialog, one undo step. The settings travel as a new `FreeTransform`
struct (serde, camelCase) — `width_percent`, `height_percent`,
`degrees`, `skew_horizontal`, `skew_vertical`, `offset_x`, `offset_y` —
whose `Default` is the neutral transform (`100`, `100`, `0`, `0`, `0`,
`0`, `0`). The stages run in a fixed, documented order on the same
layer: `scale`, then `rotate`, then `skew`, then a move, each an
already-verified command, and a stage left at its default is skipped
outright, so the result is byte-for-byte what the per-stage calls in
that order would produce and all-default settings are an exact
identity reporting nothing touched. The move is a new private
`translate` helper in the same inverse-mapped style as its three
siblings — every output pixel reads `(x - offset_x, y - offset_y)` and
is transparent where that falls off the canvas — kept separate from
Filter > Other > Offset, whose whole point is wrapping the vacated
edge back in. Photoshop's Free Transform computes one combined affine
and resamples once, positions a movable reference point, and is driven
by on-canvas handles; this project's sequential composition resamples
nearest-neighbour once per stage (so a scale followed by a rotate
rounds twice), always about the canvas centre, from typed values — a
documented scope cut, the same kind Phase 135's Camera Raw composite
made. Each stage's own erroring rules are unchanged, and because the
edit is checkpointed as a whole, a stage that errors leaves nothing
earlier applied.

**Verified two ways.** Six new `document.rs` tests. The move alone
is pinned by hand: offset `(1, 0)` on `ramped_3x3` gives `[[0, 10,
20], [0, 40, 50], [0, 70, 80]]` and `(0, -1)` gives `[[40, 50, 60],
[70, 80, 90], [0, 0, 0]]`, with the vacated pixels fully transparent.
A transform with every stage non-neutral — `50%` × `50%`, `90°`, skew
`45°`/`0°`, offset `(1, 0)` on `ramped_4x4` — equals the four calls
made in that order byte-for-byte and genuinely differs from the
untouched fixture. All-default settings are a byte-for-byte identity
reporting `None` touched. With only `degrees = 90`, the result equals
`rotate(90)` alone (the Phase 136 grid). A one-pixel selection confines
the edit, and a `0%` width, a `90°` skew, a locked layer, and an
unknown layer (even at all-default settings) all error. All six passed
on the first run; as with the Camera Raw composite, the second
verification is structural — every non-trivial byte the composite
produces is one Phases 136–138 already hand-computed and cross-checked
in Python, and the tests pin that the composite reproduces exactly
those bytes.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**937 Rust tests total** (931 → 937, 930 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 140 — Edit > Transform > Again

`transform_again(id)` repeats the most recent transform — whichever of
`rotate`, `scale`, `skew`, or `free_transform` last ran with
non-neutral values — on layer `id`, as a `free_transform` with those
same values. Photoshop's own Again is what makes stepped copies cheap
(rotate a petal, duplicate the layer, Again, Again…), and the target
layer is deliberately independent of the layer the transform was first
applied to, so exactly that workflow works here. The `Document` gains a
`last_transform: Option<FreeTransform>` field: `rotate`, `scale`, and
`skew` each record their own values as the equivalent `FreeTransform`
(`degrees` alone, the two percentages alone, the two skew angles alone),
and `free_transform` records the whole struct — but only when it is
non-neutral, so an all-default Free Transform doesn't quietly replace
the transform you meant to repeat. Because the remembered transform
travels with the document snapshot through undo and redo, undoing a
transform also forgets it, exactly as Photoshop does. `DocumentView`
gains a matching `can_transform_again` flag, and a **Transform Again**
button beside Free Transform is enabled only when there is something to
repeat; the command errors with "Nothing to transform again." otherwise.

**Verified two ways.** Five new `document.rs` tests. `rotate(90)` then
Again equals `rotate_layer_180` byte-for-byte on `ramped_3x3` — two
quarter turns are a half turn, and `can_transform_again` flips from
`false` to `true` across the first rotate. A Free Transform move by
`(1, 0)` then Again slides the layer two pixels in total: `[[0, 0, 10],
[0, 0, 40], [0, 0, 70]]`. A `50%` scale followed by an all-default
Free Transform and then Again equals two `50%` scales (the neutral
transform did not replace the remembered one), and a `45°` skew then
Again equals two skews. Rotating one layer and calling Again on a second
layer leaves the two byte-for-byte identical. Finally, Again with
nothing recorded errors with the expected message, and a locked or
unknown layer errors even with a transform recorded. All five passed on
the first run; the second verification is again structural — every
result is an already-verified command applied twice, and the tests pin
that Again reproduces exactly that.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-seven: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**942 Rust tests total** (937 → 942, 935 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 141 — Edit > Transform > Distort

`distort(id, corners)` is the Transform family's first genuinely
projective member. It maps the layer's four corners — top-left,
top-right, bottom-right, bottom-left, given as `[x, y]` pixel positions
— onto `corners`, warping everything between them with the projective
transform (a homography) those four correspondences define, so
straight lines stay straight and a trapezoid target foreshortens the
way a plane seen at an angle does, with rows compressing toward the
far edge rather than merely narrowing. It is inverse-mapped like
`rotate`: the homography is solved from the destination corners back
to the source corners (`(0, 0)`, `(width - 1, 0)`, `(width - 1, height -
1)`, `(0, height - 1)`) as the standard eight linear equations, two per
correspondence, by a new `solve_8x8` Gaussian elimination with partial
pivoting in `f64`; then every output pixel evaluates `(a·x + b·y + c,
d·x + e·y + f) / (g·x + h·y + 1)`, rounds half-away-from-zero, and
reads that source pixel — transparent wherever it falls off the canvas
or the denominator vanishes. Corners that are collinear or coincident
admit no homography (a zero pivot) and error, as does a non-finite
coordinate. Photoshop's own Distort is dragged by handles and resamples
bicubically; typed corners and nearest-neighbour are the same
documented scope cuts the rest of the family makes, and Distort is not
recorded for Transform Again, which repeats `FreeTransform`s only. A
new **Distort…** dialog opens with the four corners at the canvas
corners and takes a new position for each.

**Verified two ways.** Six new `document.rs` tests. The canvas's own
corners are a byte-for-byte identity; sending the corners one step
clockwise reproduces `rotate(90)` exactly, and shifting every corner
right by one reproduces the Phase 139 move exactly — three affine
special cases the general solver must recover, and does. Pulling the
top corners of `ramped_3x3` in to `x = 0.5` and `1.5` leaves only the
middle output pixel of the top row inside the trapezoid, reading the
source's own top-middle `20`: `[[0, 20, 0], [40, 50, 60], [70, 80,
90]]`. On `ramped_4x4` with the top edge inset one pixel a side, the
solved destination-to-source map is `x → 3x + y - 3`, `y → 3y / (0.6667y
+ 1)`, so output `(1, 0)` reads `(0, 0) = 10`, `(2, 0)` reads `(3, 0) =
40`, `(1, 1)` reads `(1, 1.8 → 2) = 100`, and the whole bottom half
reads the source's bottom row — `[[0, 10, 40, 0], [0, 100, 110, 0],
[130, 140, 150, 160], [130, 140, 150, 160]]`, the perspective
compression a row-by-row scaling could never produce. A keystone with
a tall left edge gives `[[10, 10, 0, 0], [50, 50, 60, 40], [90, 90, 100,
160], [130, 130, 0, 0]]`. A one-pixel selection confines the rewrite;
collinear corners, four coincident corners, a `NaN` coordinate, a locked
layer, and an unknown layer all error. All six passed on the first run,
every grid cross-checked against an independent Python implementation
of the same eight-equation solve — written first, with the identical
pivoting order, so its `f64` arithmetic is bit-for-bit the Rust
solver's — and the same rounding rule.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-eight: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**948 Rust tests total** (942 → 948, 941 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 142 — Edit > Transform > Perspective

`perspective(id, horizontal, vertical)` is Phase 141's `distort` with
its corners moved in mirrored pairs, the way Photoshop's own
Perspective drags one corner and slides its neighbour on the same edge
the opposite way. `horizontal` is a pixel inset applied to both ends of
one horizontal edge — positive narrows the top edge, negative the
bottom — and `vertical` likewise narrows the left edge when positive
and the right edge when negative, so a positive `horizontal` is the
classic "building leaning away" keystone; the other two corners stay
put. Everything else is `distort`'s: the projective warp, the
nearest-neighbour resampling and transparent fill, and the error for a
non-finite value. An inset that collapses an edge to a point (half the
canvas width or more) leaves two corners coincident, and the
homography solver rejects it the same way it rejects any degenerate
quad. A new **Perspective…** dialog takes the two insets with a Reset
button.

**Verified two ways.** Six new `document.rs` tests, every grid one
Phase 141's Python solver already produced for the corresponding
corner quad. A `0.5` horizontal inset on `ramped_3x3` is exactly
Distort's own trapezoid — `[[0, 20, 0], [40, 50, 60], [70, 80, 90]]` —
and equals `distort` called with those corners byte-for-byte; a `1`
inset on `ramped_4x4` is Distort's top-inset grid. The mirror inset
`-1` narrows the bottom instead: `[[10, 20, 30, 40], [10, 20, 30, 40],
[0, 60, 70, 0], [0, 130, 160, 0]]`, the top half now reading the
source's top row. A vertical inset of `1` narrows the left edge —
`[[0, 0, 40, 40], [10, 70, 80, 80], [130, 110, 120, 120], [0, 0, 160,
160]]` — and `-1` reproduces Distort's tall-left keystone exactly.
Zero insets are a byte-for-byte identity. A one-pixel selection
confines the rewrite (the selected corner pixel, outside the
trapezoid, becomes transparent while everything else is untouched). A
full-pixel inset on the 3-wide canvas puts both top corners at `x = 1`
and errors, as do `NaN`, a locked layer, and an unknown layer. All six
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous eighty-nine: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**954 Rust tests total** (948 → 954, 947 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 143 — Edit > Define Pattern

`define_pattern(id)` is the first half of pattern support: it captures
layer `id`'s own pixels inside the active selection — or the whole
layer with none — as the document's pattern, a new `Pattern { width,
height, pixels }` value kept on the `Document`, for pattern fills to
tile. Photoshop's own Define Pattern insists on a plain rectangular
marquee (no feather; the command is greyed out for anything else), and
this does the same, erroring on an elliptical, rounded, inverted, or
bordered selection, since a tile is a rectangle by definition. It reads
the one layer's own stored bytes rather than the flattened composite
(Photoshop samples the active layer too), so a locked layer is fine and
only an unknown layer errors, and nothing on the canvas changes.
Photoshop keeps patterns as application-wide presets that outlive any
document; here the one defined pattern lives on the document and
travels through undo and redo with everything else — a documented scope
cut, the same shape as Transform Again's remembered transform.
`DocumentView` (and its TypeScript mirror) gains a `has_pattern` flag,
and a new **Define Pattern** button sits with the fill-layer buttons;
the pattern's consumers arrive in the next phase.

**Verified two ways.** Five new `document.rs` tests on `ramped_3x3`,
whose bytes are the expected values by construction. With no
selection, the pattern is the whole `3x3` layer byte-for-byte, the
canvas is untouched, and `has_pattern` flips from `false` to `true`.
Selecting columns `1..3` of rows `0..2` captures a `2x2` tile reading
`20 30 / 50 60` (each `(R, 0, 0, 255)`), pinned as the exact
sixteen-byte vector. An elliptical selection and an inverted rectangle
both error and leave no pattern behind. Defining twice replaces the
first pattern with the second (a `1x1` of `(10, 0, 0, 255)`). A locked
layer can be sampled and an unknown layer errors. All five passed on
the first run; the second verification is the fixture listing itself.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**959 Rust tests total** (954 → 959, 952 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 144 — Layer > New Fill Layer > Pattern

`add_pattern_layer(name)` completes the fill-layer trio and gives
Phase 143's pattern its first consumer: a new top layer, document
sized, tiled from the top-left corner with the pattern `define_pattern`
captured, every tile the pattern's own bytes (pixel `(x, y)` reads
pattern `(x mod width, y mod height)`), alpha included. It errors when
no pattern has been defined, which is also when the new **Pattern
Fill** button beside Solid Color and Gradient Fill is disabled. Like
the other two fill layers it is an ordinary, fully editable pixel layer
rather than a live, re-openable fill (this app's layer model has no
generative layer kind — the same documented scope cut Solid Color
made in Phase 14), and Photoshop's own dialog's Scale, Angle, and
Link-with-Layer options are a documented scope cut alongside it.

**Verified two ways.** Four new `document.rs` tests. Capturing the
`2x2` tile `20 30 / 50 60` from `ramped_3x3` (columns `1..3` of rows
`0..2`) and adding a pattern layer yields a new top layer reading
`[[20, 30, 20], [50, 60, 50], [20, 30, 20]]` — the tile repeated, with
the third column and row wrapping back to the tile's first — every
pixel fully opaque, while the original layer beneath is untouched.
Capturing the whole `3x3` layer and tiling it into a new `3x3` layer
reproduces the layer byte-for-byte. The new layer is the top of the
stack, unlocked, and named as given. With no pattern defined the call
errors and adds nothing. All four passed on the first run; the tiled
grid is read directly off the fixture, so the second verification is
the listing itself.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-one: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**963 Rust tests total** (959 → 963, 956 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 145 — Pattern Stamp tool

The Pattern Stamp is the first new *tool* (as opposed to a menu
command) in many phases, and it costs very little because the brush
already does almost all the work. `Stroke` gains a third variant,
`PatternStamp { opacity }`: `Document::stroke` builds the same
soft-edged, selection-clipped coverage mask it builds for the Brush and
Eraser, and then, instead of a single flat colour, each covered pixel
takes the pattern pixel at `(x mod width, y mod height)` — tiles
aligned to the canvas origin, Photoshop's default "Aligned" mode, so
lifting the brush and stamping again continues the same tiling — with
the pattern pixel's own alpha scaled by the tool's opacity (`0..=255`)
and by the coverage, blended `source-over` exactly as the Brush is.
The stamp reads the pattern Phase 143's `define_pattern` captured and
errors before touching anything when none is defined. The match inside
the stroke loop was reshaped so the Brush and the stamp share one
blend: each arm now yields a `(colour, source_alpha)` pair and the
Eraser arm `continue`s, so the source-over arithmetic exists once. A
new **Pattern Stamp** tool button sits beside Brush and Eraser, enabled
only once a pattern exists; the colour swatch is disabled for it, the
opacity slider still applies, and pointer drags send
`pattern_stamp_stroke` exactly as the Brush sends `paint_stroke`.
Photoshop's own unaligned mode and Impressionist option are a
documented scope cut.

**Verified two ways.** Four new `document.rs` tests. A stamp centred
on pixel `(1, 1)` with radius `3` covers every centre of a `3x3` layer
fully — the farthest centre is `√2` away, so its coverage `3 - 1.414 +
0.5` clamps to `1` — and painting onto a fully transparent layer with
the Phase 144 `2x2` tile reproduces `[[20, 30, 20], [50, 60, 50], [20,
30, 20]]` at alpha `255` exactly, since source-over onto transparency
is the source. At opacity `128` the colour is kept and the alpha lands
at `to_byte(128/255) = 128` (`(50, 0, 0, 128)` at the centre, `(90, 0,
0, 128)` at the corner). With no pattern the stamp errors and paints
nothing; with a one-pixel selection at the top-left only that pixel is
painted (`(10, 0, 0, 255)`) and the rest stays transparent. A locked
layer errors and is untouched. The Brush and Eraser's own pre-existing
stroke tests guard the shared-blend refactor and all still pass. All
four passed on the first run (clippy then asked for three `&vec![…]`
fixture buffers to be plain array slices); the coverage arithmetic is
the Brush's, verified when the Brush first shipped, and the pattern
bytes are the fixture's.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-two: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**967 Rust tests total** (963 → 967, 960 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 146 — Edit > Paste Special > Paste Into

`paste_into(clipboard, name)` adds the one Paste Special variant this
app was missing. Where `paste` (and Paste in Place) drops the clipboard
back at its original coordinates, Paste Into centres it in the active
selection — the new layer's origin is the selection's bounding box's
top-left plus half the difference between the two sizes, truncated
toward zero — and keeps only the pixels that fall inside the
selection's own shape, so an elliptical or bordered selection clips the
pasted content to that shape, not just to its bounding box. Anything
off the canvas or outside the selection is left fully transparent.
Photoshop implements the clipping as a live layer mask that can be
moved and edited afterward; this project's layer model has no masks,
so the mask is baked in as transparency — a documented scope cut,
alongside the fact that the clipboard here is rectangular pixel data
rather than a floating selection. It errors when nothing is selected
(Photoshop greys the command out) and, at the command layer, when
nothing has been copied yet. A new **Paste Into** button beside Paste
is enabled only with both a clipboard and a selection.

**Verified two ways.** Five new `document.rs` tests. Copying the
top-left `2x2` of `ramped_3x3` (`10 20 / 40 50`) and pasting into a
selection of the bottom-right `2x2` places it exactly there — `[[0, 0,
0], [0, 10, 20], [0, 40, 50]]`, every kept pixel opaque, the vacated
ones transparent — and the source layer is untouched. Pasting the whole
`3x3` into that same `2x2` selection centres it (a size difference of
`-1`, truncating to `0`, so the origin is the selection's corner) and
clips it to the same four pixels, `10 20 / 40 50`. Pasting the whole
`ramped_4x4` into an elliptical selection spanning the canvas keeps
every pixel except the four corners, whose centres lie `√4.5 ≈ 2.12`
from the ellipse's centre against a radius of `2` — exactly the
selection's own `contains` rule, so what the paste keeps is what a
brush would be allowed to paint. Pasting the whole `3x3` into a single
selected pixel at `(2, 2)` pins the centring arithmetic's truncation
toward zero: the origin is `2 + (1 - 3) / 2 = 1`, so only clipboard
`(1, 1) = 50` lands inside. With no selection the call errors and adds
no layer. All five passed on the first run; the
kept pixels are read straight off the fixtures and the ellipse
membership is the `Selection` type's own, already-tested rule.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**972 Rust tests total** (967 → 972, 965 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 147 — Camera Raw Filter > Geometry (Manual)

`camera_raw_geometry(id, settings)` fills in the Camera Raw Geometry
panel's Manual mode, and it is almost entirely composition: with
Phases 136–142 having built rotation, scaling, a transparent-fill move,
and mirrored-inset perspective, the panel's sliders map onto them
directly. The settings travel as a new `GeometrySettings` struct
(serde, camelCase) — `vertical` and `horizontal` (the panel's two
perspective sliders, here as pixel insets exactly as `perspective`
takes them), `rotate` (degrees, clockwise), `aspect` (`-99..=99`),
`scale` (percent), `offset_x` and `offset_y` (pixels) — with a
neutral `Default`. The stages run in a fixed, documented order on the
same layer: `perspective(horizontal, vertical)`, then `rotate`, then
`scale` carrying the aspect — width `scale × (100 + aspect) / 100`
percent, height `scale × (100 - aspect) / 100` percent, so a positive
aspect widens and a negative one narrows, this project's own explicit
definition of a slider Camera Raw documents only by feel — then the
offset. Stages left at their defaults are skipped, so all-default
settings are an exact identity; every stage keeps its own erroring
rules, so an aspect of `±100` (a zero-percent axis) errors through
`scale` exactly as a degenerate perspective errors through `distort`.
Upright's automatic modes (Auto, Level, Vertical, Full, Guided) need
line detection this project has no basis for, and the lens Distortion
slider needs a lens model; both are a documented scope cut, as is the
panel's on-preview grid. A new **Geometry…** dialog beside the Camera
Raw Filter button exposes the seven values with a Reset button.

**Verified two ways.** Five new `document.rs` tests. With every stage
non-neutral on `ramped_4x4` — horizontal inset `1`, rotate `90`, scale
`50` at aspect `0`, offset `(1, 0)` — the composite equals the four
calls made in that order byte-for-byte and differs from the untouched
fixture. All-default settings are a byte-for-byte identity reporting
`None` touched. With only `rotate = 90` the result is Phase 136's grid
`[[70, 40, 10], [80, 50, 20], [90, 60, 30]]`. Aspect alone is pinned
against `scale` directly: aspect `-50` at scale `100` on `ramped_3x3`
equals `scale(50, 150)`, which keeps only the middle column (`[[0, 20,
0], [0, 50, 0], [0, 80, 0]]` — the height factor of `150%` reads
`1 + (y - 1) / 1.5`, rounding every row back to itself). An aspect of
`100`, a locked layer, and an unknown layer all error. All five passed
on the first run; as with the other composites, every byte is one an
earlier phase already hand-computed and cross-checked.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-four: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**977 Rust tests total** (972 → 977, 970 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 148 — Camera Raw Filter > Geometry > Constrain Crop

`constrain_crop(id)` crops the whole document to the largest
axis-aligned rectangle of fully opaque pixels on layer `id` — which is
what a rotate, skew, perspective, or Geometry correction leaves behind
once its transparent corners are cut away. It brings with it the first
document-level crop this app has had: a new `crop(rect)` that resizes
the canvas and every layer to `rect` (which must cover at least one
pixel inside the canvas), and, like `rotate_document_90`, the only
other operation that changes the canvas's dimensions, clears the active
selection and whatever `reselect` would have restored, since their
bounds no longer mean anything. The rectangle itself is found with the
classic row-histogram stack scan — for each row, every column's run of
opaque pixels ending there, then the largest rectangle under that
histogram — so it is exact rather than a heuristic; among equal areas
the first found wins, which, scanning rows top to bottom, is the
widest, topmost candidate. The command returns the rectangle it cropped
to, errors when the layer has no fully opaque pixel at all, and is a
byte-for-byte no-op on a fully opaque layer. Camera Raw's own Constrain
Crop is a checkbox that re-applies as the sliders move; here it is a
command run once after the geometry is settled — a documented scope cut
— exposed as a **Constrain Crop** button beside Geometry.

**Verified two ways.** Six new `document.rs` tests. `crop` itself:
cropping `ramped_3x3` plus a second solid layer to columns `1..3` of
rows `0..2` yields a `2x2` document whose first layer is exactly `20 30
/ 50 60` and whose second is the solid at the new size, with the
selection cleared; an empty rectangle and one reaching past the canvas
both error and leave the canvas alone. Then the scan: `ramped_4x4` with
three pixels made transparent so the alpha mask reads `0111 / 1111 /
1111 / 0011` has a unique largest opaque rectangle — columns `1..4` of
rows `0..3`, nine pixels, beating both eight-pixel candidates — and
cropping to it leaves a fully opaque `3x3` reading `[[20, 30, 40], [60,
70, 80], [100, 110, 120]]`. `ramped_4x4` turned `45°` (Phase 136's
mapping gives `[[0, 50, 20, 0], [90, 100, 70, 30], [140, 110, 110, 80],
[0, 150, 120, 0]]`) has transparent corners and two tied eight-pixel
candidates; the scan's first find, the wide one, wins, and the document
becomes the `4x2` middle band `[[90, 100, 70, 30], [140, 110, 110, 80]]`.
A fully opaque layer reports the whole canvas and is untouched; a fully
transparent layer errors mentioning "opaque" without resizing, and an
unknown layer errors. All six passed on the first run, the scan
cross-checked in Python against a brute-force search over every
rectangle on the two fixtures and on three hundred random masks up to
`6x6` (identical areas throughout, and the same tie-break on the
rotated fixture).

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-five: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand/script-verified
Rust tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**983 Rust tests total** (977 → 983, 976 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 149 — Magic Wand tool (and pixel-mask selections)

The Magic Wand is the first selection tool that no geometric shape
can describe, so this phase does two things: it teaches `Selection` to
carry a pixel mask, and it ships the wand as that mask's first
producer. `SelectionShape` gains a `Mask` variant and `Selection` an
optional `mask: Arc<SelectionMask>` — a document-sized bitmap shared
through an `Arc` so that cloning a selection, which every stroke and
filter does once per call, stays a pointer copy rather than a
canvas-sized memcpy. `Selection::contains` consults the bitmap for a
`Mask` shape and is otherwise unchanged, so every selection-respecting
command in the app — fills, strokes, filters, adjustments, copy, cut,
Paste Into, Define Pattern's rectangle check — honours a mask
selection without a line of change, and Select > Inverse still just
flips a flag. The cost was that `Selection` can no longer be `Copy`:
some thirty-five `let selection = self.selection;` captures across the
file became `.clone()`s and a handful of by-value `Option` accessors
became `as_ref()`, all mechanical and all guarded by the existing
tests. The bitmap is never serialised to the frontend (`serde(skip)`);
the view carries only the `"mask"` shape and its bounding box, which
the canvas now draws as a dotted outline. Select > Modify's Expand,
Contract, Smooth, and Border reshape a bounding box, which a mask has
no meaningful way to follow, so they decline a mask selection with a
clear error for now — a documented gap the morphological versions can
close later.

`select_magic_wand(id, x, y, tolerance, contiguous)` then replaces the
selection with every pixel of layer `id` whose colour is within
`tolerance` of the clicked pixel's own — per channel, RGBA, the same
`abs_diff <= tolerance` test the Paint Bucket already uses — reached
4-connected from the click when `contiguous` is set (Photoshop's
default) or anywhere on the layer when it is not, with `bounds` the
mask's bounding box. It reads the one layer's own bytes (Photoshop's
Sample All Layers is a documented scope cut, as is its Anti-alias
option; masks here are hard-edged like every other selection), works
on a locked layer, and errors on an unknown layer or a click off the
canvas. A new **Magic Wand** tool button sits with the marquees, with
a Tolerance slider and a Contiguous checkbox appearing in the tool
options while it is active; a click on the canvas sends the wand.

**Verified two ways.** Seven new `document.rs` tests, reading the
selection back pixel by pixel through `Selection::contains`. On
`ramped_3x3` from `(0, 0) = 10` at tolerance `15`, `(1, 0) = 20` is
within `15` of the seed and adjacent, while `(2, 0) = 30` is `20` away
and `(0, 1) = 40` is `30` away, so exactly the top-left two pixels are
selected, with bounds `(0, 0)–(2, 1)` and shape `Mask`; at tolerance
`25` the wand reaches `(2, 0)` through `(1, 0)` but still not `40`. A
second test pins that the comparison is against the seed rather than
the neighbour: every step along the ramp is `10` apart, yet tolerance
`15` from the seed selects two pixels, not the whole ramp. A `3x1` row
`10 50 10` clicked at tolerance `0` selects only the first pixel
contiguously and both `10`s non-contiguously, with bounds spanning the
row. A mask selection confines `fill_selection` to exactly its pixels
and inverts to exactly the complement, and the view exposes the `Mask`
shape; a mask survives deselect and reselect; all four Modify commands
decline it with an error mentioning "pixel-mask" and leave it intact;
a locked layer can be sampled, and an off-canvas click or unknown layer
errors. All seven passed on the first run, alongside the whole existing
suite unchanged through the `Copy`-to-`Clone` refactor; the expected
pixels are read straight off the fixture.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-six: this session's
Xvfb instance was already confirmed, through a control test and a
full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**990 Rust tests total** (983 → 990, 983 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 150 — Select > Color Range

`select_color_range(id, color, fuzziness)` is the second producer of
Phase 149's pixel-mask selections and shows what that infrastructure
bought: it is thirty lines. It replaces the selection with every pixel
of layer `id` whose red, green, and blue are each within `fuzziness` of
`color`, wherever on the layer it sits — a Chebyshev distance on RGB,
alpha ignored, since Photoshop's Color Range judges colour and not
coverage — and errors, leaving the current selection untouched, when
no pixel qualifies (Photoshop's own "No pixels were selected" warning).
The bitmap-to-selection tail the Magic Wand had inline (compute the
bounding box, wrap the bitmap in an `Arc`, install it as a `Mask`
selection) is now a shared `set_mask_selection` helper both commands
use. Photoshop's Color Range is richer in ways that are documented
scope cuts here: its selection is soft (partial selection falling off
with distance, whereas every selection in this project is hard-edged),
its colour can be sampled and refined with add/subtract eyedroppers,
Localized Color Clusters weights by distance from the samples, and the
Skin Tones, Highlights, Midtones, Shadows, and Out of Gamut presets
select by criteria other than one colour. A new **Color Range…**
dialog beside the Magic Wand takes a colour picker and a Fuzziness
slider.

**Verified two ways.** Five new `document.rs` tests on `ramped_3x3`,
whose R ramp makes the expected sets obvious. Colour `(50, 0, 0)` at
fuzziness `10` selects exactly the middle row — `40`, `50`, `60` are
within `10`; `30` and `70` are `20` away — with bounds `(0, 1)–(3, 2)`
and shape `Mask`; at fuzziness `0` only the centre pixel. `(200, 200,
200)` at fuzziness `5` matches nothing: the call errors with a message
mentioning "No pixels" and a previously made rectangle selection is
still intact. Alpha is ignored: on `depth_ramped_3x3`, whose left
column is fully transparent, colour `(40, 0, 0)` at fuzziness `0`
still selects `(0, 1)`. The Magic Wand's own Phase 149 tests, now
running through the shared helper, are unchanged; an unknown layer
errors. All five passed on the first run; every expected pixel is read
off the fixture.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-seven: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**995 Rust tests total** (990 → 995, 988 lib + 7 pipeline). `cargo fmt`,
`clippy`, and `npm run build` all clean.

## Phase 151 — Select > Grow

`grow_selection(id, tolerance)` extends the current selection to every
pixel of layer `id` it can reach, 4-connected, through pixels whose
colour lies within the selection's own colour range widened by
`tolerance`: per channel, RGBA, `min − tolerance ..= max + tolerance`
over the pixels already selected, saturating at `0` and `255`. It is
the Magic Wand's contiguous fill seeded by every selected pixel at once
and judged against a range rather than one clicked colour, which is
also how Photoshop frames it — Grow takes its Tolerance from the Magic
Wand's options, and the new **Grow** button beside Color Range does the
same, reading the Wand's Tolerance slider. Two definitional choices are
this project's own and are stated here: the range is computed once,
from the selection as it stood, so the fill never widens its own
criterion as it spreads (a pixel 20 away from everything originally
selected can still join, but only if some chain of in-range neighbours
leads to it); and repeating the command recomputes the range from the
now-larger selection, so successive Grows keep spreading, as
Photoshop's do. The result is always a pixel-mask selection, even when
nothing qualified, so a rectangle that gains nothing becomes the
equivalent mask. Nothing selected, or an unknown layer, errors and
leaves the selection intact. Two helpers arrive with it that Similar
(next) will share: `selected_bits`, the current selection rasterised
one flag per pixel centre, and `colour_range_of`, the per-channel range
of the flagged pixels widened by a tolerance.

**Verified two ways.** Five new `document.rs` tests on `ramped_3x3`
(the R channel steps `10 … 90` across the grid), every expected pixel
read off the fixture and the whole set reproduced by an independent
Python model of the algorithm (`grow_check.py`) before the Rust tests
ran. The centre pixel (`50`) selected as a rectangle and grown by `10`
admits its `40` and `60` neighbours but neither `20` above nor `80`
below, giving exactly the middle row with bounds `(0, 1)–(3, 2)` and
shape `Mask`. Selecting `40` and `50` together makes the range
`40..=50`: at tolerance `5` (`35..=55`) nothing joins and the selection
merely becomes the equivalent mask; at tolerance `10` (`30..=60`) the
`60` joins and then `30` — twenty away from `50`, but in range —
joins through it, yielding `[[F, F, T], [T, T, T], [F, F, F]]`. On the
`3×1` row `10 50 10` with the first pixel selected, tolerance `0` leaves
the far `10` unselected: Grow reaches only adjacent pixels. Growing the
centre twice at `10` widens the range to `30..=70` on the second pass
and reaches `30` and `70` but still not `20` or `80`, giving `[[F, F,
T], [T, T, T], [T, F, F]]`. With nothing selected the call errors with
"Nothing is selected"; with a rectangle selected an unknown layer errors
and the rectangle survives. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-eight: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1000 Rust tests total** (995 → 1000, 993 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 152 — Select > Similar

`select_similar(id, tolerance)` extends the current selection to every
pixel of layer `id`, wherever it sits, whose colour lies within the
selection's own colour range widened by `tolerance` — the same
per-channel RGBA range Phase 151's Grow uses (`min − tolerance ..= max
+ tolerance` over the pixels already selected, via the shared
`selected_bits` and `colour_range_of` helpers), without Grow's
adjacency requirement. Similar is to Grow exactly what the Magic
Wand's non-contiguous mode is to its contiguous one, and it takes its
tolerance from the Wand's options the same way; a new **Similar**
button beside Grow reads the Wand's Tolerance slider. Every pixel
already selected defines the range and so stays selected. As with
Grow, the range is fixed from the selection as it stood, repeating the
command recomputes it from the larger selection, and the result is
always a pixel-mask selection. Nothing selected, or an unknown layer,
errors and leaves the selection intact.

**Verified two ways.** Five new `document.rs` tests, every expected
pixel read off its fixture and the full set reproduced by an
independent Python model (`similar_check.py`) before the Rust tests
ran. A new `cornered_3x3` fixture — `10` in the four corners, `90`
everywhere else — separates the two commands cleanly: with the
top-left corner selected, Similar at tolerance `0` selects all four
corners (bounds `(0, 0)–(3, 3)`, shape `Mask`) while Grow at the same
tolerance keeps only the one. On `ramped_3x3`, `40` and `50` selected
together yield range `40..=50`: tolerance `5` (`35..=55`) adds nothing
and tolerance `10` (`30..=60`) adds `30` and `60`, giving `[[F, F, T],
[T, T, T], [F, F, F]]`; the whole middle row selected (`40..=60`) at
tolerance `10` reaches `30` and `70` but not `20` or `80`. Alpha is
part of the comparison, as it is for the Wand: on `depth_ramped_3x3`
the centre `(50, α 128)` at tolerance `10` selects only itself, its
`40` and `60` row-neighbours sitting at α `0` and `255`, far outside
`118..=138`. With nothing selected the call errors with "Nothing is
selected"; with a rectangle selected an unknown layer errors and the
rectangle survives. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous ninety-nine: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1005 Rust tests total** (1000 → 1005, 998 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 153 — Magic Eraser tool

`magic_erase(id, x, y, tolerance, contiguous, opacity)` erases to
transparency every pixel of layer `id` that the Magic Wand would select
from a click at `(x, y)` — within `tolerance` of the clicked colour on
every RGBA channel, 4-connected from the click when `contiguous` is set
or anywhere on the layer when not — scaling each pixel's alpha by `1 −
opacity` (`opacity` in `0..=255`; `255` erases outright), the same
multiply-toward-zero the Eraser stroke applies, so colour bytes are
left alone and only coverage changes. The Wand's region computation
moved out of `select_magic_wand` into a shared `wand_bits` function
that both the Wand and the Magic Eraser call; the Wand's own tests run
unchanged through it. Like every painting tool the eraser is confined
to the active selection: only selected pixels of the region are erased,
and a click on an unselected pixel erases nothing and reports `None`
rather than erroring, as the Paint Bucket does. It otherwise returns
the erased region's bounding box, and errors on a locked or unknown
layer or a click off the canvas. Photoshop's Anti-alias and Sample All
Layers options are documented scope cuts, as they were for the Wand.
A new **Magic Eraser** tool button sits beside the Eraser; while it is
active the Wand's Tolerance slider and Contiguous checkbox appear in
the tool options (the two tools share them, as they share Photoshop's
Tolerance), and the Flow slider sets the erasure's Opacity.

**Verified two ways.** Five new `document.rs` tests, with the two
non-trivial alpha values cross-checked in Python emulating the Rust
`f32` arithmetic (`to_unit` = byte ÷ 255, `to_byte` = round half away
from zero of unit × 255). On `ramped_3x3` a click at `(0, 0)` with
tolerance `15` at full opacity clears exactly the Wand's own region —
`(0, 0)` and `(1, 0)` go to alpha `0`, every other pixel stays `255`,
the colour bytes `10` and `20` survive, and the returned box is `(0,
0)–(2, 1)`. Opacity `128` on an opaque pixel gives `255 × (1 − 128/255)
= 127.0` exactly → `127`; opacity `64` on `depth_ramped_3x3`'s alpha-128
centre gives `128 × (1 − 64/255) = 95.87` → `96`. On the `3×1` row `10
50 10` a contiguous click at the first pixel clears only it while a
non-contiguous click clears both `10`s (box `(0, 0)–(3, 1)`) and leaves
the `50` opaque. With `(0, 0)` and `(1, 0)` selected, tolerance `25`
reaches the whole top row but erases only the two selected pixels (box
`(0, 0)–(2, 1)`), and a click on the unselected `(2, 0)` changes nothing
and returns `None`. An off-canvas click, an unknown layer, and a locked
layer (error mentioning "locked", pixel untouched) all error. All five
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1010 Rust tests total** (1005 → 1010, 1003 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 154 — Edit > Paste Special > Paste Outside

`paste_outside(clipboard, name)` is the mirror image of Phase 146's
Paste Into: the clipboard becomes a new top layer centred on the active
selection's bounding box exactly as Paste Into centres it (the box's
top-left plus half the size difference, truncated toward zero), but
only the pixels that fall *outside* the selection's shape are kept, so
the pasted content surrounds the selection instead of filling it. The
two commands now share one private `paste_against_selection` routine
that differs in a single comparison, which makes their relationship
exact: pixel for pixel, one layer holds the clipboard byte and the
other is zero. An inverted selection flips what "outside" means but not
the bounding box the clipboard is centred on. As with Paste Into,
Photoshop's live layer mask is baked in as transparency (a documented
scope cut), and nothing selected is an error. A new **Paste Outside**
button sits beside Paste Into, enabled under the same conditions.

**Verified two ways.** Five new `document.rs` tests, every expected
pixel read off its fixture. The whole `ramped_3x3` pasted outside its
single selected centre pixel lands at origin `1 + (1 − 3) / 2 = 0` and
keeps everything but the centre — `[[10, 20, 30], [40, 0, 60], [70, 80,
90]]` with `(1, 1)` fully transparent and `(0, 0)` opaque `10`. Paste
Into and Paste Outside from the same clipboard and selection are exact
complements: for every byte of the three layers, `into + outside ==
original` and at least one of the two is `0`. The canvas-spanning
ellipse on `ramped_4x4`, which Paste Into's own test showed excludes
exactly the four corners, keeps exactly those corners here — `10`,
`40`, `130`, `160`, opaque, with everything else transparent. Inverting
the centre-pixel selection before pasting outside yields Paste Into's
un-inverted result, only the `50` at `(1, 1)`. Nothing selected errors
with a message naming "Paste Outside" and adds no layer. Paste Into's
own five Phase 146 tests run unchanged through the shared routine. All
five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and one: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1015 Rust tests total** (1010 → 1015, 1008 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 155 — Edit > Copy Merged

`copy_merged()` is Edit > Copy of what is actually on screen: every
visible layer composited together with its opacity and blend mode,
exactly as the canvas shows them, captured within the active
selection's shape (or the whole canvas) into the same `Clipboard` that
Copy fills, so Paste, Paste in Place, Paste Into, and Paste Outside all
take it unchanged. It reuses the app's own `composite::flatten` — the
single place the W3C source-over math lives, which the canvas view, Merge
Visible, and Flatten Image already share — so the copied bytes are the
displayed bytes by construction, not a second implementation of the
blend. Hidden layers and layers at zero opacity contribute nothing; an
all-hidden document errors, as Photoshop greys the command out. The
selection-masked extraction that Copy used on one layer's pixels now
takes any document-sized buffer (`extract(source, bounds)`), so the two
commands share it. Copy Merged is read-only like Copy: nothing to
checkpoint, no lock check. A new **Copy Merged** button sits beside
Copy, with **Shift+Ctrl+C** bound to it (plain Ctrl+C still copies the
selected layer alone).

**Verified two ways.** Five new `document.rs` tests on a new
`ramped_3x3_with_overlay` fixture — `ramped_3x3` plus a second layer
holding one opaque pixel at `(0, 0)` and nothing else — with the one
blended value cross-checked in Python emulating the composite's `f32`
arithmetic. With an opaque `200` overlaid, Copy Merged's `3×3`
clipboard (origin `(0, 0)–(3, 3)`) reads `200` at `(0, 0)` and the base's
`20` and `90` at `(1, 0)` and `(2, 2)`; hiding the overlay puts the base's
`10` back. An opaque `255` red at 50% layer opacity over the base's `10`
gives `0.5 × 1.0 + 0.5 × 10/255 = 0.5196` → `133` at full alpha, the
canvas's own bytes. A `(1, 1)–(3, 3)` rectangle yields a `2×2` clipboard
at that origin holding exactly `50 60 / 80 90`, and the canvas-spanning
ellipse on `ramped_4x4` drops exactly the corners (`(0, 0)` transparent,
`(1, 0)` opaque `20`), as Copy's own tests established. Hiding the only
layer errors with a message mentioning "visible". All five passed on the
first run; Copy's and Cut's own tests run unchanged through the shared
extraction.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and two: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new button's and shortcut's wiring was reviewed by
hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1020 Rust tests total** (1015 → 1020, 1013 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 156 — Move Selection

`move_selection(dx, dy)` shifts the selection outline by a pixel offset
without touching any layer's pixels — what dragging from inside a
selection with a marquee tool does in Photoshop, and what its arrow
keys nudge. A geometric selection whose shifted bounding box still fits
on the canvas keeps its shape, inversion, and border exactly and simply
moves. One pushed partly past the canvas edge is rasterised first
(through Phase 151's `selected_bits`) and moved as a pixel mask, because
a clipped ellipse is no longer an ellipse and the shape-plus-box
representation cannot say "an ellipse with its right third missing" —
pixels moved off the canvas are dropped and nothing comes in from
beyond it, matching Photoshop's own clipping of a selection at the
canvas edge. A mask selection always moves as a mask. Nothing selected,
or a move that would leave nothing selected, errors and leaves the
selection intact. In the frontend the arrow keys nudge the selection by
1 px (10 px with Shift) whenever a marquee tool is active and something
is selected — the plain-arrow branch of the keyboard handler that until
now had nothing to do — and a new **Move Selection…** dialog beside
Similar takes an exact horizontal and vertical offset. Photoshop's
drag-to-move gesture on the marquee itself is a documented scope cut,
as is Transform Selection (next on the list).

**Verified two ways.** Five new `document.rs` tests, every expected
value hand-derived and the three mask cases reproduced by a
four-line Python model of the bitmap shift before the Rust tests ran.
A `2×2` ellipse at the origin of a `4×4` canvas moved by `(1, 2)` is
still an `Ellipse` with bounds `(1, 2)–(3, 4)`, and `(−1, −2)` brings it
back. An inverted `2×2` rectangle moved by `(1, 1)` stays inverted —
`(0, 0)` selected, `(1, 1)` not, `(3, 3)` selected — and a `3×3`
rectangle with a 1 px border moved by `(1, 1)` keeps `border = 1`, with
`(1, 1)` and `(3, 3)` on the band, `(2, 2)` in the hole, and `(0, 0)`
outside. The `2×2` at `(1, 1)` of `ramped_3x3` moved right by one would
span columns `2..4` on a 3-wide canvas, so it becomes the `Mask` of its
surviving column, bounds `(2, 1)–(3, 3)`. The four corners of
`cornered_3x3` (selected via Similar) moved right by one leave exactly
`(1, 0)` and `(1, 2)`, the right-hand corners having left the canvas.
Nothing selected errors with "Nothing is selected"; a single selected
pixel moved three columns off errors with "nothing selected" and stays a
`Rectangle` at `(0, 0)–(1, 1)`; a `(0, 0)` move is a harmless no-op. All
five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and three: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's and the arrow keys' wiring was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1025 Rust tests total** (1020 → 1025, 1018 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 157 — Select > Save Selection / Load Selection

`save_selection(name)` stores the active selection under a name —
shape, bounds, inversion, border, and a mask's bitmap all included —
and `load_selection(name)` brings it back as the active selection,
Photoshop's "New Selection" load operation. Saving under a name that
already exists replaces that saved selection (Photoshop's dialog offers
to replace the channel), names are trimmed, and the saved list keeps
its first-saved order. Photoshop stores saved selections as alpha
channels in the Channels panel; this project has no channels, so they
live as named selections on the `Document` (`saved_selections`) and
travel through undo/redo with it — a mask's bitmap is shared through
its `Arc`, so saving costs a pointer, not a canvas. Like the active
selection and the reselect memory, saved selections are discarded when
the canvas changes size (`rotate_document_90`, `crop`), since their
coordinates would no longer mean anything. Load's Add/Subtract/
Intersect operations and Save's channel-combination operations are
documented scope cuts. The `DocumentView` gains `savedSelections`, the
list of names, mirrored in `types.ts`; a **Save Selection…** dialog
takes a name and a **Load Selection…** dialog offers the saved names in
a drop-down, enabled only when there is something to load.

**Verified two ways.** Five new `document.rs` tests, each asserting
the selection read back pixel by pixel or field by field. An inverted
ellipse `(0, 0)–(3, 2)` saved as "ring", deselected, replaced by a
rectangle, and loaded again comes back an inverted `Ellipse` with the
same bounds, and the view lists `["ring"]`. The four corners of
`cornered_3x3` selected via Similar, saved, deselected, and loaded
reproduce the exact `Mask` bitmap. Saving "a", then "b", then " a " (a
different rectangle each time) leaves the names `["a", "b"]` in that
order with "a" holding its newest rectangle `(2, 2)–(3, 3)` and "b" its
own `(1, 1)–(2, 2)`. Saving with nothing selected errors with "Nothing
is selected", a blank name errors mentioning "name", and loading a name
never saved errors quoting it — all leaving the saved list empty and
the active rectangle intact. A saved selection does not survive
`rotate_document_90` or `crop`: the view's list empties and loading it
errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and four: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The two dialogs' wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1030 Rust tests total** (1025 → 1030, 1023 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 158 — Selection modes: New, Add, Subtract, Intersect

The marquee tools gain Photoshop's four combination modes. A new
`SelectionMode` enum (`New`, `Add`, `Subtract`, `Intersect`) rides on
`select_rectangle_with` and `select_ellipse_with`; the existing
`select_rectangle` and `select_ellipse` are now one-line `New` calls
through them, so nothing that used them changes. `New` replaces the
selection outright, and so do `Add` and `Intersect` when nothing is
selected yet — Photoshop starts a fresh selection rather than adding to
or intersecting with nothing — while `Subtract` with nothing selected
is an error. Otherwise the current selection, whatever its shape,
inversion, border, or mask, is rasterised through Phase 151's
`selected_bits` and combined pixel by pixel with the new marquee
(union, difference, or intersection) into a pixel-mask selection — the
one representation that can hold two rectangles at once. A `Subtract`
or `Intersect` that would leave nothing selected errors and leaves the
selection intact. A combined selection is always a mask, even when the
union of two rectangles happens to be a rectangle: detecting that
would buy nothing, since every command already honours masks. In the
frontend the `select_rectangle` and `select_ellipse` commands take an
optional `mode` (absent means `New`, so the single-row and single-column
marquees are untouched); a **Mode** drop-down appears in the tool
options while a marquee tool is active, and Photoshop's modifiers work
while dragging — Shift adds, Alt subtracts, Shift+Alt intersects —
overriding the drop-down for that one drag. The Magic Wand, Color
Range, and Load Selection keep replacing the selection for now; giving
them modes is a documented follow-up.

**Verified two ways.** Five new `document.rs` tests reading the
selection back pixel by pixel through `selected_grid` and `contains`,
each expected grid derived by hand from the two shapes' pixel sets.
`(0, 0)` plus an added `(2, 2)` on `ramped_3x3` selects exactly those
two corners as a `Mask` with bounds `(0, 0)–(3, 3)`. Subtracting the
centre pixel from Select All leaves the eight-pixel ring, subtracting
with nothing selected errors with "Nothing is selected", and
subtracting everything that is left errors with "nothing selected" and
keeps the ring. Intersecting the `2×2` at the origin with the `2×2` at
`(1, 1)` leaves only `(1, 1)`; intersecting two disjoint rectangles
errors and keeps the first, still a `Rectangle` at `(0, 0)–(2, 2)`. Add
and Intersect with nothing selected produce a plain `Rectangle` and
`Ellipse` respectively, and `New` over an existing selection replaces
it. Select All minus the canvas-spanning ellipse on `4×4` leaves exactly
the four corners (the ellipse's corner-exclusion Paste Into's tests
established), and adding the centre pixel to an inverted rectangle
works on the inverted result — `(0, 0)` and `(1, 1)` selected, `(2, 2)`
not. All five passed on the first run; every earlier marquee test runs
unchanged through the `New` path.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and five: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The Mode drop-down's and the modifier keys' wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1035 Rust tests total** (1030 → 1035, 1028 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 159 — Image > Apply Image

`apply_image(target, source, blend, opacity, invert,
preserve_transparency)` blends a source — any layer, or with `None` the
merged composite of every visible layer — onto the target layer with a
blend mode and an opacity in percent, as if the source were a layer
stacked on top of the target and merged down. Per channel it is the
same W3C source-over math the canvas composite uses (`Cs′ = (1 − αb)·Cs
+ αb·B(Cb, Cs)`, `αo = αs + αb(1 − αs)`, `Co = (αs·Cs′ + αb·Cb(1 −
αs)) / αo`, with `αs` the source alpha scaled by the opacity), so the
result is exactly what stacking and merging would show. *Invert*
inverts the source's colour (not its alpha) before blending. *Preserve
Transparency* keeps the target's coverage exactly: colour mixes toward
the blended value by the source's effective alpha, with the backdrop
treated as opaque for the blend, but alpha never changes, so fully
transparent target pixels stay untouched — Lock Transparent Pixels, in
effect, and this project's explicit definition of an option Photoshop
documents only by outcome. It runs through `filter_pixels`, so it is
confined to the selection and reads the source from a snapshot (a
layer may be applied to itself). It errors on a locked or unknown
target, an unknown source, or an opacity over 100. Photoshop's
single-channel sources, its mask options (Mask, Mask Layer, Mask
Channel, Transparency Mask, Mask Invert), and its live preview are
documented scope cuts. A new **Apply Image…** dialog beside Copy
Merged offers a Source drop-down (Merged plus every layer, top to
bottom), all twelve blend modes, an Opacity field, and the two
checkboxes.

**Verified two ways.** Five new `document.rs` tests on a new
`apply_image_fixture` — `ramped_3x3` under a "target" layer of opaque
`100` red with a fully transparent `(0, 0)` and a half-transparent `(2,
2)` — with the three blended values cross-checked in Python emulating
the composite's `f32` arithmetic. Normal at 100%: opaque over opaque
replaces (`(1, 1)` → `50`), the source shows through the transparent
pixel (`(0, 0)` → `10, α 255`), the half-transparent pixel becomes the
source at full alpha (`(2, 2)` → `90, α 255`), the dirty box is the
canvas, and the source layer is untouched. Multiply gives `100/255 ×
50/255 → 19.6 → 20`; Normal at 50% gives `(100 + 50) / 2 = 75`;
inverted Normal applies the source `(50, 0, 0)` as `(205, 255, 255)`,
every channel flipped. With Preserve Transparency the
transparent `(0, 0)` stays `[0, 0, 0, 0]`, `(2, 2)` becomes `90` at its
own α `128`, and `(1, 1)` is `50`. With no source and the target hidden,
the merged image is the base ramp alone, so `(1, 1)` → `50` and `(0,
0)` → `10`. A one-pixel selection at `(0, 0)` confines the apply to that
pixel (`(1, 1)` stays `100`); opacity `101` errors mentioning "Opacity",
and an unknown source, an unknown target, and a locked target all
error. Four of the five passed on the first run; the Invert expectation
had been written as if only the red channel flipped (`205, 0, 0`), and
the test, not the code, was corrected to the full inversion the
Python model and Photoshop both give.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and six: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The dialog's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1040 Rust tests total** (1035 → 1040, 1033 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 160 — Dodge tool

`Stroke::Dodge { exposure }` is a new brush for `Document::stroke`: every
pixel the brush covers has its colour lifted toward white by
`exposure` percent (`0..=100`) scaled by the brush's coverage — per
channel `c + (1 − c) · exposure · coverage` — so the stroke's soft
1 px edge lightens less than its body, exactly as the Brush's edge
paints less. Alpha is left alone, and fully transparent pixels are
skipped: they have no tone to lift, and Photoshop's Dodge leaves them
alone too. This is Photoshop's Midtones range; its Shadows and
Highlights ranges (which weight the effect by the pixel's own tone)
and Protect Tones are documented scope cuts. Coverage within one
stroke is maxed rather than summed, as for every stroke here, so
passing back over the same pixels in a single drag does not compound —
a second stroke does. A new **Dodge** tool button sits beside the
Magic Eraser; the Flow slider sets its Exposure, and the colour swatch
is disabled for it as for the erasers.

**Verified two ways.** Five new `document.rs` tests, the four
non-trivial bytes cross-checked in Python emulating the Rust `f32`
arithmetic (coverage from `point_segment_distance`, `to_unit`,
`to_byte`). A radius-3 dot at exposure 50 on a solid `(100, 0, 200)`
layer gives `(178, 128, 228)` — `100/255 + (155/255)/2 = 177.5 → 178`,
`0 → 127.5 → 128`, `200 → 227.5 → 228` — at unchanged alpha. Exposure 0
is an identity and exposure 100 reaches pure white. A radius-1 dot at
`(1, 1)` covers pixel `(0, 0)` — whose centre is `√0.5` away — at
`1 − 0.7071 + 0.5 = 0.7929`, lifting `100` to `161`. A half-transparent
pixel keeps its alpha `128` while its colour lifts, and a fully
transparent pixel is untouched byte for byte. A one-pixel selection
confines the stroke, and a locked layer errors. All five passed on the
first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and seven: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1045 Rust tests total** (1040 → 1045, 1038 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 161 — Burn tool

`Stroke::Burn { exposure }` is Dodge's mirror: every pixel the brush
covers has its colour pulled toward black by `exposure` percent scaled
by the brush's coverage — per channel `c · (1 − exposure · coverage)` —
with alpha left alone and fully transparent pixels skipped, exactly as
Dodge does. The two tools share one arm of `stroke`'s match, differing
only in the direction of the move, so Burn's soft-edge behaviour,
transparency handling, and stroke-coverage maxing are Dodge's by
construction. Midtones only, with Shadows/Highlights and Protect Tones
documented scope cuts, as for Dodge. A **Burn** tool button sits beside
Dodge, driven by the same Flow-as-Exposure slider. One consequence
worth stating: Dodge followed by Burn at the same exposure is not an
identity — Dodge moves by a fraction of the distance to white and Burn
by a fraction of the distance to black, which differ unless the pixel
is mid-grey — and Photoshop's pair behave the same way.

**Verified two ways.** Five new `document.rs` tests, the four
non-trivial bytes cross-checked in Python emulating the Rust `f32`
arithmetic. A radius-3 dot at exposure 50 on solid `(100, 200, 255)`
halves every channel to `(50, 100, 128)` (`255 × 0.5 = 127.5 → 128`) at
unchanged alpha. Exposure 0 is an identity and exposure 100 reaches
pure black. The same radius-1 dot as Dodge's test covers pixel `(0, 0)`
at `0.7929`, taking `200` to `200 × (1 − 0.5 × 0.7929) = 120.7 → 121`.
A half-transparent pixel keeps its alpha `128` while its colour
darkens, and a fully transparent pixel is untouched byte for byte.
Dodge then Burn at exposure 50 takes `100 → 178 → 89`, and a locked
layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and eight: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1050 Rust tests total** (1045 → 1050, 1043 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 162 — Sponge tool

`Stroke::Sponge { flow, saturate }` completes the toning trio: every
pixel the brush covers has its HSL saturation moved by `flow` percent
scaled by the brush's coverage — toward full saturation, `s + (1 − s) ·
flow · coverage`, in Saturate mode, or toward grey, `s · (1 − flow ·
coverage)`, in Desaturate mode — through the project's own `rgb_to_hsl`
/ `hsl_to_rgb` pair, so hue and lightness are held exactly and only
saturation moves. Alpha is left alone and fully transparent pixels are
skipped, as for Dodge and Burn. One guard is this project's own: a grey
pixel has zero saturation and therefore no hue, and pushing its
saturation up would have invented hue 0 — red — out of nothing (the
independent Python model of the HSL round trip showed exactly that
before the guard was written), so greys are left untouched in both
modes, which is also what Photoshop's Sponge does to them. Photoshop's
Vibrance option is a documented scope cut. A **Sponge** tool button
sits beside Burn with a Desaturate/Saturate Mode drop-down in the tool
options while it is active; the Flow slider is its Flow.

**Verified two ways.** Five new `document.rs` tests, every byte
cross-checked in Python emulating the Rust `f32` HSL round trip
(`rgb_to_hsl`, `hsl_to_rgb`, `to_unit`, `to_byte`, and the brush's
coverage). `(200, 100, 100)` is HSL `(0°, 0.476, 0.588)`: Desaturate at
flow 100 drops it to the grey of its lightness, `(150, 150, 150)`, and
flow 50 halves the saturation to `(175, 125, 125)`. Saturate at flow
100 reaches full saturation at that lightness, `(255, 45, 45)`, flow
50 lands at `s = 0.738 → (228, 73, 73)`, and a green `(100, 200, 100)`
keeps its hue on the way to `(45, 255, 45)`. The radius-1 edge coverage
of `0.7929` at flow 50 desaturates to `(180, 120, 120)`. Saturating a
layer whose `(2, 2)` is grey `150` leaves that pixel byte-identical, an
alpha-128 pixel keeps its alpha while saturating, a transparent pixel
is untouched, and flow 0 is an identity. A one-pixel selection confines
the stroke, and a locked layer errors. Four of the five passed on the
first run: the Saturate test had chained its flow-100 stroke onto the
pixel the flow-50 stroke had already lifted, whose byte-rounded
lightness differs by a hair from the original's, giving `(255, 46, 46)`
rather than the `(255, 45, 45)` the model derives from the original —
the test, not the code, was corrected to a fresh layer.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and nine: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1055 Rust tests total** (1050 → 1055, 1048 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 163 — Blur tool

`Stroke::Blur { strength }` is the first neighbourhood brush: every
pixel the brush covers moves — all four channels — toward the radius-1
box blur of the layer (`box_blur_at`, the same edge-clamped average
Filter > Blur > Box Blur uses) by `strength` percent scaled by the
brush's coverage. The blur is read from a snapshot of the layer taken
before the stroke, so a stroke never smears its own output along its
path: dragging across a region gives exactly the result of dotting it,
and at full strength a stroke covering the whole layer is byte for byte
Box Blur at radius 1. `Document::stroke` now takes that snapshot for
neighbourhood tools only (a clone of the layer per stroke call, which
the Sharpen tool will share next); the pixel-local tools pay nothing.
Photoshop's Sample All Layers and its per-stroke blend mode are
documented scope cuts. A **Blur** tool button sits beside Sponge; the
Flow slider is its Strength.

**Verified two ways.** Five new `document.rs` tests, the blur grid and
the two scaled bytes cross-checked in Python (integer-truncating
average, then `f32` `lerp`, `to_unit`, `to_byte`). A radius-3 dot at
full strength on `ramped_3x3` yields exactly Box Blur's radius-1 grid,
`[[23, 30, 36], [43, 50, 56], [63, 70, 76]]` — the corner being `((10 +
10 + 20) × 2 + 40 + 40 + 50) / 9 = 23` — and the layer's bytes equal
`box_blur(id, 1)`'s. Strength 50 moves the corner half way, `16.5 →
17`, and strength 0 is an identity. The radius-1 edge coverage of
`0.7929` moves the corner `10.3` of the way to `20`. On
`depth_ramped_3x3` the alpha columns `0 / 128 / 255` average to `127`
at the centre while its red stays `50`, and a one-pixel selection
confines the stroke. A stroke dragged from corner to corner gives the
same grid as the single dot, proving the snapshot read, and a locked
layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and ten: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1060 Rust tests total** (1055 → 1060, 1053 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 164 — Sharpen tool

`Stroke::Sharpen { strength }` is the Blur tool's opposite and shares
its machinery: each covered pixel's R, G, and B move *away* from the
radius-1 box blur of the pre-stroke layer by `strength` percent scaled
by the brush's coverage — the unsharp-mask formula `original +
(original − blurred) · amount`, rounded and clamped, with no threshold
— which is exactly what Filter > Sharpen > Sharpen More computes at
amount 1.0, so a full-strength stroke covering the whole layer is byte
for byte that filter. Alpha is left alone (sharpening is a contrast
operation, not a coverage one, as `unsharp_mask` already says), and the
blur is read from the same pre-stroke snapshot Blur takes, so a drag
never sharpens pixels it has already sharpened. A **Sharpen** tool
button sits beside Blur; the Flow slider is its Strength. Photoshop's
Sample All Layers, Protect Detail, and per-stroke blend mode are
documented scope cuts.

**Verified two ways.** Five new `document.rs` tests, every byte
cross-checked in Python (`f32` arithmetic, round half away from zero,
clamp). A radius-3 dot at full strength on `ramped_3x3` gives `[[0,
10, 24], [37, 50, 64], [77, 90, 104]]` — the corner's `10 − 13` clamping
to `0`, the far corner's `90 + 14 = 104`, the centre unchanged — and
the layer's bytes equal `sharpen_more`'s. Strength 50 gives `10 − 6.5 =
3.5 → 4` and `90 + 7 = 97`; strength 0 is an identity. Pixel `(1, 0)`
at the radius-1 edge coverage of `0.7929` moves `20` by `−10 × 0.7929`
to `12.07 → 12`. On `depth_ramped_3x3` the centre keeps its alpha `128`
(and its symmetric red `50`) and the transparent column stays
transparent; a one-pixel selection at `(2, 2)` sharpens only it. A
stroke dragged corner to corner gives the same grid as the dot, and a
locked layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and eleven: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1065 Rust tests total** (1060 → 1065, 1058 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 165 — Red Eye tool

`red_eye(id, x, y, darken)` fixes a flash-lit pupil from one click.
From the clicked pixel it floods the 4-connected region of red-dominant
pixels — red exceeding the larger of green and blue by more than 50
levels, this project's explicit, stated definition of "red eye" (skin
and lips, whose red leads by less, fall outside it) — then neutralises
each pixel of the region: its red is replaced by the mean of its green
and blue, so the pupil turns grey, and all three channels are scaled by
`1 − darken` percent (Photoshop's Darken Amount), rounded half up in
integer arithmetic; alpha is untouched. Like the Paint Bucket and the
Magic Eraser it is confined to the active selection — only selected
pixels of the region change, and a click on an unselected pixel does
nothing and returns `None` — and it returns the fixed region's bounding
box otherwise. A click on a pixel that is not red-dominant errors
("that pixel isn't red"), as does a locked or unknown layer, an
off-canvas click, or a Darken Amount over 100. Photoshop's Pupil Size
option is a documented scope cut. A **Red Eye** tool button sits beside
Sharpen; the Flow slider is its Darken Amount.

**Verified two ways.** Five new `document.rs` tests on a new
`red_eye_3x3` fixture — a plus of pupil pixels `(200, 40, 40)` on skin
`(220, 180, 160)`, whose red leads by only 40 — with every expected
byte checked in integer Python before the Rust tests ran. A click on
the centre at Darken 50 turns all five pupil pixels `(20, 20, 20)` —
red to the mean `40`, then halved — leaves the four skin corners
untouched, and reports the box `(0, 0)–(3, 3)`. Darken 0 gives `(40,
40, 40)` and Darken 100 gives black; an uneven pupil `(180, 20, 60)` at
alpha 128 becomes `(20, 10, 30)` with its alpha kept. With only the centre
and a corner red, a click on the centre fixes it alone (box `(1, 1)–(2,
2)`) and the corner, sharing no edge with it, is not reached. A click
on skin errors
mentioning "red"; a selection of the top two rows fixes only them (box
`(0, 0)–(3, 2)`, the bottom pupil pixel untouched) and a click on an
unselected pupil pixel returns `None`. An off-canvas click, an unknown
layer, Darken 101, and a locked layer all error. Four of the five
passed on the first run: the contiguity test had first placed its
"disconnected" red pixel in a corner of the plus fixture, where it
shares an edge with two arm pixels and is rightly reached — the test,
not the code, was corrected to a fixture whose two red pixels share no
edge.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twelve: this
session's Xvfb instance was already confirmed, through a control test
and a full Xvfb-and-application restart in Phase 52, to have stopped
delivering synthetic `xdotool` pointer clicks to the webview entirely,
and re-running that diagnostic again was judged unlikely to produce
new information. The new tool's wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1070 Rust tests total** (1065 → 1070, 1063 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 166 — Ruler tool

The first measuring tool. `measure(x0, y0, x1, y1)` is pure geometry
in `document.rs` — it touches no document — returning a `Measurement`
of a drag in document pixels: the horizontal and vertical extents, the
straight-line distance (`hypot`), and the angle in degrees measured
counter-clockwise from the positive x axis with y pointing up on
screen, in `−180..=180`, which is Photoshop's Info-panel convention (a
drag up and to the right reads positive; a zero-length drag reads
`0°`). Non-finite coordinates error. A `ruler_measure` Tauri command
exposes it without touching the document lock, and a new **Ruler**
tool button (enabled whenever a document is open, since measuring needs
no layer) captures a drag on the canvas and shows the readout in the
status bar as `W H D A°`, staying put until the next drag. Photoshop's
Straighten Layer button and the Alt-drag protractor leg are documented
scope cuts.

**Verified two ways.** Five new `document.rs` tests, every value
cross-checked against Python's `math.hypot` / `math.atan2` (the tests
compare to `1e-4` px and `1e-3°`, well inside `f32`'s precision for
these magnitudes). A `(0, 0) → (3, 4)` drag reads `W 3 H 4 D 5` at
`−53.1301°` (down and right on screen is negative); the same segment
dragged the other way reads `126.8699°`, and `(0, 0) → (−1, 1)` reads
`√2` at `−135°`. Axis-aligned drags read `0°` (right), `90°` (up), and
`180°` (left); a zero-length drag reads all zeros; a NaN or infinite
coordinate errors. Four of the five passed on the first run, and this
time the test caught a real defect: the leftward drag read `−180°`,
because negating a zero vertical offset produces IEEE `−0.0`, whose
`atan2` against a negative `dx` is `−180` — the code now computes `0.0
− dy`, which is `+0.0`, and reads `180°` as Photoshop does. (Clippy
separately insisted the `√2` expectation be spelled as the `SQRT_2`
constant rather than a seven-digit literal.)

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and thirteen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new tool's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1075 Rust tests total** (1070 → 1075, 1068 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 167 — Color Sampler tool

`sample_points(points)` returns the composited RGBA8 value under each
of up to ten points, in order — what the canvas shows there, every
visible layer flattened with its opacity and blend mode, exactly as the
eyedropper samples. It rides on a new `composite::composite_pixel`,
which runs the compositor's single per-pixel routine
(`composite_layers_pixel`, the one place the W3C blend math lives) for
one pixel instead of flattening the whole document per readout. More
than ten points, or a point off the canvas, is an error; a fully
transparent spot reads `[0, 0, 0, 0]` as `flatten` writes it. In the
frontend a **Color Sampler** tool button places a sampler per click
(clicks beyond ten are ignored, as Photoshop refuses an eleventh), a
**Clear Samplers** button appears in the tool options while it is
active, and the status bar lists every sampler as `#n R G B A`,
re-read after every edit — each snapshot hands back a fresh document
view, so keying the readout effect on it catches every change, the
same trick the RGB Levels readout uses. Samplers live in the frontend
(they are a viewing aid, not document state, so they neither undo nor
save), and one placed beyond the canvas after a crop is simply dropped
from the readout. Photoshop's Current Layer sampling mode and its
sample-size averaging (3×3, 5×5, …) are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests, with the one
blended value cross-checked earlier in Copy Merged's Python emulation
and the whole-image case checked against `flatten` itself. On
`ramped_3x3_with_overlay` the points `(1, 1), (0, 0), (2, 2)` read
`50, 200, 90` in that order and an empty list reads empty. The overlay
at 50% layer opacity over the base's `10` reads `133`, and hidden reads
`10`. On `depth_ramped_3x3` the transparent `(0, 1)` reads `[0, 0, 0,
0]` and the centre reads `[50, 0, 0, 128]`. Sampling all nine points of
a half-transparent overlay reproduces `flatten`'s bytes exactly. An
off-canvas point errors, eleven points error mentioning "ten", and ten
are accepted. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fourteen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new tool's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1080 Rust tests total** (1075 → 1080, 1073 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 168 — Count tool

`add_count_mark(x, y)` places the next numbered mark at a pixel and
returns the running total, which is that mark's number;
`clear_count_marks()` removes them all; `count_marks()` lists them in
placement order, and the `DocumentView` carries them as `countMarks`.
Unlike the Color Sampler's points, count marks are document data in
Photoshop — they save with the file and undo — so they live on the
`Document` here too: both commands go through the checkpointed edit
path, so placing or clearing marks is one undo step each, and, like
the active selection, the reselect memory, and the saved selections,
marks are discarded when the canvas changes size (`rotate_document_90`,
`crop`), their coordinates no longer meaning anything. A click off the
canvas errors. Photoshop's multiple count groups, custom colours, and
marker/label sizes are documented scope cuts. A new **Count** tool
button places a mark per click, a **Clear Count** button appears in
the tool options while it is active, every mark is drawn on the canvas
as a numbered badge positioned by the same percentage mapping the
selection outline uses, and the status bar shows `Count N`.

**Verified two ways.** Five new `document.rs` tests reading the marks
back through both the accessor and the view. Three marks at `(2, 0)`,
`(0, 2)`, `(2, 0)` are numbered `1, 2, 3` and listed in that order —
the same pixel may be counted twice, as in Photoshop. Clearing empties
both readings and the next mark is numbered `1` again. Marks at `(3,
0)` and `(0, 3)` on a `3×3` canvas error and leave the list empty. A
mark survives a pixel edit and a selection change but not a document
rotation or a crop. A new document has no marks. All five passed on
the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fifteen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new tool's wiring and the badge overlay
were reviewed by hand instead. Every other layer of this project's
quality bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings`, `npm run build`) is fully green.

**1085 Rust tests total** (1080 → 1085, 1078 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 169 — Note tool

`add_note(x, y, text)` pins a text annotation to a pixel and returns
its index; `set_note_text(index, text)` rewrites it, `remove_note(index)`
deletes it (later notes shift down), `clear_notes()` removes them all,
and `notes()` lists them in placement order — a new `Note { x, y, text
}` struct the `DocumentView` carries as `notes`. Text is trimmed and
must not be blank; a click off the canvas or an unknown index errors,
the latter naming the note by its 1-based number as the UI shows it.
Notes are document data in Photoshop — they save with the file and
undo — so, like count marks, they live on the `Document`, every change
is one checkpointed undo step, and they are discarded when the canvas
changes size. Photoshop's note author, colour, and audio annotations
are documented scope cuts. A new **Note** tool button opens a dialog
for the clicked pixel; each note is drawn on the canvas as a clickable
badge whose tooltip is its text and whose click reopens the dialog to
edit or delete it; a **Clear Notes** button appears in the tool
options while the tool is active, and the status bar shows `Notes N`.

**Verified two ways.** Five new `document.rs` tests reading the notes
back through both the accessor and the view. Notes at `(2, 0)` and
`(0, 2)` get indices `0` and `1`, `"  first "` is stored trimmed, and
the view's list equals the accessor's. Rewriting the middle of three
notes to `" bee "` stores `bee`, removing the first shifts the others
down, and clearing empties the list. Blank text errors on add
(mentioning "text") and on rewrite, leaving the existing note intact.
Off-canvas pins error, and rewriting or removing note `0` on an empty
document errors mentioning "#1". A note survives a pixel edit but not a
document rotation or a crop. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and sixteen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The tool, dialog, and badge wiring were
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1090 Rust tests total** (1085 → 1090, 1083 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 170 — Paint Symmetry

`stroke_symmetric(id, points, radius, stroke, symmetry)` is
`Document::stroke` applied to the stroke's points and then to their
mirror image(s) about the canvas centre — across the vertical centre
line (`x ↦ width − x`), the horizontal one (`y ↦ height − y`), or both,
which adds the diagonal copy for four in all — returning the union of
the copies' dirty rectangles; with no symmetry it is exactly one
stroke. A new `Symmetry` enum (`Vertical`, `Horizontal`, `Both`)
names Photoshop's Vertical, Horizontal, and Dual Axis modes. Each copy
is its own `stroke` call, so every copy honours the selection and the
lock independently, the neighbourhood tools' pre-stroke snapshot is per
copy, and copies that overlap compound as two strokes would — stated
here rather than hidden. The `paint_stroke`, `erase_stroke`, and
`pattern_stamp_stroke` commands take an optional `symmetry` (absent
means off, so the existing calls are untouched) and a **Symmetry**
drop-down appears in the tool options while the Brush, Eraser, or
Pattern Stamp is active. Photoshop's Circular, Spiral, Mandala, and
Radial symmetries, and its movable axis, are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests on a transparent
`4×4` with a radius-`0.5` red dot at pixel `(0, 0)`'s centre — the
coverage arithmetic (`0.5 − 0 + 0.5 = 1` on the dot's own pixel, `0.5 −
1 + 0.5 = 0` on its neighbours) checked by hand so exactly one pixel
per copy is painted. Off paints `(0, 0)` alone with box `(0, 0)–(1,
1)`; Vertical paints `(0, 0)` and `(3, 0)` (the mirror of `x = 0.5` is
`3.5`) with box `(0, 0)–(4, 1)`; Horizontal paints `(0, 0)` and `(0,
3)`; Dual Axis paints all four corners, opaque red, with the box the
whole canvas. A selection covering only the left half lets Dual Axis
land `(0, 0)` and `(0, 3)` but not the right-hand copies, and a locked
layer errors with nothing painted. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and seventeen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The drop-down's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1095 Rust tests total** (1090 → 1095, 1088 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 171 — Layer Comps

`save_layer_comp(name)` records every layer's visibility, opacity, and
blend mode under a name (replacing a same-named comp, keeping the list's
first-saved order), `apply_layer_comp(name)` restores them, and
`delete_layer_comp(name)` removes one; `layer_comp_names()` lists them
and the `DocumentView` carries the names as `layerComps`. The states
are keyed by layer id, so a comp is robust to reordering, and the two
ways a stack can drift after a comp is saved are handled the way
Photoshop's panel does: a layer deleted since is skipped on apply, and
a layer added since is left exactly as it is. Comps are document data
— every change is one checkpointed undo step — and, being about layers
rather than positions, they survive a canvas resize. Names are trimmed
and must not be blank; applying or deleting an unknown name errors,
quoting it. Photoshop also captures layer position and layer styles in
a comp; since layers here are document-sized and styles are baked in,
both are documented scope cuts. A new **Layer Comps…** dialog beside
Apply Image lists the saved comps with Apply and Delete buttons and
saves a new one from a name field.

**Verified two ways.** Five new `document.rs` tests reading the layer
states back field by field. Saving "day" with the overlay visible at
opacity `1.0` and Normal, then hiding it, halving its opacity, and
switching it to Multiply, and applying "day" restores all three, with
the untouched base layer unchanged and the view listing `["day"]`.
Saving "a", then "b", then " a " again after a change leaves the names
`["a", "b"]` with "a" holding the newer state; deleting "a" leaves
`["b"]`. A comp saved with two layers still applies after one is
deleted, and a layer added after the save is left hidden as it was. A
blank name errors mentioning "name", unknown names error quoting the
name on both apply and delete, and a comp survives
`rotate_document_90`. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and eighteen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The dialog's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1100 Rust tests total** (1095 → 1100, 1093 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 172 — Select > Transform Selection

`transform_selection(width_percent, height_percent, degrees, dx, dy)`
scales, rotates, and moves the selection outline about its own
bounding-box centre without touching any pixels. Every canvas pixel's
centre is mapped back through the inverse transform — undo the move,
then the rotation (clockwise positive, as Phase 136's `rotate`), then
the scale, the same arithmetic the layer transforms use — and tested
against the current selection with `Selection::contains`, so a rotated
ellipse stays an ellipse, a scaled rectangle a rectangle, and a mask a
mask; the result is a pixel-mask selection clipped to the canvas. It
errors when nothing is selected, on a non-positive scale or a
non-finite value, or when the result would select nothing, leaving the
selection intact each time. Photoshop's on-canvas handle gesture is a
documented scope cut; a new **Transform Selection…** dialog beside Move
Selection takes the five values instead.

**Verified two ways.** Five new `document.rs` tests reading the
selection back pixel by pixel through a new `selected_pixels` helper,
each expected set derived by hand from the inverse map and confirmed by
a Python model of it in `f32` before the Rust tests ran. The `2×2` at
`(1, 1)` on `6×6` scaled to 200% about its centre `(2, 2)` maps a pixel
centre `p` back to `2 + (p − 2) / 2`, inside `[1, 3)` for `p` from `0.5`
to `3.5`, so exactly the `4×4` at the origin is selected (`Mask`, bounds
`(0, 0)–(4, 4)`). A `3×1` bar at `(1, 2)` on `5×5` turned 90° becomes
the `1×3` bar at `(2, 1)`. The `4×2` ellipse at `(0, 1)` on `4×4` covers
the middle two rows (each pixel centre at normalised distance²
`0.8125`), and turned 90° about `(2, 2)` covers exactly the middle two
columns. A one-pixel selection moved by `(1, 0)` lands at `(1, 0)`, and
the identity transform of a `2×2` keeps the same four pixels as a
`Mask`. Nothing selected errors with "Nothing is selected"; a zero
scale and a NaN angle error; a move of 10 px off a 3-wide canvas errors
with "nothing selected" and keeps the `Rectangle`. All five passed on
the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and nineteen:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The dialog's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1105 Rust tests total** (1100 → 1105, 1098 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 173 — Move tool

`move_pixels(id, dx, dy)` is the Move tool. With no selection the whole
layer shifts, the vacated edge left transparent — a public front for
the private `translate` that Free Transform and Camera Raw Geometry
already used. With a selection only the selected pixels move: they are
lifted from a snapshot, their source cleared to transparent, and set
down at the offset overwriting whatever was there — Photoshop's cut-
and-drop — with the selection outline carried along through Phase
156's `move_selection` and dropped when it leaves the canvas entirely,
as Photoshop drops it. Reading from the snapshot is what makes a
one-pixel move of a run of pixels land each on its neighbour's old spot
rather than smearing. Pixels pushed off the canvas are lost. A zero
move is a no-op returning `None`; otherwise the whole canvas is reported
dirty. A locked or unknown layer errors before anything is touched. In
the frontend a **Move** tool button captures a drag on the canvas and
applies the rounded offset at pointer-up, and the arrow keys nudge the
layer by 1 px (10 px with Shift) while the tool is active — the same
branch of the keyboard handler that nudges the selection outline for
the marquees. Photoshop's Auto-Select, Show Transform Controls, and
alignment buttons are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests reading pixels and
the selection back by hand-derived position. `ramped_3x3` moved right
by one reads `[[0, 10, 20], [0, 40, 50], [0, 70, 80]]` with the vacated
column transparent, byte-equal to `translate`, and reports the whole
canvas dirty. With `(0, 0)` selected and moved by `(1, 1)`, the `10`
lands on `(1, 1)` (overwriting `50`), `(0, 0)` is transparent, `(1, 0)`
and `(2, 2)` are untouched, and the outline now sits at `(1, 1)–(2,
2)`. The pair `10 20` selected and moved right by one reads `0 10 20`
— each moved pixel read from the snapshot, not from a neighbour already
overwritten. The right-hand column selected and moved right by one is
cleared, nothing is written, the centre is untouched, and the selection
is dropped. A zero move returns `None` and changes nothing; an unknown
layer errors; a locked layer errors with its pixels and its `Rectangle`
selection intact. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The tool's drag and nudge wiring was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1110 Rust tests total** (1105 → 1110, 1103 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 174 — Polygonal Lasso tool

`select_polygon_with(mode, points)` takes the polygon through three or
more points (closed back to the first) and rasterises it to every
pixel whose centre it contains, using the even-odd rule through a new
`point_in_polygon` (a +x ray cast, each edge half-open in `y` so a ray
through a vertex counts once), into a pixel-mask selection combined
with the current selection per the same New/Add/Subtract/Intersect
mode the marquees take — Phase 158's `combine_selection` now builds
its rectangle or ellipse and hands it to a shared `combine_with` that
takes any prebuilt selection, mask included. The even-odd rule means a
self-crossing outline selects its odd-wound lobes, as Photoshop's lasso
does. Fewer than three points, a non-finite coordinate, or a polygon
covering no pixel centre errors, leaving the selection intact.
Anti-alias and Feather are documented scope cuts, as for every
selection here. A new **Polygonal Lasso** tool button places a vertex
per click, draws the outline so far as an SVG polyline over the
canvas, closes the polygon when the first vertex is clicked again
(within 3 px) or the **Close** button is pressed — Shift/Alt at that
click choose the mode, as for the marquees — and offers **Cancel**;
the Mode drop-down appears for it as it does for the marquees.

**Verified two ways.** Five new `document.rs` tests reading the
selection back pixel by pixel, every expected set derived by hand and
confirmed by a Python `f32` model of the same ray cast before the Rust
tests ran. The right triangle `(0, 0)–(4, 0)–(0, 4)` on `4×4` contains
a pixel centre exactly when `x + y < 3`: six pixels, `Mask`, bounds
`(0, 0)–(3, 3)`. The square `(1, 1)–(3, 3)` selects the same four
pixels as the rectangle marquee. Adding the small triangle `(0, 0)–(2,
0)–(0, 2)` to a one-pixel selection at `(3, 3)` gives `(0, 0)` and `(3,
3)`, and subtracting the unit square at the origin leaves `(3, 3)`. The
bow-tie `(0, 0)–(4, 3)–(4, 0)–(0, 3)` on `4×3`, whose diagonals pass
through no pixel centre, selects exactly its left and right lobes —
eight pixels. Two points, a NaN coordinate, and a polygon entirely off
the canvas all error, the last with "nothing selected", keeping the
`Rectangle`. Four of the five passed on the first run: the bow-tie test
had first been written on a `4×4` canvas, whose diagonals run straight
through pixel centres, and the Python model showed the half-open edge
rule admitting those boundary centres before the Rust test ran — the
fixture, not the code, was changed to the `4×3` bow-tie.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty-one:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The tool's click, close, and overlay wiring
was reviewed by hand instead. Every other layer of this project's
quality bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings`, `npm run build`) is fully green.

**1115 Rust tests total** (1110 → 1115, 1108 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 175 — Lasso tool

`select_lasso_with(mode, trail)` is the freehand Lasso: a pointer
drag's trail, closed back to its start, handed to Phase 174's polygon
fill after consecutive duplicate points — which a drag produces in
quantity whenever the pointer pauses — are dropped. It needs at least
three distinct points ("A lasso needs to enclose an area."), and a
trail that encloses no pixel centre, a straight line say, errors like
any empty polygon; both leave the selection intact. Everything else —
the even-odd fill, the pixel-mask result, the New/Add/Subtract/
Intersect combination — is the polygon's. A new **Lasso** tool button
captures a drag, appends every pointer move to the trail (kept in a
ref so moves append without re-rendering through stale state, and
mirrored to the same SVG polyline preview the Polygonal Lasso draws),
and sends the trail on release, with Shift/Alt at release choosing the
mode as for the marquees. Anti-alias and Feather remain documented
scope cuts.

**Verified two ways.** Five new `document.rs` tests reading the
selection back pixel by pixel, the circle case confirmed by the Python
`f32` model of the ray cast against the ellipse's own containment
test. A trail with every vertex repeated selects the same six pixels
as the plain triangle. A 64-gon inscribed in the canvas-spanning
circle on `4×4` — whose apothem `2 cos(π/64) = 1.998` clears every
pixel centre the ellipse admits (the nearest edge centres sit `1.58`
from the middle) and excludes the corners (`2.12` away) — selects
exactly the twelve pixels the ellipse marquee selects. A lasso in Add
mode unions with a one-pixel selection. Two distinct points error
mentioning "enclose", four collinear points error with "nothing
selected", and the `Rectangle` survives both. The three corners of the
triangle in the opposite winding, trail left open, still enclose it.
All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty-two:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The drag capture and release wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1120 Rust tests total** (1115 → 1120, 1113 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 176 — Clone Stamp tool

`Stroke::Clone { offset }` paints, at each pixel the brush covers, the
pixel `offset` away in the layer as it stood before the stroke —
composited source-over by the brush's coverage times the sample's own
alpha, exactly as the Pattern Stamp composites its tile, so a
transparent sample paints nothing and a sample off the canvas is
skipped. It reads the same pre-stroke snapshot the Blur, Sharpen, and
(now) Clone strokes share, which is what keeps a stroke from cloning
pixels it has itself just written: cloning from the left column into
the middle and right columns in one stroke gives the right column the
middle's *original* values. The offset is the Alt-clicked sampling
source minus the stroke's first point, and the frontend keeps it
across strokes until the source is reset — Photoshop's Aligned mode,
its default; the non-aligned mode and Sample All Layers are documented
scope cuts. A new **Clone Stamp** tool button sits beside the Pattern
Stamp; the tool options show the source point or ask for one, and the
colour swatch is disabled for it.

**Verified two ways.** Five new `document.rs` tests, the one blended
byte cross-checked in Python emulating the source-over arithmetic. A
full-coverage dot on `(0, 0)` of `ramped_3x3` with the source one pixel
to the right copies `(1, 0)`'s `20` over it and touches nothing else.
Covering everything with the same offset makes each pixel its
right-hand neighbour — `[[20, 30, 30], [50, 60, 60], [80, 90, 90]]` —
with the right column, whose sources lie off the canvas, untouched. The
`0.7929` edge coverage composites `20` over `10` to `17.9 → 18`.
Cloning from the left with everything covered gives `[[10, 10, 20],
[40, 40, 50], [70, 70, 80]]` — the right column taking the middle's
original `20, 50, 80`, not the `10`s just written. A one-pixel
selection confines the stroke, a fully transparent sample on
`depth_ramped_3x3` leaves its target byte-identical, and a locked
layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty-three:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The Alt-click source and stroke wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1125 Rust tests total** (1120 → 1125, 1118 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 177 — History Brush tool

`Stroke::History { source }` paints, at each pixel the brush covers,
the pixel at the same position in `source` — a document-sized RGBA8
buffer holding an earlier state of the layer — composited source-over
by coverage times the sample's alpha, the Clone Stamp with no offset
and a different buffer. `Stroke` now carries a lifetime for that
borrowed buffer; every existing variant is unchanged. A source of the
wrong length errors before anything is painted. The app side supplies
the buffer: a new `history_source` slot on `AppState` holds a whole-
document clone taken when the user presses **Set Source** — kept
beside the clipboard rather than in the undo stack, since it must
outlive undo and redo — and the `history_stroke` command paints from
that clone's layer of the same id (erroring if no source is set or the
source has no such layer). The `HistoryState`/`Snapshot` gain
`hasHistorySource`, mirrored in `types.ts`, so the tool options can say
whether a source exists. Photoshop lets the History panel pick any
history state or snapshot as the source; here the source is the one
state remembered last — a documented scope cut. A new **History Brush**
tool button sits beside the Clone Stamp with Set Source in its tool
options; the colour swatch is disabled for it.

**Verified two ways.** Five new `document.rs` tests, the one blended
byte cross-checked in Python emulating the source-over arithmetic.
With `ramped_3x3` remembered and then inverted (`50 → 205`), a
full-coverage dot on `(0, 0)` brings back its `10` and leaves `(1, 1)`
inverted; a stroke covering everything restores the layer byte for
byte. The `0.7929` edge coverage composites the source's `10` over a
solid `100` to `28.6 → 29`. A transparent source pixel
(`depth_ramped_3x3`'s left column) paints nothing while an opaque one
paints, and a one-pixel selection confines the restore (`(0, 0)` back
to `10`, `(1, 0)` still `235`). A four-byte source errors mentioning
"size", and a locked layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty-four:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The Set Source and stroke wiring was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1130 Rust tests total** (1125 → 1130, 1123 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 178 — Smudge tool

`Stroke::Smudge { strength }` drags colour along the stroke: each
pixel the brush covers moves toward the pre-stroke pixel one segment
step *behind* it — the pixel at `p − (b − a)`, rounded, for the
stroke's last segment `a → b` — by `strength` percent scaled by the
brush's coverage, all four channels, so the colour under the brush's
trailing edge is carried forward. This is the project's own explicit
definition of a tool Photoshop documents only by feel: a directed
Clone with a per-segment offset, mixed by strength rather than
composited. A single point has no direction and smudges nothing; a
pixel whose source lies off the canvas is left alone; and the source
is the same pre-stroke snapshot Blur, Sharpen, and Clone read, so a
drag across a row shifts it by one step rather than compounding
through pixels the stroke already moved. Photoshop's Finger Painting
and Sample All Layers are documented scope cuts. A new **Smudge** tool
button sits beside Sharpen; the Flow slider is its Strength.

**Verified two ways.** Five new `document.rs` tests, the two blended
bytes cross-checked in Python emulating the `f32` `lerp`. A one-pixel
step right along `ramped_3x3`'s middle row at full strength gives `40
40 60`: `(1, 1)` takes `(0, 1)`'s `40` and `(0, 1)`, whose source lies
off the canvas, keeps its own; the top row is untouched. Strength 50
pulls `50` half way to `45`, and strength 0 is an identity. A diagonal
step pulls `(1, 1)` from `(0, 0)`'s `10`, and with radius 1 the
horizontal step's edge covers `(1, 0)` at `0.5`, pulling its `20` to
`15`. A directionless dot changes nothing, and a two-segment drag
across the whole row at full coverage shifts it by exactly one step
(`40 40 50`), proving the snapshot read. A selection confines the
smudge and a locked layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty-five:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new tool's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1135 Rust tests total** (1130 → 1135, 1128 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 179 — Color Replacement tool

`Stroke::ColorReplace { color, tolerance }` is the Color Replacement
tool in its default Color mode with Sampling: Once. Before painting,
the stroke samples the pixel under its first point (clamped to the
canvas); then each pixel the brush covers whose RGB is within
`tolerance` of that sample, per channel, takes the brush colour's hue
and saturation at its own lightness — through the same `rgb_to_hsl` /
`hsl_to_rgb` pair the Sponge uses — mixed in by the brush's coverage.
Alpha is untouched and fully transparent pixels are skipped. Greys,
having no hue, take the brush's hue at their lightness, as Photoshop's
tool colours them. Continuous and Background Swatch sampling, the Hue,
Saturation, and Luminosity modes, the Limits options, and Anti-alias
are documented scope cuts. A new **Color Replacement** tool button
sits beside Smudge; it uses the brush colour swatch and shows a
Tolerance slider (shared with the Magic Wand's) in the tool options.

**Verified two ways.** Five new `document.rs` tests, every byte
cross-checked in Python emulating the `f32` HSL round trip and `lerp`.
A solid `(100, 200, 100)` — HSL `(120°, 0.476, 0.588)` — painted with
pure red becomes `(255, 45, 45)` everywhere covered (red's hue and
saturation at that lightness), and with pure blue `(45, 45, 255)`. With
a blue `(50, 50, 200)` pixel in the corner, a stroke starting on green
at tolerance 32 recolours the greens and leaves the blue alone;
tolerance 255 then recolours the blue too, to `(250, 0, 0)` at its own
lightness. The `0.7929` edge coverage mixes `(100, 200, 100)` toward
`(255, 45, 45)` to `(223, 77, 56)`, and a grey `150` takes the red hue
at its lightness, `(255, 45, 45)`. A transparent pixel is untouched and
an alpha-128 pixel keeps its alpha while recolouring. A one-pixel
selection confines the stroke and a locked layer errors. Four of the
five passed on the first run: the tolerance-255 case had placed its
radius-3 dot at the corner, whose far corner it covers only at `0.67`,
so the blue came back part-way mixed (`184, 16, 66`) — the dot was
moved to the centre, where it covers every pixel fully; the code was
right. (The compiler also caught a borrow of `self.width` while the
layer was mutably borrowed in the first-point sampling, fixed by
reading the already-captured locals.)

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and twenty-six:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new tool's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1140 Rust tests total** (1135 → 1140, 1133 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 180 — Background Eraser tool

`Stroke::BackgroundErase { tolerance }` is the Background Eraser with
Sampling: Once. It reuses the first-point sample Phase 179 added to
`stroke` — the pixel under the stroke's start — and applies the
Eraser's multiply-toward-zero on alpha (`α · (1 − coverage)`) only to
covered pixels whose RGB is within `tolerance` of that sample, per
channel, so a background colour scrubs away around a differently
coloured subject while the subject's pixels are untouched. Colour bytes
are never changed, and repeated strokes compound exactly as the Eraser
does. Continuous and Background Swatch sampling, the Limits options
(Discontiguous, Contiguous, Find Edges), and Protect Foreground Color
are documented scope cuts. A new **Background Eraser** tool button sits
beside the Magic Eraser with the shared Tolerance slider in its tool
options; the colour swatch is disabled for it.

**Verified two ways.** Five new `document.rs` tests on a new
`subject_on_green` fixture — a green `(100, 200, 100)` background with a
blue `(50, 50, 200)` subject pixel at `(2, 2)` — the two alpha values
cross-checked in Python emulating the `f32` arithmetic. A stroke
starting on green with tolerance 32 covering everything erases every
green to alpha `0`, colour bytes intact, and leaves the blue opaque;
the same stroke started on the blue erases the blue and spares the
green, proving the first-point sample. The `0.7929` edge coverage
leaves `255 × (1 − 0.7929) = 52.8 → 53`, and tolerance 255 takes the
subject too. A second identical edge stroke compounds `53` to `11.0 →
11` with the colour untouched. A one-pixel selection confines the
erase and a locked layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
twenty-seven: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1145 Rust tests total** (1140 → 1145, 1138 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 181 — Healing Brush tool

`Stroke::Heal { offset }` is the Clone Stamp's sampling — the
Alt-clicked source minus the stroke's first point, aligned across
strokes — with the classic heal in place of a plain copy: texture from
the source, tone from the destination. Each covered pixel takes the
sampled pixel shifted, per channel, by the difference between the
destination's and the source's local means (radius-1 box blurs of the
pre-stroke layer through `box_blur_at`), clamped to `0..=255`, then
mixed in by the brush's coverage. Sampling from a brighter region into
a darker one thus carries the source's grain across at the
destination's brightness, which is the point of the tool. Alpha is
untouched; a transparent or off-canvas sample paints nothing. Photoshop's
Diffusion slider, Sample All Layers, and pattern sources are documented
scope cuts. A new **Healing Brush** tool button sits beside the Clone
Stamp and shares its Alt-click source and options readout.

**Verified two ways.** Five new `document.rs` tests, the whole healed
grid derived in Python from `ramped_3x3` and its known radius-1 box-blur
grid `[[23, 30, 36], [43, 50, 56], [63, 70, 76]]`, and the one
coverage-mixed byte emulated in `f32`. Healing `(1, 1)` from `(2, 1)`
gives `60 + (50 − 56) = 54`, and `(0, 0)` from `(1, 0)` gives `20 + (23
− 30) = 13`. Covering everything with the source one pixel right gives
`[[13, 24, 30], [43, 54, 60], [73, 84, 90]]` (the right column's sources
lie off the canvas) and one pixel left gives `[[10, 17, 26], [40, 47,
56], [70, 77, 86]]`. The `0.7929` edge coverage mixes `10` toward `13`
to `12`. On `depth_ramped_3x3` a transparent sample paints nothing and
an opaque one keeps the destination's alpha `128` while changing its
colour. A one-pixel selection confines the stroke and a locked layer
errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
twenty-eight: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1150 Rust tests total** (1145 → 1150, 1143 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 182 — Spot Healing Brush tool

`Stroke::SpotHeal` is the Spot Healing Brush in its Proximity Match
type: no source point — each pixel the brush covers takes the mean of
the pre-stroke pixels on the square ring two pixels out from it
(Chebyshev distance exactly 2: sixteen samples, edge-clamped exactly as
`box_blur_at` clamps, averaged through the same `average_samples` with
truncating division), the surrounding and presumably unblemished
pixels, mixed in by the brush's coverage. Alpha is untouched and fully
transparent pixels are skipped. Reading the ring from the pre-stroke
snapshot means a drag over a blemish replaces every pixel with its
*original* surroundings, never with pixels the stroke has already
rewritten. Photoshop's Content-Aware and Create Texture types and
Sample All Layers are documented scope cuts. A new **Spot Healing**
tool button sits beside the Healing Brush; it needs no source.

**Verified two ways.** Five new `document.rs` tests, the ring-mean
grid derived in Python with the same clamp-and-truncate rule and the
one coverage-mixed byte emulated in `f32`. A `200` spot in the middle
of solid `100` heals to `100`. Covering `ramped_3x3` entirely gives
`[[40, 42, 45], [47, 50, 52], [55, 57, 60]]` — the centre's ring is the
whole border, mean `50`. The `0.7929` edge coverage mixes `10` toward
its ring mean `40` to `34`, and a corner-to-corner drag reproduces the
one-dot row `47 50 52`, proving the snapshot read. On
`depth_ramped_3x3` the transparent left column is untouched and the
centre keeps its alpha `128` (and, by symmetry, its `50`). A one-pixel
selection confines the stroke and a locked layer errors. All five
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
twenty-nine: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1155 Rust tests total** (1150 → 1155, 1148 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 183 — Patch tool

`patch(id, dx, dy)` is the Patch tool in its Normal, Source mode: the
active selection is the area to repair, and the drag `(dx, dy)` says
where to sample. Every selected pixel `p` is rebuilt from the pre-patch
pixel `p + (dx, dy)` with the Healing Brush's heal — the sample shifted,
per channel, by the difference between the destination's and the
source's radius-1 local means, clamped — so the source's texture
arrives at the destination's tone. Unlike the brush it replaces
outright, with no coverage falloff, and it leaves the selection outline
where it is, as Photoshop's Patch leaves the repaired area selected.
Alpha is untouched; a sample off the canvas or fully transparent leaves
its pixel alone; a zero drag does nothing and returns `None`; the
selection's bounding box is reported dirty. Nothing selected, or a
locked or unknown layer, errors. Photoshop's Destination mode, the
Transparent option, Diffusion, and Content-Aware patching are documented
scope cuts. A new **Patch** tool button reuses the Move tool's drag
capture: with a selection made, drag from it onto the source area and
the rounded offset is applied at pointer-up.

**Verified two ways.** Five new `document.rs` tests, every expected
byte read off the same Python-derived healed grid Phase 181 used. The
centre pixel patched from one pixel right becomes `60 + (50 − 56) = 54`
(the Healing Brush's own number), the source pixel is untouched, and
the selection still sits at `(1, 1)–(2, 2)`, which is also the dirty
box. The left column patched from the middle becomes `13, 43, 73`. The
right column patched from off the canvas is untouched, and on
`depth_ramped_3x3` a transparent source leaves the centre alone while
an opaque one keeps its alpha `128`. The two left columns patched from
one pixel right give `[[13, 24, 30], [43, 54, 60], [73, 84, 90]]` —
column 0 built from column 1's *original* values, proving the snapshot
read. Nothing selected errors with "Nothing is selected", a zero drag
returns `None`, and an unknown or locked layer errors with the pixels
intact. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and thirty:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The drag wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`,
`npm run build`) is fully green.

**1160 Rust tests total** (1155 → 1160, 1153 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 184 — Content-Aware Move tool

`content_aware_move(id, dx, dy)` is the Content-Aware Move tool in its
Move mode: Phase 173's selected-pixel move, after which every vacated
pixel — one the selection covered that the moved pixels did not land
back on — is filled from its surroundings with the ring mean of the
pre-move layer (the sixteen pixels at Chebyshev distance 2,
edge-clamped, all four channels), which is this project's explicit,
stated content-aware fill; Photoshop's patch-synthesis fill is
proprietary, and this is the honest small version of it. The Spot
Healing Brush's ring sample moved into a shared `ring_mean` helper so
the two tools agree byte for byte. Pixels that were already transparent
stay transparent, the selection travels with the pixels (and is dropped
if it leaves the canvas, as for Move), a zero move returns `None`, and
nothing selected or a locked or unknown layer errors. Extend mode, the
Structure and Color sliders, and Transform on Drop are documented
scope cuts. A new **Content-Aware Move** tool button reuses the Move
and Patch drag capture.

**Verified two ways.** Five new `document.rs` tests, every fill value
read off the Python-derived ring-mean grid Phase 182 established
(`[[40, 42, 45], [47, 50, 52], [55, 57, 60]]` for `ramped_3x3`). A `200`
spot on solid `100` moved one pixel right lands on `(2, 1)`, its old
place is filled with `100`, and the outline follows to `(2, 1)–(3, 2)`.
The left column of `ramped_3x3` moved onto the middle gives `[[40, 10,
30], [47, 40, 60], [55, 70, 90]]` — the middle taking `10 40 70` and the
vacated column its ring means `40, 47, 55`, opaque. Moving the two left
columns right by one fills only column 0 (column 1 is vacated but
immediately re-covered), and a transparent source pixel stays
transparent. The right column moved off the canvas is filled (`45` at
the top) and the selection dropped. Nothing selected and a locked layer
error, and a zero move returns `None`. All five passed on the first
run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-one: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The drag wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1165 Rust tests total** (1160 → 1165, 1158 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 185 — Edit > Content-Aware Fill

`content_aware_fill(id)` replaces every selected pixel of a layer by
the ring mean of the pre-fill layer — the sixteen pixels two out from
it, edge-clamped, all four channels, through the `ring_mean` helper the
Spot Healing Brush and Content-Aware Move share — so a selected blemish
or hole is filled from what surrounds it, transparent holes included.
Reading from a snapshot means filling a wide selection uses only the
original neighbours, never pixels filled a moment earlier. It returns
the selection's bounding box and errors with nothing selected or on a
locked or unknown layer. This one command covers three checklist
entries — Content-Aware Fill, Content-Aware Fill from Selection, and
Delete and Fill Selection — which in Photoshop are three routes to the
same operation. Photoshop's patch synthesis, its sampling-area brush,
Color Adaptation, Rotation Adaptation, Scale, and Mirror are documented
scope cuts. A new **Content-Aware Fill** button sits beside Delete.

**Verified two ways.** Five new `document.rs` tests, every value read
off the Python ring-mean model (the `ramped_3x3` grid `[[40, 42, 45],
[47, 50, 52], [55, 57, 60]]`, and for `depth_ramped_3x3`'s `(0, 1)` the
clamped ring's red `760 / 16 = 47` and alpha `1531 / 16 = 95`). A `200`
spot on solid `100` fills to `100` with the box `(1, 1)–(2, 2)`. Select
All on `ramped_3x3` fills to exactly the ring-mean grid. Filling the
left column of `depth_ramped_3x3` gives `(0, 1)` `[47, 0, 0, 95]` from
the untouched neighbours even though `(0, 0)` was filled first, and
leaves the centre alone. An inverted centre selection fills everything
but the centre (`(0, 0) → 40`, `(2, 2) → 60`, centre still `50`).
Nothing selected errors with "Nothing is selected", and an unknown or
locked layer errors with the pixels intact. All five passed on the
first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and thirty-two:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new button's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1170 Rust tests total** (1165 → 1170, 1163 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 186 — Remove tool

`Stroke::Remove` is the Spot Healing Brush with one change that makes
it behave like a removal rather than a touch-up: each covered pixel
takes the ring mean of the pre-stroke layer computed only over ring
samples the stroke itself does *not* cover — `stroke` already has the
whole stroke's coverage map before it paints, so the arm can ask, for
each of the sixteen ring samples, whether it lies under the brush. An
object brushed over in one stroke is therefore filled from outside the
brushed area instead of from its own remaining pixels, which is what a
removal should do; when every sample is covered (a stroke over
everything) the plain ring mean is used. Photoshop's Remove tool is a
neural model; this is its explicit proximity stand-in, stated as such.
Alpha is untouched and transparent pixels are skipped. A new **Remove**
tool button sits beside Spot Healing.

**Verified two ways.** Five new `document.rs` tests, every value
derived in Python from the same clamp-and-truncate ring rule, with a
side-by-side against the Spot Healing Brush that shows the difference
the exclusion makes. On a new `striped_3x3` (solid `100` with a `200`
stripe across the middle row) a radius-`0.5` stroke along the stripe
covers exactly the middle row, and Remove turns every stripe pixel
`100` — the surrounding value — while the Spot Healing Brush, whose
rings also average the stripe's own pixels, gives `112`. A stroke over
all of `ramped_3x3` falls back to the plain ring-mean grid `[[40, 42,
45], [47, 50, 52], [55, 57, 60]]`. A radius-1 dot at `(1, 1)` covers
four pixels; `(0, 0)`'s nine uncovered ring samples average `58`, and
its `0.7929` coverage mixes `10` toward `58` to `48`. On
`depth_ramped_3x3` the transparent column is untouched and the centre
keeps alpha `128`. A one-column selection confines the removal to its
first stripe pixel, and a locked layer errors. Four of the five passed
on the first run: the selection test expected that pixel to become
`100`, but only *selected* pixels carry coverage, so the unselected
stripe pixel at `(2, 1)` — which the clamped ring reaches from column
0 — is not excluded, and the fifteen remaining samples average
`(14 × 100 + 200) / 15 = 106`. The Python model, given the same
covered set, agrees, and the test now expects `106 200 200`.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-three: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1175 Rust tests total** (1170 → 1175, 1168 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 187 — Rectangle tool

`Document::draw_rectangle(id, x0, y0, x1, y1, radius, fill, stroke)` is
the Rectangle tool in its Pixels mode. The drag's two corners are
normalised and clipped to the canvas exactly as the Rectangular
Marquee's are (the same `normalize_selection_bounds`), and every pixel
whose centre lies inside the box — the same `+0.5` pixel-centre rule
the selection shapes use, through `shape_contains` with
`SelectionShape::RoundedRectangle` when `radius` is non-zero — is
overwritten. The optional `fill` is a flat colour; the optional
`stroke` is a `(colour, width)` band hugging the *inside* of the edge,
built the way Select > Modify > Border is built (the shape minus the
same shape shrunk by `width` on every side), so a width that swallows
the whole box strokes the whole shape. The stroke wins where the two
overlap, and the active selection confines the paint. Photoshop's
Shape and Path modes (a live vector layer), its Anti-alias option, and
its Center and Outside stroke alignments are documented scope cuts;
this app's layers are pixels only, so edges are hard. Neither fill
nor stroke, a stroke width outside `1..=250`, a non-finite corner, and
a locked or unknown layer all error; a box that rounds to no pixels
paints nothing and returns `None`, like a click with no drag.

A new **Rectangle** tool button sits after History Brush. Its options
are a **Fill** checkbox (the brush colour), a **Stroke** width slider
(`0` for none) with its own colour swatch, and a **Radius** slider; the
drag shares the marquee's live outline preview and paints at
pointer-up through a `draw_rectangle` command.

**Verified two ways.** Five new `document.rs` tests, each grid drawn
first by an independent Python model of the pixel-centre rule and
read back as one character per pixel (`.` untouched, `F` fill, `S`
stroke). A `(2, 2)`→`(0, 0)` drag on a blank `3×3` fills the top-left
`2×2` and reports that box dirty. On a blank `5×5`, a full-box fill
with a 1-pixel stroke gives `SSSSS / SFFFS / SFFFS / SFFFS / SSSSS`, a
3-pixel stroke alone (which shrinks the box to nothing) strokes all
twenty-five pixels, and a 1-pixel stroke alone leaves the `3×3`
interior untouched. Radius `2` clears exactly the four corner pixels
(`(0, 0)`'s centre is `1.5` from the corner circle's centre on both
axes, `4.5 > 4`; `(1, 0)`'s is `0.25 + 2.25 = 2.5 ≤ 4`), and with a
1-pixel stroke the band's inner shape clamps its radius to `1.5`, so
`(1, 1)` at `1 + 1 = 2 ≤ 2.25` is fill: `.SSS. / SFFFS / SFFFS / SFFFS
/ .SSS.`. A `(-1, -1)`→`(2, 2)` drag with only column 0 selected paints
`(0, 0)` and `(0, 1)` and reports the clipped `(0, 0)–(2, 2)` box.
No fill and no stroke, a zero-width stroke, a `NaN` corner, an unknown
layer, and a locked layer all error with the pixels untouched, and a
zero-width box returns `None`. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-four: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1180 Rust tests total** (1175 → 1180, 1173 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 188 — Ellipse tool

`Document::draw_ellipse(id, x0, y0, x1, y1, fill, stroke)` is the
Ellipse tool in its Pixels mode: the ellipse inscribed in the dragged
box — a circle when the box is square — painted with the Rectangle
tool's optional flat `fill` and inside `(colour, width)` `stroke`.
Phase 187's painter was lifted into a private `draw_shape(id, shape,
…)` that takes any `SelectionShape`; `draw_rectangle` now passes
`Rectangle` or `RoundedRectangle { radius }` and `draw_ellipse` passes
`Ellipse`, so both tools share the marquee's box normalisation, the
pixel-centre rule (`shape_contains`), the Border-style stroke band
(the shape minus the same shape in the box shrunk by `width` on every
side), the selection confinement, and the same errors. Shape and Path
modes, anti-aliasing, and Center/Outside stroke alignment remain
documented scope cuts.

A new **Ellipse** tool button sits after Rectangle; the two share the
Fill, Stroke, and stroke-colour options (the Radius slider is the
rectangle's alone), the marquee's live outline preview — drawn as an
ellipse for this tool — and pointer-up painting through a
`draw_ellipse` command.

**Verified two ways.** Five new `document.rs` tests, each grid drawn
first by an independent Python model of `(px − cx)²/rx² + (py − cy)²/ry²
≤ 1` at pixel centres. A reversed drag over a blank `7×5` fills
`.FFFFF. / FFFFFFF / FFFFFFF / FFFFFFF / .FFFFF.` and reports the whole
box dirty — `(1, 0)`'s centre scores `0.327 + 0.64 = 0.967`, inside,
while `(0, 0)`'s scores `0.735 + 0.64`, outside. On a blank `5×5` a
1-pixel stroke around a fill gives `.SSS. / SFFFS / SFFFS / SFFFS /
.SSS.`, and a 2-pixel stroke alone leaves only the centre pixel — the
sole pixel inside the `1×1` inner box — untouched. A `3×5` box is a
thin ellipse (`rx 1.5, ry 2.5`) whose top and bottom rows keep only
their centre pixel: `..F.. / .FFF. / .FFF. / .FFF. / ..F..`. A drag
off every edge with the two left columns selected paints `.F / FF / FF
/ FF / .F` and reports the clipped `5×5` box. No fill and no stroke, a
251-pixel stroke, an infinite corner, an unknown layer, and a locked
layer all error with the pixels untouched, and a zero-width box
returns `None`. Four of the five passed on the first run: the thin
ellipse test was first written against a `2×5` box on the guess that
its tips would miss the top and bottom rows, but `(1, 0)`'s centre
scores `0.25 + 0.64 = 0.89` there, inside, so a `2×5` ellipse paints
all ten pixels of its box — indistinguishable from a rectangle. The
test was moved to the `3×5` box above, whose grid the Python model
had actually been run on, and passed.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-five: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1185 Rust tests total** (1180 → 1185, 1178 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 189 — Line tool

`Document::draw_line(id, x0, y0, x1, y1, weight, color)` is the Line
tool in its Pixels mode: a straight line `weight` pixels wide from one
drag end to the other, painted in a flat colour. A pixel is painted
when its centre lies inside the line's rectangle — its perpendicular
distance to the segment is at most `weight / 2` *and* its projection
along the segment falls between the two ends. The second condition is
what gives the line Photoshop's butt caps: a pixel centre within half
the weight of an endpoint but past it is left alone, where the brush
tools' round capsule would have painted it. It is the same hard
pixel-centre rule the Rectangle and Ellipse tools use, so Photoshop's
Anti-alias option is a documented scope cut, as are its arrowheads and
its Shape and Path modes. Pixels are overwritten outright and the
active selection confines the paint. The dirty box is the segment's
bounding box grown by `weight / 2` on every side and clipped to the
canvas; a zero-length line or one entirely off the canvas paints
nothing and returns `None`, and a weight outside `1..=250`, a
non-finite end, or a locked or unknown layer errors.

A new **Line** tool button sits after Ellipse with a **Weight**
slider; the drag shows a live one-pixel preview line (the Lasso's SVG
overlay with a `<line>` in it) and paints at pointer-up through a
`draw_line` command in the brush colour.

**Verified two ways.** Five new `document.rs` tests, each grid drawn
first by an independent Python model of the projection-and-distance
rule. On a blank `5×5`, a weight-1 line along `y = 2.5` paints the
middle row and a weight-3 line paints the middle three. Along the
`(0, 0)`→`(5, 5)` diagonal, off-diagonal centres sit `1/√2 ≈ 0.707`
from the line — outside weight 1's half-width of `0.5`, inside weight
2's `1.0` — so weight 1 paints the five diagonal pixels and weight 2
adds both neighbouring diagonals; a `(0.5, 0.5)`→`(4.5, 2.5)` slant
paints `FF... / .FFF. / ...FF`, with `(1, 1)`'s centre `0.447` from the
line. A weight-1 line from `x = 1` to `x = 4` paints only `(1, 2)`,
`(2, 2)`, `(3, 2)` — `(0, 2)` and `(4, 2)` are `0.5` from an endpoint
but project past it — and a reversed vertical drag paints the same
three pixels as the forward one. A line from `x = −3` to `x = 9` with
the two left columns selected paints only those two pixels of the
middle row and reports the clipped `(0, 2)–(5, 3)` box; a line at
`y = 9` returns `None`. Weight `0` and `251`, a `NaN` end, an unknown
layer, and a locked layer all error with the pixels untouched, and a
zero-length line returns `None`. Four of the five passed on the first
run: every painted grid matched, but the end-cap test expected the
dirty box of the `x = 1`→`4` line to be `(1, 2)–(4, 3)`, the painted
run, whereas the box grows by the half-weight along the line as well as
across it — `0.5` floors to `0`, `4.5` ceils to `5` — so it is
`(0, 2)–(5, 3)`, one pixel wider than the run at each end, exactly as
the doc comment states. The test now expects that.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-six: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1190 Rust tests total** (1185 → 1190, 1183 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 190 — Polygon tool

`Document::draw_polygon(id, cx, cy, x, y, sides, color)` is the
Polygon tool in its Pixels mode. As in Photoshop the drag starts at
the polygon's centre and ends at its first vertex, so the drag's
length is the circumradius and its angle the rotation; the remaining
`sides − 1` vertices are spaced evenly around that circle. A pixel is
painted when its centre is inside the polygon by the even-odd rule —
`point_in_polygon`, the Polygonal Lasso's own test — which is the same
hard pixel-centre rule the other shape tools use, so Photoshop's
Anti-alias option is a documented scope cut, as are its star ratio and
smooth corners (the Star tool's territory), its stroke, and its Shape
and Path modes. Pixels are overwritten outright and the active
selection confines the paint. The dirty box is the vertices' bounding
box clipped to the canvas; a zero-length drag or a polygon entirely off
the canvas paints nothing and returns `None`, and `sides` outside
`3..=100`, a non-finite coordinate, or a locked or unknown layer
errors.

A new **Polygon** tool button sits after Line with a **Sides** slider
(3–12); the drag shows a live outline built by the same construction
in `polygonPoints` and paints at pointer-up through a `draw_polygon`
command in the brush colour.

**Verified two ways.** Five new `document.rs` tests, each grid drawn
first by an independent Python port of the even-odd test, with a check
that no pixel centre sits on a polygon edge (the closest is `0.082`
pixels away, for the triangle). Four sides dragged straight up `2.2`
pixels from `(2.5, 2.5)` is the diamond `|dx| + |dy| ≤ 2.2` — thirteen
pixels, `..F.. / .FFF. / FFFFF / .FFF. / ..F..` — with the `5×5` box
dirty. Three sides dragged up from `(2.5, 2.9)` put the apex at
`(2.5, 0.2)` and the base corners at `(±2.338 from centre, 4.25)`, so
the bottom row's centres at `y = 4.5` lie below the base: `..F.. /
..F.. / .FFF. / .FFF. / .....`. Six sides dragged right `2.8` pixels
on a `7×7` give the hexagon `..FFF.. / .FFFFF. / .FFFFF. / .FFFFF. /
..FFF..` inside a `(0, 1)–(7, 6)` box. The diamond with the two left
columns selected paints `.F / FF / .F`, and a diamond centred at
`(9, 9)` returns `None`. Two and 101 sides, a `NaN` centre, an unknown
layer, and a locked layer all error with the pixels untouched, and a
zero-length drag returns `None`. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-seven: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1195 Rust tests total** (1190 → 1195, 1188 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 191 — Star tool

`Document::draw_star(id, cx, cy, x, y, points, ratio, color)` is the
Star tool in its Pixels mode: the Polygon tool's construction with
`points` outer vertices on the drag's circle and, midway between each
pair, an inner vertex at `ratio` percent of that radius — Photoshop's
own Star Ratio, where `100` is the plain polygon and smaller values
cut deeper notches. The drag runs from the centre to the first outer
point, and the fill, selection confinement, dirty box, `None` for a
zero-length or off-canvas drag, and scope cuts (smooth indents,
stroke, anti-aliasing, Shape and Path modes) are all the Polygon
tool's, through a new private `paint_polygon` that both tools now
share. `points` outside `3..=100` and a `ratio` outside `1..=100`
error, as do non-finite coordinates and a locked or unknown layer.

A new **Star** tool button sits after Polygon; it shares the Polygon
tool's **Points** slider and adds a **Ratio** slider (1–100%), and its
live outline is the same `polygonPoints` preview with the inner
vertices added. It paints at pointer-up through a `draw_star` command
in the brush colour.

**Verified two ways.** Five new `document.rs` tests, each grid drawn
first by an independent Python port of the even-odd test with the same
edge-margin check as the Polygon tool (closest centre `0.040` pixels
from an edge, for the three-pointed star). The Polygon tool's diamond
at a `50%` ratio puts the inner vertices `1.1` pixels out on the
diagonals, so the diagonal pixels' centres — `√2 ≈ 1.414` out — fall in
the notches and the thirteen-pixel diamond becomes a nine-pixel plus,
`..F.. / ..F.. / FFFFF / ..F.. / ..F..`, with the `5×5` box dirty. The
same drag at `100%` reproduces the diamond exactly. A three-pointed
star dragged up `3.3` pixels from `(3.5, 3.9)` at `30%` on a `7×7` is a
one-pixel spike over a three-pixel base — `...F... / ...F... / ...F... /
..FFF..` on rows 1–4 — because its lower arms, tipped at `y = 5.55`,
only reach row 4's centres; its box is `(0, 0)–(7, 6)`. The plus with
the two left columns selected keeps only `(0, 2)` and `(1, 2)`, and a
star centred at `(9, 9)` returns `None`. Two and 101 points, ratios
`0` and `101`, an infinite coordinate, an unknown layer, and a locked
layer all error with the pixels untouched, and a zero-length drag
returns `None`. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-eight: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1200 Rust tests total** (1195 → 1200, 1193 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 192 — Triangle tool

`Document::draw_triangle(id, x0, y0, x1, y1, color)` is the Triangle
tool in its Pixels mode: the isosceles triangle fitted to the dragged
box — apex at the top centre, base along the bottom edge — whichever
way the drag went. Photoshop keeps the apex at the box's top as well
(pointing it elsewhere is a transform), and its rounded-corner option,
stroke, anti-aliasing, and Shape and Path modes are documented scope
cuts. The three vertices go through the same private `paint_polygon`
the Polygon and Star tools use, so the even-odd pixel-centre fill,
selection confinement, and clipped dirty box are shared. A box with no
width or no height paints nothing and returns `None`, as does one
entirely off the canvas; non-finite corners and a locked or unknown
layer error.

A new **Triangle** tool button sits after Star; it uses the marquee's
box preview and paints at pointer-up through a `draw_triangle` command
in the brush colour.

**Verified two ways.** Five new `document.rs` tests, each grid drawn
first by an independent Python port of the even-odd test with the
edge-margin check (closest centre `0.064` pixels from an edge, on the
wide box). A `(5, 5)`→`(0, 0)` drag on a blank `5×5` fills `..F.. /
..F.. / .FFF. / .FFF. / FFFFF`: the half-width at a row's centre `y` is
`y / 2`, so rows 0–1 hold one centre, rows 2–3 three, and row 4 all
five. A `5×3` box from `y = 1` to `4` gives `..F.. / .FFF. / FFFFF` on
rows 1–3, row 3's outer centres at `0.5` and `4.5` clearing the edge at
`0.417`. A `3×3` box at `(1, 1)` gives a one-pixel apex over a
three-pixel base. A box hanging off the top-left corner — apex
`(0.5, −2)`, base from `(−2, 3)` to `(3, 3)` — paints `FF / FF / FFF`
and reports the clipped `(0, 0)–(3, 3)` box; the full-box triangle with
the two left columns selected keeps `.F / .F / FF` on rows 2–4; and a
box at `(7, 7)–(9, 9)` returns `None`. A `NaN` corner, an unknown
layer, and a locked layer error with the pixels untouched, and a box
with no width or no height returns `None`. All five passed on the
first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
thirty-nine: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1205 Rust tests total** (1200 → 1205, 1198 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 193 — Levels channel selection

`Document::levels_on(id, channel, input_black, input_white, gamma,
output_black, output_white)` lifts the scope cut Levels shipped with:
Photoshop's Channel dropdown. `LevelsChannel::Rgb` remaps all three
channels exactly as `levels` always has (which now simply delegates),
while `Red`, `Green`, or `Blue` puts only that one channel through the
same normalise-gamma-remap chain and leaves the other two, and alpha,
untouched — the way a per-channel Levels move tints an image rather
than re-toning it. The `levels` command takes an optional `channel`
(defaulting to RGB) and the Levels dialog gains a **Channel** select
above its sliders.

This phase also reconciles eight checklist entries. Five detail rows
under Levels — Input Black Point, Midtone/Gamma, White Point, Output
Black, Output White — are the very parameters `levels` has taken since
it shipped, and the two `Select > Grow` / `Select > Similar` rows
duplicate the GROW and SIMILAR entries shipped in Phases 151 and 152;
all are now checked "for consistency" in the same wording the earlier
Border and Smooth duplicates use, so the shipped count moves by eight
rather than one.

**Verified two ways.** Five new `document.rs` tests on a `[100, 150,
200]` pixel with input `50–200`, every expected byte first computed in
Python emulating `f32` arithmetic: the three channels normalise to
`0.3333`, `0.6667`, and `1.0`, so Red alone gives `[85, 150, 200]`,
Green alone `[100, 170, 200]`, Blue alone `[100, 150, 255]`, and RGB
`[85, 170, 255]` — byte-identical to the old `levels`. Gamma `2.00` on
Red gives `0.3333^0.5 = 0.5774 → 147`, and an output black of `64`
gives `64 + 0.3333 × 191 = 127.67 → 128`. With only the first of two
pixels selected a Blue move leaves the second at `[100, 150, 200, 128]`
until the selection is dropped, and alpha `128` survives; an unknown or
locked layer errors with the pixel intact. All five passed on the
first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and forty:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new dropdown's wiring was reviewed by
hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1210 Rust tests total** (1205 → 1210, 1203 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 194 — Levels Auto and Auto Options

`Document::auto_tone_clipped(id, shadow_clip, highlight_clip)` and
`auto_contrast_clipped` lift the clipping scope cut Auto Tone and Auto
Contrast shipped with, and put the Levels dialog's **Auto** button on
top of them. Photoshop's Auto Options Clip fields ignore the darkest
and lightest fractions of the histogram before measuring a channel's
range — `0.10%` each by default, each allowed up to `9.99%` — so a
handful of stray extreme pixels no longer pins the stretch. The shared
`auto_stretch` now builds a 256-bin histogram per channel over the
sampled pixels and reads each channel's low as the value of its
`⌊n × clip / 10000⌋`-th darkest pixel and its high as the same-ranked
lightest one, so a clip of `0` is exactly the old true minimum and
maximum and `auto_tone`/`auto_contrast` are unchanged (they delegate
with zero clips); a channel whose clipped high is not above its
clipped low is left alone. Clips are hundredths of a percent, `0..=999`,
and anything above errors. The `auto_tone` and `auto_contrast`
commands take optional clips; the Levels dialog gains **Clip shadows
%** and **Clip highlights %** fields (default `0.10`) and an **Auto**
button that runs Auto Tone with them, closing the dialog. Photoshop's
other Auto Options algorithms (Enhance Monochromatic Contrast is Auto
Contrast, Find Dark & Light Colors and Snap Neutral Midtones are Auto
Color, deferred) and its target-colour swatches are documented scope
cuts.

**Verified two ways.** Five new `document.rs` tests on a forty-pixel
ramp `0, 5, …, 195`, every byte first computed in Python emulating
`f32`. At `5%` per end two pixels are skipped, so the range is
`10..185`: the row starts `0, 0, 0, 7, 15`, has `131` at value `100`,
and ends `240, 248, 255, 255, 255`; at `9.99%` three are skipped
(`15..180`), starting `0, 0, 0, 0, 8` and ending `247, 255, 255, 255,
255`. Zero clips give `0, 7, 13, 20, 26 … 229, 235, 242, 248, 255`,
byte-identical to `auto_tone`. Shadows alone (`10..195`) map `100` to
`124`; highlights alone (`0..180`) map it to `142`. Auto Contrast with
a flat-`100` green channel beside the red ramp still moves green to
`131`, since the shared range is the clipped `10..185`. A clip of
`1000` errors, as do an unknown and a locked layer, with the pixels
intact. One of the five passed on the first run: the other four were
first written on a ten-pixel ramp with `10%` and `25%` clips — values
the method itself rejects, since Photoshop's Clip fields stop at
`9.99%` — and every one failed on that error before any expected byte
was compared. They were rewritten on the forty-pixel ramp with `5%`
and `9.99%` clips, whose ranks the Python model had been run on, and
passed; the ceiling test was tightened to `999`/`1000` at the same
time.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and forty-one:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new button and fields were reviewed by
hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1215 Rust tests total** (1210 → 1215, 1208 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 195 — Curves Point mode

`Document::curves_points(id, points)` lifts the "five fixed input
positions" scope cut Curves shipped with: Photoshop's Point mode. The
curve is any number (at least two) of `(input, output)` control points
at arbitrary inputs, sorted by input so a caller may list them in any
order, joined by straight segments — the same linear-interpolation
scope cut as before; the smooth spline remains cut — and flat beyond
the outer points, so a curve whose points stop short of `0` or `255`
clamps the tones outside them. The old `curves` is now this with its
five fixed inputs zipped to the slider values, so it is unchanged.
Fewer than two points, or two points sharing an input, errors.

The Curves dialog gains a **Point mode** checkbox: on, the five sliders
give way to an editable list of Input/Output pairs with **Add Point**
and per-row remove buttons (a list can't shrink below two), applied
through a new `curves_points` command; Reset restores both the sliders
and the two-point identity list.

**Verified two ways.** Five new `document.rs` tests on a four-pixel
fixture, every byte hand-computed with the segment formula and its
round-half-up. Two endpoints `(0, 0)`–`(255, 255)` are the identity.
`(0, 0)`→`(128, 255)`→`(255, 255)` maps `10` to `19.9 → 20`, `64` to
`127.5 → 128`, `60` to `119.5 → 120`, `100` to `199.2 → 199`, and `128`
and `200` to `255`. `(192, 255)` and `(64, 0)` listed backwards sort
themselves: `10` and `30` clamp to `0`, `200` and `255` to `255`, `128`
is the segment's midpoint at `128`, and `100` is `36/128` of the way at
`71.7 → 72`. The five fixed inputs with outputs `0, 100, 128, 192, 255`
are byte-identical to the old `curves` on every pixel, with `10 →
15.6 → 16`. One point, two points at the same input, an unknown layer,
and a locked layer error with the pixels intact. All five passed on
the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and forty-two:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new point list was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1220 Rust tests total** (1215 → 1220, 1213 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 196 — Levels and Curves Black/White Point eyedroppers

`Document::levels_black_point(id, x, y)` and `levels_white_point` are
the Black Point and White Point eyedroppers both the Levels and the
Curves dialogs carry. Clicking a pixel makes it pure black (or white)
by setting *each channel's* input black (or white) point to the
pixel's own value in that channel — the way Photoshop neutralises a
bluish shadow rather than merely darkening it — through a new private
`levels_per_channel(id, input_black, input_white)`, which is `levels`'s
input remap with its own black and white per channel, gamma `1`, full
output range, and the same "white at least one above black" clamp.
The sample is read with `layer_pixel`, so a point off the canvas, an
unknown layer, or a locked layer errors; the adjustment itself still
respects the selection, so the sampled pixel may lie outside it.
Photoshop's configurable target colours (its defaults are pure black
and white) and its Gray Point eyedropper are documented scope cuts —
the latter is next.

Both dialogs gain **Black Pt** and **White Pt** buttons that arm the
eyedropper and close the dialog; the toolbar then reads "Click a pixel
to set the black point" with a Cancel button, and the next canvas
click — whatever tool is active — runs `levels_black_point` or
`levels_white_point` on the selected layer and disarms.

**Verified two ways.** Five new `document.rs` tests, every byte first
computed in Python emulating `f32`. A black point at `[40, 60, 80]`
turns that pixel `[0, 0, 0]` and a `[140, 160, 180]` neighbour into
`[119, 131, 146]` — `100/215`, `100/195`, `100/175` of `255`. A white
point at `[200, 220, 240]` turns it `[255, 255, 255]` and a `[100,
110, 120]` neighbour, each channel exactly half its white, into `[128,
128, 128]`. The two chain: after that white point a `[40, 60, 80]`
pixel reads `[51, 70, 85]`, and a black point clicked on it then makes
it `[0, 0, 0]` while the white pixel stays white and its alpha `128`
survives. With only the second pixel selected, sampling the first
still adjusts only the second. Off-canvas coordinates, an unknown
layer, and a locked layer error with the pixels intact. All five
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
forty-three: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The eyedropper arming was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1225 Rust tests total** (1220 → 1225, 1218 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 197 — Levels and Curves Gray Point eyedropper

`Document::levels_gray_point(id, x, y)` completes the eyedropper trio.
Clicking a pixel makes it neutral by giving each channel its own
gamma, chosen so the pixel's value in that channel lands on the
rounded mean of its three channels: for a channel value `c` and target
`t` the exponent is `ln(t/255) / ln(c/255)`, applied as
`(v/255)^exponent` to every pixel's value `v` in that channel, so the
clicked pixel's colour cast is lifted from the whole layer while its
brightness is roughly kept. A channel already at the target, or at `0`
or `255` where no gamma can move it, is left alone. Photoshop keeps
luminosity with its own weighting and lets the target grey be
configured; both are documented scope cuts, as before. The sample is
read with `layer_pixel` (off-canvas, unknown, and locked all error)
and the adjustment respects the selection. Both dialogs gain a **Gray
Pt** button beside Black Pt and White Pt, arming the same toolbar
eyedropper, which now runs `levels_gray_point` for it.

**Verified two ways.** Five new `document.rs` tests, every byte first
computed in Python emulating `f32`, with the raw products checked to
sit well away from a rounding boundary (the closest is `33.0065`).
Clicking `[100, 150, 200]` targets `150`: red's exponent is `0.5669`,
green's `1`, blue's `2.1841`, and the pixel becomes `[150, 150, 150]`.
The same exponents carry across the layer: `[50, 50, 50]` becomes
`[101, 50, 7]` (`101.26`, `7.26`), `[200, 200, 200]` with alpha `128`
becomes `[222, 200, 150]` (`222.19`), and `[150, 150, 150]` becomes
`[189, 150, 80]` (`188.76`, `80.02`). Clicking an already-neutral pixel
changes nothing anywhere. Clicking `[0, 128, 255]` — whose only movable
channel is already at the target `128` — changes nothing either, and
with only the second pixel selected, sampling the first adjusts only
the second. Off-canvas coordinates, an unknown layer, and a locked
layer error with the pixels intact. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
forty-four: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new button was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1230 Rust tests total** (1225 → 1230, 1223 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 198 — Auto Color, and Curves Auto

`Document::auto_color(id, shadow_clip, highlight_clip)` lifts Auto
Color off the deferred list now that the Gray Point machinery exists.
It is Photoshop's "Find Dark & Light Colors" followed by "Snap Neutral
Midtones": first each channel is stretched to full range exactly as
`auto_tone_clipped` does, with the same Clip percentages; then the
mean colour of the sampled pixels (the selection, or the whole layer)
is measured on the stretched result and every channel is given the
gamma that puts its mean on the mean of the three — the Gray Point
eyedropper applied to the average colour instead of a clicked pixel —
so an overall cast is removed. The eyedropper's step was lifted into a
private `neutralize_channels(id, values)` both share. Its target is now
the *exact* mean of the three values rather than a rounded one: three
equal values — a neutral colour — must be a no-op, and a rounded target
was leaving a hair of gamma behind. Phase 197's numbers are unaffected,
since its fixture's mean was the integer `150`. Photoshop's
luminosity-preserving snap and configurable target colours remain
documented scope cuts. An **Auto Color** button joins Auto Tone and
Auto Contrast (using the Levels dialog's clip percentages), and the
Curves dialog gains the same **Auto** button Levels has, so Curves Auto
is checked off as well.

**Verified two ways.** Five new `document.rs` tests, every byte first
computed in Python emulating `f32` through both steps. On `[40, 60,
80]`, `[140, 160, 180]`, `[200, 100, 60]` the stretch (red `40..200`,
green `60..160`, blue `60..180`) gives `[0, 0, 43]`, `[159, 255, 255]`,
`[255, 102, 0]`; the means are `138`, `119`, `99.33` with target
`118.78`, so red's exponent is `1.2443`, green's `1.0025`, blue's
`0.8104`, and the layer ends `[0, 0, 60]`, `[142, 255, 255]`, `[255,
102, 0]` (`60.26`, `141.67`, `101.77`). On the forty-pixel grey ramp
with `5%` clips the result is byte-identical to `auto_tone_clipped`.
With only the last two pixels selected, their stretch is `[0, 255,
255]` and `[255, 0, 0]`, whose means are all `127.5`, so nothing moves
and the first pixel is untouched. Alpha `10` and `128` survive, and a
`1000` clip, an unknown layer, and a locked layer error with the
pixels intact. Four of the five passed on the first run: the neutral
ramp came back one level high in a dozen places, because the rounded
target (`128` for a `127.6` mean) was applying a `0.996` gamma to a
layer that should have been left alone. That was a design flaw in the
shared step, not the test; the target was made exact, the Gray Point
tests were re-run against the Python model with the exact target (the
`[0, 128, 255]` case maps `128 → 127.67 → 128` and `50 → 49.69 → 50`,
the same bytes), and all ten pass.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
forty-five: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new buttons were
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1235 Rust tests total** (1230 → 1235, 1228 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 199 — Filter > Blur > Shape Blur

`Document::shape_blur(id, kernel, radius)` is Photoshop's Shape Blur
with three built-in kernels. It is the box blur's flat, edge-clamped,
truncating average — the same `average_samples` — taken over the
pixels inside a shape of the given radius rather than a square: a
diamond (`|dx| + |dy| ≤ r`, Manhattan distance) or a disc (`dx² + dy² ≤
r²`, Euclidean), so the blur's "bokeh" takes that shape. `Square` is
exactly the box blur, which now delegates to `shape_blur`, and every
sample still counts equally, including edge pixels sampled more than
once through clamping. Photoshop draws the kernel from any custom-shape
preset; the three built-ins are a documented scope cut. Alpha is
averaged with the colour, as the box blur always has. A zero radius, a
locked layer, and an unknown layer error. A **Shape Blur** button after
Box Blur opens a dialog with a Shape select (Circle, Diamond, Square)
and a Radius slider, applied through a `shape_blur` command.

**Verified two ways.** Five new `document.rs` tests on grey ramps whose
pixel `(x, y)` holds `step × (y × size + x)`, every value first
produced by a Python model of clamp-and-truncate over each lattice
shape. On the `7×7` ramp (step `3`) at radius `3`, the square's 49
clamped samples give `20` at the corner and `72` at the centre and are
byte-identical to `box_blur`; the diamond's 25 samples give `13`, `15`,
`20` along the top row, `72` at the centre, and `130` at the far
corner; the disc's 29 samples — it also takes the `(±2, ±2)` corners
the diamond leaves out — give `14`, `16`, `22`, `72`, and `129`. At
radius `1` on the `5×5` ramp (step `5`) the diamond is the plus of five
samples, so the clamped corner reads `(0 + 0 + 5 + 0 + 25) / 5 = 6` and
the centre `60`; at radius `2` the lattice disc and diamond are the
same thirteen points and the two grids are identical, with `11` at the
corner. A one-pixel selection confines the blur to its pixel, and a
transparent centre in an opaque plus averages to alpha `(0 + 4 × 255)
/ 5 = 204`. A zero radius, an unknown layer, and a locked layer error
with the pixels intact. All five passed on the first run once a test
helper was renamed: the fixture builder collided with an existing
`ramp_square` in the test module and the compiler caught it before any
test ran.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and forty-six:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The new dialog was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `npm
run build`) is fully green.

**1240 Rust tests total** (1235 → 1240, 1233 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 200 — Sharpen tool: Protect Detail and Sample All Layers

`Stroke::Sharpen` grows the Sharpen tool's two options-bar checkboxes.
**Protect Detail** leaves a channel alone when its local contrast —
`|sampled − blurred|`, the very difference the tool would amplify — is
under `PROTECT_DETAIL_THRESHOLD`, eight levels, so flat noise is not
sharpened into speckle; Photoshop's own Protect Detail is an
undisclosed halo-and-noise suppressor, and a fixed contrast threshold
(Unsharp Mask's Threshold at 8) is this project's explicit stand-in.
**Sample All Layers** measures the sharpening on the pre-stroke
*composite* rather than the layer alone — `stroke` now builds that
composite with `composite_pixel` before the layer is mutably borrowed,
only when the option is on — and paints the resulting change onto the
current layer's own pre-stroke value, so a sharpen stroke on an empty
layer can pull contrast up from the layers beneath it, as in Photoshop.
The `sharpen_stroke` command takes both flags (defaulting off) and the
Sharpen tool's options bar gains the two checkboxes.

**Verified two ways.** Five new `document.rs` tests on `ramped_3x3`,
whose radius-1 local contrasts a Python model of the clamped box blur
gives as `−13 −10 −6 / −3 0 4 / 7 10 14`. Protect Detail at full
strength leaves the five channels under eight in magnitude alone, so
the grid is `0 10 30 / 40 50 60 / 70 90 104` against the unprotected
`0 10 24 / 37 50 64 / 77 90 104`; at half strength the corner still
moves to `4` while the protected `(2, 0)` stays `30` rather than `27`.
A fully transparent solid-`50` layer over the ramp has no contrast of
its own — without Sample All Layers the stroke leaves every `50` — but
with it the composite is the ramp, whose contrasts land on the `50`s
as `37 40 44 / 47 50 54 / 57 60 64` with alpha still `0`. On a lone
opaque layer the option is byte-identical to the plain stroke, and
both options together on the transparent layer give the gated `37 40
50 / 50 50 50 / 50 60 64`. A one-pixel selection confines the stroke
and a locked layer errors. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
forty-seven: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The new checkboxes were
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1245 Rust tests total** (1240 → 1245, 1238 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 201 — Curves graph: histogram, baseline, intersection line

The Curves dialog gains Photoshop's graph. A new pure `curve_lookup(
points) -> [u8; 256]` builds the lookup table a point list describes —
the validation, sorting, straight-segment interpolation with half-up
rounding, and flat extension beyond the outer points that
`curves_points` used to do per pixel; `curves_points` now builds the
table once and indexes it, byte-for-byte the same result. A read-only
`curves_lookup` command exposes the table, and the dialog draws, in a
`256×256` SVG: the selected layer's luminosity **histogram** (the mean
of the three channel counts from the existing `histogram` command,
fetched when the dialog opens) as a grey area; the dashed identity
diagonal, Photoshop's **baseline**; the curve itself as a polyline
through the 256 table entries, refetched whenever the sliders or the
Point-mode list change; and the **intersection line** — a vertical
guide at the input and a horizontal one at the output of whichever
point's control last took focus. Show Clipping, Channel Overlays, and
the on-image adjustment tool remain open.

**Verified two ways.** Five new `document.rs` tests on `curve_lookup`,
every value hand-computed with the segment formula. The two endpoints
give the identity table. `(0, 0)`→`(128, 255)`→`(255, 255)` maps `10 →
19.9 → 20`, `60 → 119.5 → 120`, `64 → 127.5 → 128`, `100 → 199.2 →
199`, and `128`, `200`, `255 → 255`. `(192, 255)` and `(64, 0)` listed
backwards give `0` through input `64`, `72` at `100`, `128` at `128`,
and `255` from `192` up. For the five fixed inputs with outputs `0,
100, 128, 192, 255`, `curves_points` on the four-pixel fixture is
exactly the table applied per channel, with `lut[10] = 16`. No
points, one point, duplicate inputs, and a duplicate among three all
error. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
forty-eight: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The graph's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1250 Rust tests total** (1245 → 1250, 1243 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 202 — Curves per channel, with channel overlays

`Document::curves_channels(id, rgb, red, green, blue)` lifts the
"always the RGB composite" scope cut Curves shipped with: one point
list for the composite and one each for Red, Green, and Blue, all
applied together. As in Photoshop the channel curve runs first and
the composite second — a red value `v` becomes `rgb[red[v]]` — so a
red curve that halves and a composite that doubles are not the
identity. Every list goes through `curve_lookup` before any pixel
changes, so a bad list in any channel errors with the layer intact,
and identity channel lists make this exactly `curves_points`. The
Curves dialog gains a **Channel** select: the sliders and the Point-
mode list edit the chosen channel while the other channels' lists
wait in a store and are swapped in on switching; Apply sends all four
lists through a `curves_channels` command, and Reset clears every
channel. The graph now fetches all four lookup tables and draws every
channel's curve in its colour — Photoshop's Show Channel Overlays —
with the active channel's on top, thicker.

**Verified two ways.** Five new `document.rs` tests, every byte
hand-computed from the lookup tables. Four identity lists leave `[10,
64, 128]` and `[60, 200, 100]` untouched. The steep `(0, 0)`→`(128,
255)`→`(255, 255)` list on red alone maps `10 → 20` and `60 → 120`
while green and blue keep their values; on blue alone it maps `128 →
255` and `100 → 199`. Red halved (`(0, 0)`→`(255, 128)`, so `200 →
100.4 → 100`) and then the steep composite (`100 → 199`) gives
`[199, 255, 255]` on a `[200, 200, 200]` pixel — the other order would
have given `128` — proving the channel-then-composite order. The five
fixed inputs on the composite with identity channels are byte-
identical to `curves_points` on every pixel. A one-point green list
errors with the pixels intact, as do an unknown and a locked layer.
All five passed on the first run once a test constant was renamed:
`IDENTITY_CURVE` already existed in the test module for the five-
slider form, and the compiler caught the clash before any test ran.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
forty-nine: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The channel switching was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1255 Rust tests total** (1250 → 1255, 1248 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 203 — Curves Show Clipping

`Document::curves_clipping(id, rgb, red, green, blue)` is the Curves
dialog's Show Clipping readout: a read-only count of how many of the
layer's pixels — the selection's, or the whole layer's — the four
curves would drive to pure black (every channel `0`) and to pure
white (every channel `255`), applying the channel curves before the
composite exactly as `curves_channels` does. Photoshop paints the
clipped pixels over the image while an endpoint is Alt-dragged; a
live count is this project's readout, a documented scope cut. The
dialog gains a **Show Clipping** checkbox that, while on, refetches
the counts through a read-only `curves_clipping` command every time
any channel's points change and shows them as "N black · M white".

**Verified two ways.** Five new `document.rs` tests on the four-pixel
Curves fixture `[10, 64, 128]`, `[60, 200, 100]`, `[0, 192, 255]`,
`[30, 30, 30]`, every count worked out by hand from the tables. Four
identity lists count nothing. A composite crushed to `0` through input
`100` and lifted to `255` from `150` drives only `[30, 30, 30]` to
black — the others keep at least one channel off `0` — for `(1, 0)`; a
composite that jumps to `255` at input `10` makes three pixels white,
the `[0, 192, 255]` one staying out because its red is `0`, for `(0,
3)`. A red list that lifts red to `255` everywhere changes nothing
under an identity composite but makes that fourth pixel white too
under the jumping one, `(0, 4)`, proving the channel-first order. With
only the last pixel selected the crush counts `(1, 0)`; with the first
three, `(0, 0)`; and the pixels are untouched either way. A one-point
list and an unknown layer error. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fifty:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The readout's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1260 Rust tests total** (1255 → 1260, 1253 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 204 — Curves Pencil mode

`Document::curves_table(id, table)` is Curves in Photoshop's Pencil
mode: the curve is a raw 256-entry table drawn freehand rather than a
point list, applied as given to all three channels (a Pencil curve on
a single channel is a documented scope cut). The dialog's **Smooth**
button is a pure `smooth_curve_table`: every entry becomes the rounded
mean of itself and its two neighbours, the ends using themselves in
place of the missing neighbour, so one press rounds a step off by a
third of its height on each side while any straight line — the
identity included — is left exactly as it was. The dialog gains a
**Pencil mode** checkbox; while it is on, dragging across the graph
sets the table at the pointer, filling the gap from the last sample
with a straight run so a fast stroke stays continuous, the drawn
table is shown in yellow, Smooth runs a `smooth_curve` command over
it, Apply sends it through `curves_table`, and Reset restores the
identity table. Both commands refuse a table that is not exactly 256
entries.

**Verified two ways.** Five new `document.rs` tests, every byte
hand-computed. The identity table is a no-op and the reversed table
inverts: `[10, 64, 128] → [245, 191, 127]` and `[60, 200, 100] → [195,
55, 155]`, alpha kept. A jagged table with `100` at input `10` and `0`
elsewhere is applied exactly as drawn. Smoothing a step from `0` to
`255` at input `128` gives `0, 85, 170, 255` across inputs `126–129`
(`(0 + 0 + 255 + 1) / 3` and `(0 + 255 + 255 + 1) / 3`) with the ends
still `0` and `255`, the identity smooths to itself, and a second
pass spreads the step to `28, 85, 170, 227`. A one-pixel selection
confines the table, and an unknown or locked layer errors. All five
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fifty-one:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The drawing handlers were reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1265 Rust tests total** (1260 → 1265, 1258 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 205 — Curves on-image adjustment tool

`curve_with_point(points, input, delta)` is the pure half of
Photoshop's on-image adjustment tool for Curves: given the current
point list, the tone of the pixel pressed on, and the drag's height in
output levels, it returns the list with the point at that input moved
by `delta` — or, when no point sits there, a new point inserted at the
curve's current output for that input plus `delta` — clamped to
`0..=255` and sorted. The dialog gains an **On-image** button that
arms the tool and closes; pressing on the picture samples the layer
pixel under the pointer (`rgb_levels`) and takes the mean of its
channels as the input, a vertical drag counts one output level per
screen pixel (up to lighten), and pointer-up runs a read-only
`curve_with_point` command over the active channel's list, switches
the dialog to Point mode with the result, focuses the new point (so
the intersection lines sit on it), and reopens the dialog. A Cancel
button in the toolbar disarms and reopens instead. Pencil mode
disables the button, since a freehand table has no points to move.

**Verified two ways.** Five new `document.rs` tests, every value
hand-computed from the lookup tables. On the identity, input `100`
sits at `100`, so a `+30` drag adds `(100, 130)`; on the steep `(0,
0)`→`(128, 255)`→`(255, 255)` curve input `64` sits at `128`, so `−8`
gives `(64, 120)`. An existing point moves instead: `128` on the steep
curve by `−55` becomes `(128, 200)`, and the endpoint `(0, 0)` by `+40`
becomes `(0, 40)`. Output clamps (`250` by `+100` gives `(250, 255)`)
and an unsorted list comes back sorted with a floor-clamped point
(`(10, 0)`). The list the tool produces is what the dialog then
applies: with `(100, 130)` inserted the table reads `65` at `50`, `130`
at `100`, and `210.6 → 211` at `200`. A one-point list and duplicate
inputs error. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fifty-two:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The drag wiring was reviewed by hand instead
— including that the pointer capture is released before the tool's
early return. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1270 Rust tests total** (1265 → 1270, 1263 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 206 — Selection Brush tool

`Document::select_brush_with(mode, points, radius)` is Photoshop's
Selection Brush: paint a stroke and every pixel whose centre lies
within the brush radius of the drag's polyline — `point_segment_
distance`, the brush tools' own capsule, but hard-edged, since a
selection here is a bitmap — joins the selection, combined with the
current one per `mode`. The tool adds by default, subtracts with Alt,
and intersects with Shift+Alt, through the same `combine_with` engine
the marquees, lassos, and Magic Wand use, so a stroke can also start a
selection from nothing. A single point paints a dot, only the pixels
inside the stroke's radius-grown bounding box are tested, and the
mask goes through `SelectionMask::bounds` so a stroke that touches no
pixel, or a subtraction that would empty the selection, errors.
Photoshop's brush hardness and the overlay's opacity are documented
scope cuts. A new **Selection Brush** tool button sits after Lasso; it
reuses the lasso's trail capture, previews the stroke as a translucent
blue band at the current Brush Size, and sends the trail through a
`select_brush` command at pointer-up.

**Verified two ways.** Five new `document.rs` tests, every mask drawn
first by a Python model of the pixel-centre distance to each segment.
On a `5×5`, a stroke along `y = 2.5` at radius `0.5` selects the middle
row, at `1.5` the middle three rows, and a `(0.5, 0.5)`→`(4.5, 4.5)`
diagonal at `0.5` selects only the five centres on the line. A single
point at the centre selects a plus at radius `1` (the diagonal
neighbours sit `1.414` away) and the `3×3` at `1.5`. Adding that plus
to a `2×2` corner rectangle gives `## / ### / .### / ..#`; subtracting
a stroke along the top row then clears it; intersecting with a
vertical stroke leaves the middle column of what remained. Adding
with nothing selected starts a selection, and a stroke hanging off the
left edge selects only its on-canvas pixel. No points, a zero radius,
a `NaN` coordinate, a stroke entirely off the canvas, subtracting with
nothing selected, and subtracting the whole selection all error with
the selection intact. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
fifty-three: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1275 Rust tests total** (1270 → 1275, 1268 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 207 — Quick Selection tool

`Document::quick_select_with(mode, id, points, radius, tolerance)` is
Photoshop's Quick Selection: a Selection Brush stroke that then grows
like Select > Grow. The stroked pixels seed a flood — every pixel
4-connected to them through pixels whose colour lies within the
stroked pixels' own per-channel range widened by the Tolerance joins
in — so a short dab inside a region selects the region, and the result
is combined with the current selection per `mode`, adding by default
and subtracting with Alt. To share the pieces, the Selection Brush's
capsule became a free `brush_bits`, the mask construction a
`mask_selection`, and Select > Grow's flood a `grow_bits` that
`grow_selection` now calls; none of their behaviour changed. Brush
hardness, Auto-Enhance, and Photoshop's edge detection are documented
scope cuts. A **Quick Selection** tool button sits after Selection
Brush, sharing its stroke capture and preview band, with the Tolerance
slider shown while it is active, and sends the trail through a
`quick_select` command at pointer-up.

**Verified two ways.** Five new `document.rs` tests on a `5×5` whose
three left columns are red `200` and two right columns blue `200`,
with one red pixel at `(1, 1)` nudged to `220`, every mask reasoned
out from the per-channel range rule and the 4-connected flood. A dot
at `(0, 4)` with tolerance `0` floods every exact-`200` red pixel and
stops at the `220` one; tolerance `20` lets it through. A stroke that
touches one red and one blue pixel has the range red `0..=200`, green
`0`, blue `0..=200`, which every pixel but the `220` red satisfies, so
the flood crosses the boundary and takes both regions. Adding a blue
dab selects the blue columns, adding a red dab then takes all but the
`220` pixel, and subtracting a blue dab removes the blue columns
again. A radius-`1.5` dab around `(1, 1)` already seeds the `220`
pixel, so the whole red region floods even at tolerance `0`. No
points, an off-canvas dab, an unknown layer, and subtracting with
nothing selected all error and leave nothing selected. All five
passed on the first run; a contradictory expectation left in the
boundary-spanning test from working out the per-channel range was
removed before that run, so the test as first executed is the one
recorded here.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
fifty-four: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The tool's wiring was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1280 Rust tests total** (1275 → 1280, 1273 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 208 — Magnetic Lasso tool

`Document::select_magnetic_lasso_with(mode, id, trail, width,
contrast)` is Photoshop's Magnetic Lasso: a freehand trail whose
points snap to the strongest nearby edge before the enclosed area is
selected. For each trail point the square window of half-size `width`
pixels around it is searched for the pixel with the greatest edge
strength — the largest of `sobel_at`'s three channel magnitudes — that
is at least `contrast`; the strongest wins, the nearest of equals,
then the first in row order. The point is then moved onto that pixel's
nearer edge on each axis along which it lies outside the pixel, keeping
its own coordinate on an axis where it already lies within the pixel's
span, so a snapped outline runs along pixel boundaries rather than
through pixel centres (which the even-odd rule would exclude). A point
with no qualifying edge in reach stays put, and the snapped trail goes
through the Lasso's own `select_lasso_with`. Photoshop's frequency,
pen-pressure width, and live anchoring are documented scope cuts. A
**Magnetic Lasso** tool button sits after Lasso with **Width** and
**Contrast** sliders, sharing the lasso's trail capture and preview
and sending the trail through a `select_magnetic_lasso` command at
pointer-up with the marquee modifier keys.

**Verified two ways.** Five new `document.rs` tests on a `6×4` whose
left three columns are black and right three white, every snap and
mask first produced by a Python model of the Sobel strengths (columns
2 and 3 read `255`, the rest `0`), the search, the per-axis snap, and
the even-odd fill. A loose outline `(0.2, 0.2)`→`(0.2, 3.8)`→`(5.8,
3.8)`→`(5.8, 0.2)` with width `2` snaps to `x = 2` and `x = 4` while
keeping its own `y`, selecting exactly columns 2–3; with width `1` the
edge is out of reach and all 24 pixels are selected. A black-to-`30`
step has strength `120`, so it is ignored at contrast `128` and taken
at `100`, and the full-strength edge clears even `255`. A point at
`x = 2.3` inside the edge column keeps its `x` while `4.6` snaps to
`4`, still selecting columns 2–3, and Add mode unions that with a
selected first column. Width `0`, an unknown layer, a `NaN` point, and
a two-point trail all error with nothing selected. All five passed on
the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fifty-five:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The tool's wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1285 Rust tests total** (1280 → 1285, 1278 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 209 — Object Selection tool

`Document::select_object_in_rect_with(mode, id, x0, y0, x1, y1,
tolerance)` and `select_object_in_lasso_with(mode, id, trail,
tolerance)` are Photoshop's Object Selection tool in its Rectangle and
Lasso modes, built on one private finder that is this project's
explicit stand-in for Photoshop's neural detection. Within the dragged
region — a marquee-normalised box, or the even-odd polygon through a
freehand outline — the *background* is taken to be the most common
colour of the region's border ring (its pixels with a 4-neighbour
outside the region or off the canvas; ties go to the lower colour),
every region pixel outside that colour ± `tolerance` per channel is
foreground, and the *object* is the largest 4-connected foreground
component. The result is always a hard-edged mask combined with the
current selection per `mode`. A region with no pixels, or one in which
no foreground pixel is found, errors. Photoshop's Object Finder (hover
detection), its refresh, and its soft edge are documented scope cuts.
Two tool buttons follow Magnetic Lasso — **Object Select**, which
reuses the marquee's box drag, and **Object Lasso**, which reuses the
lasso's trail — both showing the Tolerance slider and honouring the
marquee modifier keys, through `select_object_rect` and
`select_object_lasso` commands.

**Verified two ways.** Five new `document.rs` tests on a `7×7` black
canvas with a `3×3` grey-`200` object, a one-pixel `200` speck at the
bottom-left corner, and a faint `40` mark at `(5, 1)`, every result
first produced by a Python model of the ring vote, the tolerance test,
and the component search. Boxing the whole canvas votes black, makes
the object, the speck, and the mark foreground, and selects the
nine-pixel object as the largest component; a box just around the
object finds the same nine. Boxing the mark alone selects its single
pixel at tolerance `0` and finds nothing at `50`; a `2×2` box on the
speck has three black ring pixels outvoting it, so the speck is
selected. A diamond lasso through the canvas's edge midpoints encloses
the object and none of the corners and selects the nine again.
Subtracting the object from Select All cuts a `3×3` hole, adding the
speck fills its row, and adding the object fills the canvas. A box off
the canvas, a box inside the object's flat colour, an unknown layer,
and a two-point lasso all error with nothing selected. Four of the five
passed on the first run: the mark test first put its expected pixel on
row 0 when the fixture places the mark on row 1, and its `3`-row box
also caught a corner of the object, which the finder correctly
reported as foreground at tolerance `50` where the test expected
nothing — both slips in the fixture, not the finder. The box was
shortened to two rows and the row fixed, and it passed.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and fifty-six:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The tools' wiring was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1290 Rust tests total** (1285 → 1290, 1283 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 210 — Select Subject and Remove Background

`Document::select_subject_with(mode, id, tolerance)` is Select >
Subject: the Object Selection finder run over the whole canvas, so the
most common colour of the canvas's outer ring is the background and
the largest 4-connected thing that is not it, within the Tolerance, is
the subject, combined with the current selection per `mode`.
`remove_background(id, tolerance)` keeps that subject and makes every
other pixel of the layer fully transparent, leaving the selection as
it was. The finder was split so its bitmap can be had without touching
the selection (`find_object_in_bits`), which the Object Selection tool
now also goes through. Photoshop's neural subject detection, on device
or in the cloud, is replaced by this explicit stand-in, and Select
People, Sky Selection, and Focus Area stay deferred as neural. Two
buttons follow the Object Selection tools — **Select Subject** (using
the selection Mode picker) and **Remove Background** — through
`select_subject` and `remove_background` commands.

**Verified two ways.** Five new `document.rs` tests on the Object
Selection scene, whose subject the Phase 209 Python model had already
produced. Select Subject picks the nine-pixel object; adding it to a
selected top row and subtracting it from Select All give `#######`
over the object and a `3×3` hole respectively. Remove Background keeps
the object's `200`s opaque and makes the corner, the speck, and the
mark transparent, so row 3's alphas read `0 0 255 255 255 0 0`, and a
prior one-pixel selection survives it. A flat layer has no subject, so
both operations error with the pixels intact, as do an unknown layer
and, for Remove Background, a locked one. Four of the five passed on
the first run: the Remove Background test reached for the test
module's `alpha_grid`, which reads a fixed `3×3`, on a `7×7` and
panicked on the index; it now reads the row through `pixel`, and
passed.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
fifty-seven: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The buttons were reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1295 Rust tests total** (1290 → 1295, 1288 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 211 — Mask All Objects

`Document::mask_all_objects(id, tolerance)` is Select > Mask All
Objects. The Object Selection finder now returns *every* 4-connected
foreground component on the whole canvas — against the canvas edge's
most common colour, within the Tolerance — largest first (ties by
first pixel), through a new `foreground_components` that the Object
Selection tool and Select Subject take the first of. Each component is
saved as a named selection `Object 1`, `Object 2`, … replacing any
earlier selection of that name and leaving other saved selections
alone, and the current selection becomes all of them together; the
count is returned, and a layer with no object errors with nothing
saved. Photoshop builds one layer mask per object inside a group;
with no masks in this layer model, saved selections are the stand-in,
so Load Selection reaches each object individually. A **Mask All
Objects** button follows Remove Background. This phase also reconciles
three checklist rows — Solid Color Fill, Gradient Fill, Pattern Fill —
that duplicate the PART VI fill layers shipped long ago, in the
wording the earlier duplicates use.

**Verified two ways.** Five new `document.rs` tests on the Object
Selection scene, whose components the Phase 209 Python model listed:
the nine-pixel object, the `40` mark at `(5, 1)`, and the `200` speck
at `(0, 6)`. At tolerance `0` three objects are saved in that order —
`Object 1` loads as the `3×3`, `Object 2` as the mark (first in row
order of the two singles), `Object 3` as the speck — and the selection
afterwards is all three together. At tolerance `50` the mark is
background, so two are saved and `Object 2` is the speck. A rectangle
saved as `Object 1` and a `Keep me` selection saved beforehand are
respectively replaced and kept, for four saved selections in all. A
flat layer errors with nothing saved and nothing selected, as does an
unknown layer. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
fifty-eight: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The button was reviewed by
hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1300 Rust tests total** (1295 → 1300, 1293 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 212 — Guides: New Guide, Guide Layout, Clear Guides

The document gains ruler guides. A `Guide` is a horizontal or vertical
line on a pixel boundary, `position` pixels from the top or left, so
`0` is the near edge and the canvas's height or width the far one.
`add_guide` (View > New Guide) validates the position and ignores a
duplicate, `remove_guide` errors when nothing is there, `clear_guides`
empties the list, and `guide_layout(columns, rows)` (View > New Guide
Layout) adds the interior boundaries of an equal split — `k × width /
columns` rounded to the nearest pixel, and likewise for rows — on top
of whatever guides exist; Photoshop's gutters, margins, and per-column
widths are documented scope cuts, as is snapping. Guides are the one
piece of position-bound document data that survives the two
canvas-reshaping operations: a document rotation turns them with the
picture (a vertical guide at `c` becomes horizontal at `c` clockwise or
at `old width − c` counter-clockwise; a horizontal one at `c` becomes
vertical at `old height − c` clockwise or at `c`), and a crop carries
them along, shifted by the crop's origin, dropping any left outside
it — one on the crop's own far edge is kept. The frontend draws each
guide as a cyan hairline over the canvas (a click on one removes it)
and a **Guides…** dialog offers New Guide (orientation and position),
New Guide Layout (columns and rows), and Clear Guides through
`add_guide`, `remove_guide`, `guide_layout`, and `clear_guides`
commands. Guides ship in the document view as `guides`.

**Verified two ways.** Five new `document.rs` tests on a `9×6` canvas,
every position worked out by hand. Adding vertical `3`, horizontal
`0`, vertical `3` again, and vertical `9` keeps three guides in
placement order, and `10` or horizontal `7` error without adding.
Removing a guide leaves the other, removing it again errors, and Clear
empties the list. A `3 × 2` layout adds verticals at `3` and `6` and a
horizontal at `3`; a four-column split of `9` rounds `2.25, 4.5, 6.75`
to `2, 5, 7` (half away from zero); `1 × 1` adds nothing. Clockwise
rotation turns vertical `3` into horizontal `3` and horizontal `2`
into vertical `6 − 2 = 4`; counter-clockwise turns them into
horizontal `9 − 3 = 6` and vertical `2`. Cropping to `(2, 1)–(7, 5)`
drops vertical `1`, shifts `4` and `7` to `2` and `5`, and keeps the
horizontal guide on the crop's bottom edge at `4`. Three of the five
passed on the first run: the rotation test had derived the clockwise
horizontal guide's new position from the old width rather than the old
height (`7` instead of `4`), and the crop test had forgotten that a
guide on the crop's far edge is inside the inclusive range and
survives; both were slips in the expectations, checked against the
pixel-mapping formulas and the documented rule, and the tests pass
with the corrected values.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
fifty-nine: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The overlay and dialog
were reviewed by hand instead. Every other layer of this project's
quality bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy
--all-targets -- -D warnings`, `npm run build`) is fully green.

**1305 Rust tests total** (1300 → 1305, 1298 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 213 — Link Layers

Every `Layer` gains a `linked` flag — one link set per document, as in
Photoshop's original linking — set by `set_linked` and reported in the
layer view. `move_pixels` now takes every linked layer along with a
linked one: it collects the targets (the layer alone when it is not
linked), refuses the whole move if any target is locked, and then
either translates each target outright or, with a selection active,
lifts the selected pixels on each target and sets them down at the
offset, moving the selection once at the end; that per-layer pixel
half was split out as `move_layer_pixels` so the loop and the old
single-layer path are the same code. Moving an unlinked layer never
disturbs the linked set. The layer panel gains a link checkbox beside
the lock, through a `set_layer_linked` command. Photoshop's Select
Linked Layers and its per-layer link *groups* are documented scope
cuts.

**Verified two ways.** Five new `document.rs` tests on three `3×3`
layers each holding one opaque dot at the origin in red, green, and
blue, every landing pixel reasoned out by hand. With red and blue
linked, moving red by `(1, 2)` puts both red and blue dots at `(1, 2)`
and leaves the green one at the origin; moving the unlinked green by
`(2, 0)` leaves red and blue where they were. With the origin pixel
selected, moving blue by `(1, 1)` moves red's and blue's dots and the
selection to `(1, 1)` exactly once. Locking blue makes a move of the
linked red error with both dots in place. Unlinking blue restores its
independence, the view reports `[true, false, true]` before that and
the ids of the linked pair, and an unknown layer errors. All five
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and sixty:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The panel checkbox was reviewed by hand
instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1310 Rust tests total** (1305 → 1310, 1303 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 214 — Move tool: Auto-Select and hover layer bounds

Two read-only queries give the Move tool its Photoshop options.
`Document::layer_at(x, y)` is Auto-Select in its Layer mode: the
topmost visible layer with a non-transparent pixel at that point, or
`None` when nothing is there — hidden layers and fully transparent
pixels are looked through, and a point off the canvas is `None`.
`layer_bounds(id)` is the hover outline: the bounding box of a layer's
non-transparent pixels, `None` for an empty layer, an error for an
unknown one. With the Move tool active the options bar shows an
**Auto-Select** checkbox; when it is on, pressing on the canvas asks
`layer_at` for the layer under the pointer and makes it the selected
layer before the drag moves it. Hovering with the Move tool asks
`layer_at` and then `layer_bounds` once per pixel the pointer enters
and draws the result as a yellow outline over the canvas, cleared
while dragging. Auto-Select's Group mode is a documented scope cut in
this groupless layer model, as is Show Transform Controls.

**Verified two ways.** Five new `document.rs` tests on the three-dot
fixture, every answer reasoned out by hand. With the blue dot on top
the origin belongs to blue; move blue one pixel right and the origin
belongs to green while `(1, 0)` belongs to blue; move green away and
the origin belongs to red. Hiding blue, then green, hands the origin
down the stack, an empty pixel is `None`, and a point off the canvas
is `None`. Red's bounds are the origin pixel, `(0, 0)–(1, 1)`, and
`(1, 2)–(2, 3)` after a move; a `5×4` layer with a barely visible
pixel at `(3, 1)` and an opaque one at `(0, 3)` spans `(0, 1)–(4, 4)`,
a fully transparent layer has `None`, and an unknown layer errors. All
five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and sixty-one:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The hover and press wiring was reviewed by
hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1315 Rust tests total** (1310 → 1315, 1308 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 215 — Smart Guides

`Document::snap_move(id, dx, dy, threshold)` is Photoshop's Smart
Guides for the Move tool: a read-only query that snaps a drop offset
so an edge of what is being moved — the active selection's bounding
box, or the layer's own opaque bounds — lands exactly on a guide, on
an edge of another visible layer's opaque bounds, or on the canvas
edge whenever it would otherwise come within `threshold` pixels of
one. Each axis snaps independently to the smallest correction; on a
tie the leading edge (left or top) wins, and an edge already on a
target needs no correction. Hidden layers and the moving layer itself
are not targets. Photoshop also snaps centres and draws the pink
alignment lines; both are documented scope cuts. The Move tool's
options bar gains a **Smart Guides** checkbox, on by default; with it
on, a drop asks `snap_move` with an 8-pixel threshold and moves by the
snapped offset (the Content-Aware Move and Patch drags are untouched).

**Verified two ways.** Five new `document.rs` tests on a `10×10`
canvas with a `2×2` block at `(1, 1)` on the moving layer and another
at `(6, 6)` on a second layer, every correction worked out by hand
from the edge lists. A drag of `(2, 0)` at threshold `2` puts the
mover's right edge one short of the other block's left edge, so `dx`
becomes `3`, while its top edge at `1` is one from the canvas top, so
`dy` becomes `−1`; `dx = 10` puts the left edge one past the canvas
edge and is pulled back to `9`; `dx = 20` is out of reach of
everything. With a vertical guide at `8`, `dx = 4` leaves the right
edge one from the guide and the left edge one from the other block —
a tie the leading edge wins, `5`; `dy = 5` sits exactly on the other
block's top, correction `0`; `dy = 6` has the top one from that block's
top and the bottom one from its bottom and the canvas edge, and the
leading edge's snap wins, `5`; `dy = 7` sits on the block's bottom.
With a `(1, 1)–(4, 4)` selection, `(1, 0)` snaps to `(2, −1)` — the
selection's right edge onto the block, its top onto the canvas. Hiding
the other layer leaves `(2, 3)` alone, a zero threshold never snaps,
an empty layer passes its offset through, and an unknown layer
errors. Two of the five passed on the first run: three expectations
each overlooked one nearer target the rule finds — the canvas edge
next to a far-flung left edge, the other block's top edge tying with
the canvas bottom, and the canvas edge next to a selection's left edge
— and were corrected to what the documented rule gives, checked
against the edge lists by hand.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and sixty-two:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The drop wiring was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `npm
run build`) is fully green.

**1320 Rust tests total** (1315 → 1320, 1313 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 216 — Layer groups

The document gains Layer > Group Layers as metadata over the flat
stack: a `LayerGroup` is a name and a list of member ids kept in
stack order, bottom to top, and a layer belongs to at most one group.
`group_layers(ids, name)` validates the name, the ids, and that none
is already grouped; `ungroup(index)` dissolves a group and leaves its
layers where they are; `set_group_visible` and `set_group_locked`
apply to every member at once; `move_group(index, dx, dy)` moves every
member — whole layers, or their selected pixels with a selection
active, the selection moving once — refusing if any member is locked,
through the same `translate` and `move_layer_pixels` the Move tool and
Link Layers use; and `group_at(x, y)` is Auto-Select in Group mode,
the group of the layer under the pointer. Removing a layer takes it
out of its group, and a group left empty dissolves. A group composites
exactly as its members do (Photoshop's Pass Through); nesting and
group-level opacity or blend modes are documented scope cuts. The
layer panel draws a header row — folder icon, name, a visibility
checkbox for the whole group, and Ungroup — above each group's top
member; a **Group Layers** button groups the selected layer together
with every linked layer as `Group N`; and the Move tool's Auto-Select
gains a **Group** option that picks up the whole group under the
pointer and moves it through `move_group`. The document view carries
`groups`.

**Verified two ways.** Five new `document.rs` tests on the three-dot
fixture, every outcome reasoned out by hand. Grouping blue and red,
listed top-first, stores them bottom-first as `[red, blue]` under the
given name at index `0`; a second group that reuses red errors, as do
an empty list, a blank name, and an unknown id. Ungrouping the first
of two groups leaves the second; removing green dissolves its solo
group; removing red leaves `[blue]` in the pair. Hiding the pair hides
red and blue but not green (`[false, true, false]`), and locking it
locks the same two. Moving the pair by `(2, 1)` moves red's and blue's
dots and leaves green's, a zero move returns `None`, a locked member
blocks the move with the dots in place, and with one pixel selected
only that pixel of each member moves and the selection moves once.
The origin belongs to no group until the pair exists, then to it, and
to none again once blue is hidden and the ungrouped green is on top.
All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-three: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The panel and Move-tool
wiring was reviewed by hand instead. Every other layer of this
project's quality bar (hand-verified Rust tests, `cargo fmt`, `cargo
clippy --all-targets -- -D warnings`, `npm run build`) is fully green.

**1325 Rust tests total** (1320 → 1325, 1318 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 217 — Clipping masks

Every `Layer` gains a `clipped` flag — Layer > Create Clipping Mask —
set by `set_clipped`, which refuses the bottom layer since it has
nothing to clip to. The compositor now takes the layers as a slice
rather than an iterator and, for a clipped layer, finds its *base* —
the nearest unclipped layer below it in the slice — and scales the
clipped layer's alpha by the base's own transparency at that pixel
(not by the base's opacity), so the layer shows only where the base
has pixels; stacked clipped layers share the one base beneath them,
and releasing a middle layer makes it the base of those above. A new
`Document::compositing_layers` supplies that slice: every visible,
non-zero-opacity layer, except a clipped layer whose base does not
itself take part, which Photoshop hides along with its base; the three
flatteners and `composite_pixel` all build the list once per pass
instead of filtering per pixel. The layer panel gains a clip checkbox
beside link, through a `set_layer_clipped` command.

**Verified two ways.** Five new `document.rs` tests through
`composite_pixel`, every byte first computed in Python emulating the
`f32` Normal composite. Opaque green clipped to a base that is red at
`(0, 0)` and transparent at `(1, 0)` shows green at the first pixel
and nothing at the second, and releasing it brings the green back.
Clipped to a half-transparent red base (`alpha 128`) the green reads
`[85, 170, 0, 192]` — its alpha scaled to `0.502`, composited over the
red — and at `50%` layer opacity `[153, 102, 0, 160]`. Hiding the base
hides the clipped layer too (no layer composites at all), while hiding
the clipped layer leaves the red base. Blue clipped above clipped
green shows blue only where the red base has pixels; releasing green
makes it blue's base, so blue then shows everywhere. Clipping the
bottom layer errors, as does an unknown one. All five passed on the
first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-four: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The panel checkbox was
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1330 Rust tests total** (1325 → 1330, 1323 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 218 — Layer masks

Every `Layer` gains an optional `mask`: a document-sized 8-bit buffer
multiplied straight into the layer's alpha at composite time, `255`
showing and `0` hiding, before opacity and any clipping mask apply.
`add_layer_mask(id, source)` starts one from Photoshop's four menu
entries — Reveal All (white), Hide All (black), Reveal Selection
(white inside the selection, black outside), Hide Selection (the
reverse) — replacing any mask the layer had; `set_layer_mask` installs
a supplied buffer, the way a painted or imported mask will arrive, and
insists on one byte per document pixel; `remove_layer_mask(id, apply)`
deletes the mask, or with `apply` first multiplies it into the layer's
own alpha so the picture keeps looking the same, refusing a locked
layer. Masks turn with a document rotation and are cropped with a
crop, by the same index mapping as the pixels. The layer view carries
`hasMask`, the panel shows a mask badge beside a masked layer's name,
and four toolbar buttons — **Add Mask** (reveal the selection, or all
with nothing selected), **Hide All Mask**, **Apply Mask**, **Delete
Mask** — go through `add_layer_mask` and `remove_layer_mask`
commands. Painting directly on the mask, mask density and feather, and
the mask's channel view are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests through
`composite_pixel`, the grey values first computed in Python emulating
`f32`. On the clipping fixture, Reveal All leaves the green layer
showing everywhere and Hide All hides it, revealing the red base at
the first pixel and nothing at the second. With the first pixel
selected, Reveal Selection shows green there and nothing beside it,
Hide Selection the reverse, and with nothing selected the selection
variants error. A grey `128` mask takes opaque green to alpha `128`
and alpha `200` to `100`; a `64` mask at `50%` opacity gives `32`; a
three-byte mask on a two-pixel layer errors. Applying a `[128, 0]`
mask bakes alphas `128` and `0` into the pixels and removes the mask,
deleting a later all-black mask leaves them as they are, removing a
mask that is not there errors, and a locked layer refuses to apply
with its mask kept. A `3×2` layer masked to show only `(2, 0)` shows
only `(1, 2)` after a clockwise rotation and only `(0, 1)` after a
crop to `(1, 1)–(2, 3)`. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-five: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The buttons and badge were
reviewed by hand instead. Every other layer of this project's quality
bar (hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets
-- -D warnings`, `npm run build`) is fully green.

**1335 Rust tests total** (1330 → 1335, 1328 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 219 — Vector masks

`Document::add_vector_mask(id, points, reveal)` is Layer > Vector Mask
> Current Path: the polygon through `points` is rasterised — pixel
centres inside it by the even-odd rule, the Polygonal Lasso's own
test, after consecutive duplicate points are dropped — into the same
8-bit mask a layer mask uses, white inside and black outside, or the
reverse with `reveal` false, replacing any mask the layer had. From
then on it *is* a layer mask: it composites, applies, deletes, turns,
and crops exactly as Phase 218's do. A path needs three or more
distinct points and must enclose at least one pixel centre. Keeping
the path editable as vectors, and holding a vector mask beside a
separate pixel mask, are documented scope cuts. A **Vector Mask** tool
button reuses the lasso's trail capture and preview: draw a closed
path on the canvas and pointer-up sends it through an
`add_vector_mask` command, Alt hiding the inside instead of revealing
it.

**Verified two ways.** Five new `document.rs` tests through
`composite_pixel`, every mask reasoned out from the even-odd rule. On
a solid `5×5`, the diamond through the canvas's edge midpoints holds
the centres with `|dx| + |dy| < 2.5` from the middle, so the composite
shows the thirteen-pixel diamond `..#.. / .###. / ##### / .###. /
..#..` and the layer reports a mask. On a `3×3`, the centre pixel's
square as a path hides that pixel and shows the corners with `reveal`
false, and the reverse with it true. Applying the mask bakes alpha
`255` at the centre and `0` at a corner and removes it. Duplicate
consecutive points are dropped, a two-point path errors, and a sliver
that lies between pixel centres errors as enclosing nothing. An
unknown layer and a `NaN` coordinate error with no mask added. All
five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-six: this session's Xvfb instance was already confirmed, through
a control test and a full Xvfb-and-application restart in Phase 52, to
have stopped delivering synthetic `xdotool` pointer clicks to the
webview entirely, and re-running that diagnostic again was judged
unlikely to produce new information. The tool's wiring was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1340 Rust tests total** (1335 → 1340, 1333 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 220 — Adjustment layers

The document gains its first non-destructive layer kind. An
`Adjustment` is one of Invert, Brightness/Contrast, Threshold, or
Posterize with its parameters, and `apply_adjustment(adjustment, rgb)`
is the pure per-pixel function behind it — the very formulas the four
destructive commands used, which now call it too, so a live layer and
a baked command agree byte for byte by construction. A layer carrying
an `adjustment` (`add_adjustment_layer(name, adjustment)`, re-tuned by
`set_adjustment`) has a fully transparent pixel buffer that is never
composited; instead the compositor, on reaching it, reshapes the
backdrop's colour by the adjustment and blends the result in at the
layer's *strength* — its opacity, through its mask, and through a
clipping base's transparency, exactly as a pixel layer's alpha would
be scaled — leaving the backdrop's alpha alone, so an adjustment over
nothing does nothing. Threshold and Posterize keep their dialog
bounds. The layer view carries `adjustment`, the panel shows a badge,
and an **Adjustment Layer…** dialog picks the kind and parameters and
either adds a new layer or re-tunes the selected adjustment layer
through `add_adjustment_layer` and `set_adjustment` commands. The
remaining Image > Adjustments as live layers, and Photoshop's
Properties panel, are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests through
`composite_pixel`, the blended value first computed in Python
emulating `f32`. An Invert layer over `[200, 100, 50]` composites
`[55, 155, 205]`, its own pixels are transparent, and over an empty
document it composites nothing. Brightness `+30` at zero contrast,
Posterize `2`, and Threshold `100` as live layers give `[230, 130,
80]`, `[255, 0, 0]`, and white (luma `124.2`), each byte-identical to
the destructive command on the same pixel. An Invert layer at `50%`
opacity over pure red gives `[128, 128, 128]`, and at `0%` leaves the
red. A `[255, 0]` mask confines the inversion to the first pixel, a
`[0, 255]` mask lifts it, hiding the layer lifts it, and clipping it to
a base that is red at one pixel and transparent at the other inverts
only the first and leaves the second transparent. Threshold `0` and
Posterize `1` are refused with no layer added, a Brightness/Contrast
layer at zero is a no-op, re-tuning it to Invert inverts, and
re-tuning a pixel layer or to a bad adjustment errors. All five passed
on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-seven: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The dialog was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1345 Rust tests total** (1340 → 1345, 1338 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 221 — Fill layers

Layer > New Fill Layer becomes a live layer kind rather than a baked
one. A `Fill` is Solid Color with its colour, Gradient with its two
end colours, or Pattern, and a layer carrying a `fill`
(`add_fill_layer(name, fill)`) is an ordinary pixel layer whose pixels
were rendered from that recipe over the whole canvas by a private
`render_fill`: every pixel the colour; the Gradient tool's own
projection onto the top-left-to-bottom-right diagonal and lerp,
carried out op for op as `gradient_fill` does onto a transparent
buffer, so the result is byte-identical to `add_gradient_layer`; or
the defined pattern tiled from the top-left corner exactly as
`add_pattern_layer` tiles it. The selection is deliberately ignored —
a fill layer's content is the fill itself, and Reveal Selection is the
mask step. `set_fill(id, fill)` re-renders the layer from a new recipe,
replacing any paint on it as editing a fill's recipe would, while its
name, opacity, blend mode, mask, link, clip, and lock all survive; a
pixel layer, or a Pattern fill with no pattern defined, is refused
with the layer untouched. The layer view carries `fill`, the panel
shows a badge, and a **Fill Layer…** dialog picks the kind (Solid
Color from the brush colour, Gradient from the brush and gradient-end
colours, Pattern from the defined pattern) and either adds a new layer
or re-renders the selected fill layer through `add_fill_layer` and
`set_fill` commands. The three baked generators stay as they were.
Photoshop's gradient style, angle, and scale, its pattern scale and
Link with Layer, and painting on a fill layer's mask instead of its
pixels are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests, the gradient
bytes first computed in Python emulating `f32`. A `[10, 20, 30]` solid
fill on a 3×2 document fills every pixel, reports its recipe in the
view, and re-tunes to `[1, 2, 3, 128]` everywhere. A red-to-
translucent-blue gradient fill on a 4×2 document is byte-identical to
`add_gradient_layer`, with `[217, 0, 38, 226]` at `(0, 0)` (`t =
0.15`), `[140, 0, 115, 169]` at `(1, 1)`, and `[38, 0, 217, 93]` at
`(3, 1)` (`t = 0.85`), and the baked layer refuses `set_fill`. A
Pattern fill is refused before a pattern is defined, adding nothing,
and a `1 2` tile then repeats `1 2 1 2` across both rows. Re-tuning a
fill at 50% opacity under a Hide All mask keeps its name, opacity, and
mask, composites nothing through the mask, and a Pattern re-tune with
no pattern leaves the pixels alone. A solid fill added under a
one-pixel selection still fills the pixel outside it, a red brush dab
on it paints, and re-tuning restores the fill there. All five passed
on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-eight: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The dialog was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1350 Rust tests total** (1345 → 1350, 1343 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 222 — Apply Image > Add and Subtract

Apply Image's Blending list grows the two arithmetic modes only it and
Calculations offer. An `ApplyBlend` is `Mode { mode }` for any of the
twelve layer blend modes, or `Add { scale, offset }` / `Subtract {
scale, offset }`, and `apply_image_with(target, source, blend, opacity,
invert, preserve_transparency)` is `apply_image` with that full list —
the old signature now delegates to it as `Mode`. Its `blend(cb, cs)` is
the layer mode's own formula, or Photoshop's channel arithmetic in unit
terms: `(target + source) / scale + offset / 255` for Add and `(target
− source) / scale + offset / 255` for Subtract, clamped to `0..=1`,
with Scale in `1.0..=2.0` and Offset in `-255..=255` validated before
any pixel changes. The arithmetic replaces only the blend-mode step, so
alpha, opacity, Invert, and Preserve Transparency compose around it
exactly as before: at full opacity over an opaque target the channel is
the arithmetic result itself. The `apply_image` command now takes an
`ApplyBlend`, and the Apply Image dialog's Blending select lists Add
and Subtract after the twelve modes, showing Scale and Offset fields
for them. Calculations, which shares the two modes, remains unshipped.

**Verified two ways.** Five new `document.rs` tests on a 1×1 target
`[100, 200, 30]` over a source `[50, 100, 240]`, each byte first
computed in Python emulating `f32`. Add at scale `1` gives `[150, 255,
255]` (two channels clamped) and at scale `2` gives `[75, 150, 135]`,
the source untouched. Add at scale `1.5`, offset `−20` gives `[80,
180, 160]`. Subtract gives `[50, 100, 0]`, with offset `128` `[178,
228, 0]`, and at scale `2`, offset `64` `[89, 114, 0]`. Add at `50%`
opacity lands halfway at `[125, 228, 143]` — the exact `142.5`
rounded half away from zero — and over a transparent target the
source shows through unchanged. Scale `0.5`, `2.5`, and `NaN` and
offset `256` and `−256` are each refused with the target untouched,
and `Mode { Multiply }` equals `apply_image` with Multiply, `[20, 78,
28]`. Four of the five passed on the first run: the Add-at-50%
expectation had been computed as `142` by a Python check that applied
banker's rounding to the exact `142.5`; Rust's `round` goes half away
from zero to `143`, and the expectation was corrected — the code was
right.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
sixty-nine: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The dialog was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1355 Rust tests total** (1350 → 1355, 1348 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 223 — Apply Image > Channel

Apply Image gains its Channel list. An `ApplyChannel` is RGB, Red,
Green, Blue, or Transparency, and its `view(pixel)` is how the source
pixel is seen before anything else happens: unchanged for RGB; `[v, v,
v, a]` for one colour channel `v`, the source's own alpha kept, so a
transparent source pixel still applies nothing; and `[a, a, a, 255]`
for Transparency — the alpha as an opaque grey covering the whole
canvas, as Photoshop's Transparency channel is, so a transparent
source pixel applies as opaque black. `apply_image_with` takes the
channel before the blend and views every source pixel through it, after
which Invert, the blend mode or arithmetic, opacity, and Preserve
Transparency compose exactly as before; `apply_image` passes RGB. The
`apply_image` command takes a `channel`, and the dialog gains a Channel
select between Source and Blending. Alpha channels as sources and
applying into a single target channel are documented scope cuts, this
project having no channel model beyond the four bytes of a pixel.

**Verified two ways.** Five new `document.rs` tests, the Multiply
bytes first computed in Python emulating `f32`. On the 1×1 target
`[100, 200, 30]` over the source `[50, 100, 240]`, the Red, Green, and
Blue channels apply as the greys `50`, `100`, and `240`, the source
untouched, and RGB is the source itself. A source at alpha `128`
beside a fully transparent pixel applies through Transparency as `128`
and `0` grey, both opaque, but through Red as the half mix `[75, 125,
40]` and nothing at all. Red inverted applies as `205`. Red through
Multiply gives `[20, 39, 6]` and through Add at scale `2` `[75, 125,
40]`. The merged composite's Green channel is the opaque target's own
`200`, its Transparency opaque white, and with the target hidden the
source's `100`. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and seventy:
this session's Xvfb instance was already confirmed, through a control
test and a full Xvfb-and-application restart in Phase 52, to have
stopped delivering synthetic `xdotool` pointer clicks to the webview
entirely, and re-running that diagnostic again was judged unlikely to
produce new information. The dialog was reviewed by hand instead.
Every other layer of this project's quality bar (hand-verified Rust
tests, `cargo fmt`, `cargo clippy --all-targets -- -D warnings`, `npm
run build`) is fully green.

**1360 Rust tests total** (1355 → 1360, 1353 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 224 — Apply Image > Mask

Apply Image gains its Mask group. An `ApplyMask` names a mask image —
a layer, hidden or not, or with `None` the merged composite — a channel
read from it, and whether to invert, and its `weight(pixel)` is that
channel's byte over 255: Red, Green, or Blue the byte itself,
Transparency the alpha, and RGB the BT.601 luma rounded to a byte,
Photoshop's Gray; `invert` takes `255 − value` first. `apply_image_with`
takes an optional mask after the blend, resolves its pixels once up
front like the source's, and multiplies the weight into the source's
effective opacity at every pixel right after Opacity — before Invert,
the blend, and Preserve Transparency, so all of those compose exactly
as before; `apply_image` passes no mask. An unknown mask layer errors
with the target untouched. The `apply_image` command takes a `mask`,
and the dialog gains a Mask checkbox that reveals Mask Image, Mask
Channel (Gray, Red, Green, Blue, Transparency), and Invert Mask. Alpha
channels as masks and the live Preview remain scope cuts.

**Verified two ways.** Five new `document.rs` tests on the 1×1 target
`[100, 200, 30]` and source `[50, 100, 240]` with a hidden mask layer
`[255, 0, 128, 64]`, every mixed byte first computed in Python
emulating `f32`. Through the mask's Red the source applies whole and
through its Green not at all. Through Blue `128` the half mix is `[75,
150, 135]` and through Transparency `64` the quarter mix `[87, 175,
83]`. Inverted, Green lets everything through, Red nothing, and
Transparency (`191`) gives `[63, 125, 187]`. With the merged image as
the mask — the opaque target itself — its Red `100` at `50%` opacity
gives `[90, 180, 71]`, and through Gray its luma `150.72` rounds to
`151` for `[70, 141, 154]`. An unknown mask layer is refused with the
target untouched, the mask layer is only read, and the old entry point
stays unmasked. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
seventy-one: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The dialog was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1365 Rust tests total** (1360 → 1365, 1358 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 225 — Image > Calculations

Calculations arrives whole, built on Apply Image's types. A `CalcSource`
is a layer (or `None`, the merged composite), a channel, and an invert
flag, and `ApplyChannel::value(pixel)` — new, and now what
`ApplyMask::weight` reads too — is the byte a single channel yields:
Red, Green, or Blue itself, Transparency the alpha, Gray the BT.601
luma rounded. `calculations(source1, source2, blend, opacity, mask,
result)` reads both sources as greys, inverted where asked, blends
Source 1 onto Source 2 with the full `ApplyBlend` list (Source 2 the
base, so Subtract is `Source 2 − Source 1`), then mixes the result
back toward Source 2 by the opacity times the mask's weight — the
order Photoshop's dialog describes. The grey goes where `result` says:
`NewDocument` returns a new one-layer opaque grey document this one's
size, leaving this one untouched (the app opens it in place of the
current document with a fresh history, as Open does — Photoshop opens
a second window, which this single-document app cannot); `NewChannel`
appends an `AlphaChannel` named `Alpha N` to a new `channels` list on
the document, one byte per pixel, listed by name in the view, cleared
with the saved selections when the canvas changes size, and loadable
as a selection through a new `load_channel(name)` (grey `128` and up,
the one-bit reading of Photoshop's partial selection); `Selection`
replaces the selection the same way directly. Every input is validated
before anything changes. A **Calculations…** dialog carries both
sources, the blend with Scale and Offset, Opacity, the Mask group, and
Result; a **Load Channel…** dialog mirrors Load Selection.

**Verified two ways.** Five new `document.rs` tests on a 2×1 document
— layer `a` `[200, 50, 100, 255] [10, 20, 30, 128]` under `b` `[100,
150, 200, 255] [255, 255, 255, 0]` — every grey first computed in
Python emulating `f32`. Normal gives Source 1 itself, `a`'s Red `[200,
10]`; Multiply against `b`'s Green `[150, 255]` gives `[118, 10]`;
Gray reads `a`'s luma, `100.55 → 101` and `18`; the merged image's
Blue is `[200, 30]`, `a` showing through `b`'s transparent pixel; and
the document is untouched. Subtract gives `[0, 245]`, Add at scale `2`
offset `10` `[185, 143]` (the exact `142.5` rounded half away). `a`'s
Red inverted is `[55, 245]`, at `50%` opacity `[103, 250]`, under a
Transparency mask from `a` `[103, 252]`, and with that mask inverted
`[150, 253]`. A New Channel result names `Alpha 1` holding `[200,
10]`, a second `Alpha 2`, the Selection result selects `[true,
false]`, loading `Alpha 1` selects the same, and an unknown channel
errors. Unknown source or mask layers, opacity `101`, and scale `3`
are each refused with no channel added and no selection made. All five
passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
seventy-two: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The dialogs were reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1370 Rust tests total** (1365 → 1370, 1363 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 226 — Color Range's Select list

Select > Color Range grows from one colour to its whole dialog. A
`ColorRange` is Sampled Colors — a list of `ColorSample`s, each a
colour and, when taken off the image, the pixel it came from — with
Fuzziness and optional Localized Color Clusters (a Range in percent
of the canvas diagonal), or one of the presets: Reds, Yellows, Greens,
Cyans, Blues, Magentas, Highlights, Midtones, Shadows, Skin Tones.
`color_range_bits(id, range)` is the read-only judge, one flag per
pixel: a sampled pixel matches when some sample is within Fuzziness
per channel and, localized, when that sample's position is within
Range of it; the hue presets are the 60° sectors centred on 0°, 60°,
… 300° of `rgb_to_hsl`'s hue, a pixel with no hue matching none; the
tone presets are BT.601 luma `<= 65`, `105..=150`, and `>= 190`,
Photoshop's default bands; Skin Tones is the classic RGB rule (R > 95,
G > 40, B > 20, spread > 15, R − G > 15, R > G, R > B). It errors on
an unknown layer, no samples, a Range over 100, or Localized without
positions. `select_color_range_with(id, range, invert)` flips the
flags when asked and replaces the selection, refusing an empty result
— so a no-match range inverted selects everything; the old
`select_color_range` is one sample, not inverted. The dialog gains
the Select list, an Add button and an on-image **Sample on image**
eyedropper building a removable swatch list, Localized Color Clusters
with Range (enabled once every sample carries a position), Invert,
and a Grayscale Selection Preview drawn from `color_range_bits` into a
canvas. Graded partial selection, the matte and Quick Mask previews,
and Detect Faces are documented scope cuts.

**Verified two ways.** Five new `document.rs` tests, the two hue
boundaries first computed in Python emulating `f32`. On a row of pure
red, `(255, 127, 0)` at `29.88°`, `(255, 128, 0)` at `30.12°`,
yellow, green, cyan, blue, magenta, and a grey, each hue preset picks
exactly its sector — the two oranges falling to Reds and Yellows
either side of `30°` — and the grey never. Greys `30, 65, 66, 104,
105, 150, 151, 189, 190, 200` split into Shadows `{30, 65}`, Midtones
`{105, 150}`, and Highlights `{190, 200}`; `[220, 180, 150]` and
`[150, 100, 50]` are skin, while a grey, pure red, and `[100, 120,
80]` are not. Two samples select the union, Fuzziness `10` reaches
`[0, 10, 0]` and `[0, 0, 10]` from `[10, 0, 0]` but not a grey, Invert
flips the mask, and the old entry point still works. On five red pixels the diagonal
is `√26 ≈ 5.10`, so Range `50%` reaches `2.55` pixels — `0..=2` — from
a sample at `x = 0`, `0%` its own pixel, `100%` all, and samples at
both ends at `20%` select the ends and their neighbours. An unknown
layer, no samples, Localized without positions, and Range `101` error;
a no-match preset refuses and leaves the selection, inverted it
selects all. Four of the five passed on the first run: the Fuzziness
`10` expectation had overlooked that `[0, 0, 10]` is also within `10`
of `[10, 0, 0]` in every channel; the code was right and the
expectation was corrected. An existing test also caught the no-match
error's wording having drifted from "No pixels", which was restored.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
seventy-three: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The dialog was reviewed
by hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1375 Rust tests total** (1370 → 1375, 1368 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

## Phase 227 — The Channels panel

The alpha channels Calculations introduced get their panel. A
`ChannelView` is Composite, Red, Green, Blue, or `Alpha { name }`, and
`channel_image(view)` renders what the canvas shows for it: the
flattened composite itself, one of its colour channels as an opaque
grey, or an alpha channel's bytes as an opaque grey. The `composite://`
protocol now reads a `channel=` query — `red`, `green`, `blue`, or
`alpha:<percent-encoded name>` — and serves that image on request,
rendered fresh each time rather than cached, since a view is a look
and not the document; without the query it serves the cached composite
as before. Channels can be managed: `add_channel(name, pixels)` (blank
name → the lowest free `Alpha N`, which Calculations now uses too, so
deleting `Alpha 1` frees its number), `rename_channel` (non-blank,
unique), `move_channel` (one step up or down, a no-op at the ends),
`delete_channel`, and `paint_channel(name, points, radius, grey)`,
which sets every pixel the Selection Brush's hard coverage
(`brush_bits`) touches to `grey`, ignoring the selection. A Channels
panel under the layers lists RGB, Red, Green, Blue, and every alpha
channel with a thumbnail at the chosen size (None, Small, Medium,
Large); clicking a row shows it on the canvas, double-clicking an alpha
row renames it, and its buttons move, load, and delete it; New Channel
adds a black one. With an alpha channel selected, brush strokes paint
its grey — the brush colour's BT.601 luma — through `paint_channel`
instead of the layer. A deleted channel's view falls back to the
composite. Spot channels and channel overlays remain scope cuts.

**Verified two ways.** Five new `document.rs` tests, all exact byte
comparisons. A 2×1 layer `[10, 20, 30, 255] [40, 50, 60, 128]` views
as itself for Composite and as opaque greys `10/40`, `20/50`, `30/60`
for Red, Green, Blue. A channel added with a blank name is `Alpha 1`
and views as `200/10` grey; an unknown name errors; a one-byte channel
on a two-pixel document is refused, a given name is kept, and a
duplicate name errors. Renaming `Alpha 1` to `Hair` works while a
blank, a taken, and an unknown name error; moving `Hair` down swaps
the two, again is a no-op, up swaps back, an unknown name errors;
deleting `Hair` leaves `Alpha 2`, and the next blank names are `Alpha
1` then `Alpha 3`. Painting grey `200` at radius `1` about `(2.5, 0.5)`
on a five-pixel channel sets `[0, 200, 200, 200, 0]`, a second dab of
`90` at radius `0.5` sets the first pixel, an unknown channel and no
points error, loading selects `128` and up, and the layer is
untouched. A painted channel views as its greys while Composite and
`flatten` are unchanged. All five passed on the first run.

Live interactive verification under Xvfb was not attempted this
phase, for the same reason as the previous one hundred and
seventy-four: this session's Xvfb instance was already confirmed,
through a control test and a full Xvfb-and-application restart in
Phase 52, to have stopped delivering synthetic `xdotool` pointer clicks
to the webview entirely, and re-running that diagnostic again was
judged unlikely to produce new information. The panel was reviewed by
hand instead. Every other layer of this project's quality bar
(hand-verified Rust tests, `cargo fmt`, `cargo clippy --all-targets --
-D warnings`, `npm run build`) is fully green.

**1380 Rust tests total** (1375 → 1380, 1373 lib + 7 pipeline). `cargo
fmt`, `clippy`, and `npm run build` all clean.

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
