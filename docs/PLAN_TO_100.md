# The plan to 100%

The standing order: every row of `PHOTOSHOP_PARITY.md`, every documented
scope cut inside a checked row reopened and built, Photoshop's own pain
points solved, a better UI — with no further questions asked. This file
is the A–Z that makes that possible: what "100%" means as a finite,
checkable list; the constraints that shape how each part is built; and
the order of execution. Progress is recorded phase by phase in
`README.md`, as always; this document is the map, not the log.

## A. What changed, and what did not

- **Backend hosting is now allowed** for the rows that need it: the
  seven generative-AI rows and the seven cloud/collaboration rows. A
  real service, in this repository, that the desktop app talks to.
- **Smart Portrait and Makeup Transfer are tabled** (not declined,
  not built now). The four Neural Filters panel-category rows are gated
  on them and flip when they do.
- **Still true, and shaping everything below:** this sandbox is
  CPU-only; pretrained weights on Hugging Face/Kaggle/Google Drive and
  most GitHub Releases are unreachable; loading a downloaded pickle
  checkpoint is blocked. Every model in this project is trained here,
  from scratch or from this project's own earlier weights, and says so.

## B. What 100% means (finite, checkable)

1. **The 22 open rows**, minus the 2 tabled and the 4 headers gated on
   them = **16 rows to build now**, each with a real path (section C).
2. **Every documented scope cut reopened.** 172 checked rows carry a
   "documented scope cut" — a named sub-feature Photoshop has and this
   app deliberately skipped. Each is a real, enumerable work item.
   `grep -n "scope cut" docs/PHOTOSHOP_PARITY.md` is the backlog; it
   shrinks as each is built and the row's own text updated.
3. **Photoshop's pain points, solved in this app** — researched, not
   assumed (Adobe community threads and 2025–26 reviews): performance on
   large files and heavy brushes; crashes and lost work (no crash-safe
   autosave/recovery); the Actions panel regressing and actions
   silently clearing; AI features that stall the whole app; feature
   bloat and UI clutter with no way to tune it down. Each maps to a
   concrete deliverable in section C, phases G and H.
4. **A better UI** — a real menu bar organised the way Photoshop's is
   (File/Edit/Image/Layer/Type/Select/Filter/View/Window/Help) driving
   the commands that already exist; saveable workspaces; an options bar;
   a shortcut editor; a consistent theme; an onboarding tour; and the
   736 KB single JS bundle split so first paint is fast.

Honesty that stays in every deliverable: a generative model trained
here on 787 landscape photographs at 64×64 native resolution is a real
text-conditioned image generator and is not Firefly. Every open
equivalent of an Adobe-proprietary service (Firefly Boards, Creative
Cloud Libraries, Adobe Fonts) is a real service with the same job, not
Adobe's. Each row's checklist text says exactly which.

## C. Execution order

Each phase ends the same way every phase in this project has: real
tests, `cargo fmt`/`clippy`/`test` and `npm run build` clean, a README
phase write-up with the numbers, checklist rows updated, commit, push,
CI green.

**Phase C1 — Text-conditioned generative model (the long pole; starts
first, trains for days, checkpointed and resumable).**
`src-tauri/models/train_generate/`: a class- and caption-conditional
DDPM (Ho et al. 2020; classifier-free guidance, Ho & Salimans 2022) at
64×64 on the 787 landscape photographs, conditioned on their 7 real
categories and a bag-of-words over their 815 real Flickr titles. EMA
weights, cosine schedule, DDIM sampling. ~60k steps. Serves, in order:
- **Generate Image** — prompt → 64×64 → this project's own Super Zoom
  ×3 → 192×192 result as a new layer (on-device via `tract` if the UNet
  lowers; otherwise served by the backend, section C2).
- **Reference Images** — SDEdit (Meng et al. 2021): noise the reference
  partway and denoise under the prompt.
- **Prompt to Edit** — SDEdit over the selection under the prompt.
- **Generative Upscale** — SDEdit at low noise over the ×3 upscale, so
  the model adds plausible detail rather than interpolating.
- **Generative Layers** — a layer that remembers prompt, seed, and
  method and regenerates on demand, the way smart objects re-render.
- **AI Model Picker** — a real choice between real models: the
  on-device generative-fill model, the diffusion model, an external
  endpoint.
- **AI Assisted Editor** — the backend calling a real LLM (Anthropic's
  API, reachable from here, with the user's own key) with this app's
  command list as tools; a rule-based fallback when no key is set.

**Phase C2 — The backend service** (`server/`, Rust/axum, in this
repo): the contracts the client already speaks (`PUT/GET
/documents/<name>`, `GET /documents`, the generative `POST {prompt,
width, height}`), plus sharing links with per-user permissions (Invite
to Edit), review links with threaded comments (Share for Review), an
asset library (Creative Cloud Libraries' open equivalent), a font
catalogue served from open-licensed fonts (Adobe Fonts' open
equivalent; Google Fonts CSS is reachable from here), shared boards
(Firefly Boards' open equivalent), and cloud-side Select Subject
(Select Subject — Cloud Processing: the detector run on the server at a
heavier setting). Photoshop Cloud Documents and Search Your Cloud Files
flip the moment the store exists.

**Phase C3 — Refine Hair**: a decontaminating edge-refinement pass over
an existing selection's border (colour decontamination along the edge,
Refine Edge's own Decontaminate Colors), classical, on-device.

**Phase D — Reopen the scope cuts**, largest-impact first: transform
handles on canvas; a real scalable font for the Type tools; brush
dynamics; the remaining Filter Gallery options; Content-Aware Fill's
patch synthesis; layer styles' remaining options; the Blur Gallery's
interactive controls; the rest, row by row, until `grep "scope cut"`
returns only rows that name Adobe-proprietary services.

**Phase E — Neural Filters headers**: flip with the tabled pair.

**Phase F — Tabled**: Smart Portrait, Makeup Transfer — revisited when
the user says so, not before.

**Phase G — Pain points**: crash-safe autosave and recovery on launch;
a real progress-and-cancel path for every long operation (AI, filters,
export) so the UI never stalls; Actions that pause for input and never
clear; large-file performance (tiled recomposite, dirty-rect everywhere,
worker threads for filters); a "reduce clutter" workspace that hides
what a user doesn't use.

**Phase H — UI**: the menu bar; workspaces; the options bar; shortcut
editor; theme; onboarding; bundle splitting; live Playwright
verification of every dialog once the Xvfb path is repaired (the
documented gap) — repairing it is itself an item here.

## D. Long-running work

Training runs in the background with atomic checkpoints and a
`--resume` flag; a run that outlives a session is continued in the
next. Nothing in a training directory but the scripts and attributions
is committed; the exported ONNX is the artifact.

## E. Done means

Every row checked except the tabled pair and their four headers; the
scope-cut grep down to Adobe-proprietary services only; every item in
sections B.3 and B.4 shipped and verified; CI green on the head commit.
