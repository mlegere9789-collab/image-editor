# image-editor-server

The self-hosted backend behind the desktop app's cloud rows: **Cloud
Documents**, **Search Your Cloud Files**, **Invite to Edit**, **Share
for Review** and **Libraries**. One Rust binary (axum), one data directory, no database.

```bash
cd server
cargo run --release -- --data-dir ~/image-editor-data --listen 127.0.0.1:8787
```

On first start it prints an **admin token** (also written to
`<data-dir>/admin.token`) and a token for a first user, `owner`. Tokens
are shown once; only their SHA-256 hashes are stored. Paste the URL and
the user token into **Edit > External Services** in the app.

Create more users with the admin token:

```bash
curl -X POST -H "Authorization: Bearer $ADMIN" -H "Content-Type: application/json" \
     -d '{"name":"ana"}' http://127.0.0.1:8787/users
```

## Storage

```
<data-dir>/
  admin.token        the admin token, for the operator
  index.json         users (token hashes), documents, shares, reviews -- rewritten atomically
  blobs/<id>/<n>     every saved version of every document, never overwritten
  blobs/lib-<id>/<n> a library's graphic assets
```

## HTTP contract

All bodies are JSON unless noted; refusals are `{ "error": "..." }`.
User routes take `Authorization: Bearer <user token>`; admin routes the
admin token; review-link routes need no token (the link is the secret).

| Method | Path | Who | What |
|---|---|---|---|
| GET | `/health` | anyone | `{ ok, service, version }` |
| GET | `/me` | user | `{ user }` |
| GET | `/users` | admin | `{ users: [name] }` |
| POST | `/users` `{ name }` | admin | `{ user, token }` (201) |
| POST | `/users/{name}/token` | admin | `{ user, token }` -- rotates, old token stops working |
| GET | `/documents` | user | `{ documents: [name], details: [{ name, owner, access, version, saved_at, saved_by, bytes }] }` -- own documents by bare name, shared ones as `owner/name` |
| PUT | `/documents/{name}` (octet-stream, ≤ 64 MB) | owner or editor | `{ name, version }` -- a new version every time; a new bare name creates the document |
| GET | `/documents/{name}` | anyone with access | the latest version's bytes |
| DELETE | `/documents/{name}` | owner | 204; every version, share and review link goes with it |
| GET | `/documents/{name}/versions` | anyone with access | `{ versions: [{ version, saved_at, saved_by, bytes }] }` |
| GET | `/documents/{name}/versions/{n}` | anyone with access | that version's bytes |
| GET | `/documents/{name}/shares` | owner | `{ shares: [{ user, role }] }` |
| PUT | `/documents/{name}/shares/{user}` `{ role: "edit" \| "view" }` | owner | invite (or change the role) |
| DELETE | `/documents/{name}/shares/{user}` | owner | 204 |
| POST | `/documents/{name}/reviews` `{ title? }` | owner or editor | `{ review, path }` (201) -- a link pinned to the current version |
| GET | `/documents/{name}/reviews` | owner or editor | `{ reviews: [review] }` |
| GET | `/reviews/{id}` | link holder | `{ review: { id, document, version, title, created_by, created_at, comments } }` |
| GET | `/reviews/{id}/document` | link holder | the pinned version's bytes |
| POST | `/reviews/{id}/comments` `{ author, text, x?, y?, parent? }` | link holder | `{ comment }` (201); `x`/`y` pin a point as fractions 0..1; `parent` replies to a thread's first comment |
| PUT | `/reviews/{id}/comments/{n}/resolved` `{ resolved }` | owner or editor | `{ comment }` |
| GET | `/libraries` | user | `{ libraries: [{ id, name, owner, access, assets }] }` |
| POST | `/libraries` `{ name }` | user | `{ library }` (201) |
| DELETE | `/libraries/{id}` | owner | 204, assets and blobs included |
| GET | `/libraries/{id}/shares` | owner | `{ shares: [{ user, role }] }` |
| PUT | `/libraries/{id}/shares/{user}` `{ role }` | owner | share (or change the role) |
| DELETE | `/libraries/{id}/shares/{user}` | owner | 204 |
| GET | `/libraries/{id}/assets` | anyone with access | `{ assets: [{ id, name, kind, data, bytes, added_by, added_at }] }` |
| POST | `/libraries/{id}/assets` `{ name, kind, data }` | owner or editor | `{ asset }` (201); `kind` is `color`, `gradient` or `adjustment`; a name already used within that kind is replaced |
| PUT | `/libraries/{id}/graphics/{name}` (octet-stream) | owner or editor | `{ asset }` (201) -- a `graphic` asset, its PNG bytes |
| GET | `/libraries/{id}/assets/{n}/blob` | anyone with access | a graphic's bytes |
| DELETE | `/libraries/{id}/assets/{n}` | owner or editor | 204 |

Document names: 1-200 characters, no `/`. User names: 1-64 of
`[A-Za-z0-9._-]`. A document the caller has no access to reads as
`404`, not `403`, so its existence is not revealed. CORS allows any
origin, since the desktop app's webview calls from its own
`tauri://localhost` origin.

## Tests

`cargo test` covers the store's rules (versions, roles, review links,
reopen from disk, name rules) and the HTTP contract end to end through
the router, including CORS preflight.
