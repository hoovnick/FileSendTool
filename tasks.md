# FST — tasks

## Done

- [x] Cargo project, Rust 1.96 / MSVC, release profile (`opt-level = 3`, LTO, strip)
- [x] `share.rs` — share table, 32-char CSPRNG tokens, expiry / cap / revoke status
- [x] `server.rs` — axum on `127.0.0.1:0`, landing page + byte stream, Range via
      `tower-http::ServeFile`, identical 404 for unknown/expired/exhausted/revoked
- [x] `tunnel.rs` — cloudflared supervisor, hostname scrape, restart watchdog,
      isolated config, job object so the child can't outlive the app
- [x] `app.rs` — egui window, drag-and-drop, expiry picker, download cap, share
      list with copy / open / QR / revoke, activity log
- [x] Tray icon with Show / Exit, hide on X and on minimise, keeps serving hidden
- [x] Working-set trim on hide (241 MB → 11 MB)
- [x] `examples/serve_once.rs` — headless harness used for verification
- [x] Icon + version info compiled into the .exe (`build.rs` + winresource)
- [x] Desktop and Start menu shortcuts (point at `distst.exe`)
- [x] Bundled cloudflared: resolves beside the exe, then `vendor\`, then PATH
- [x] `package.ps1` builds `dist\` — fst.exe + cloudflared.exe + licences, 79.6 MB
- [x] Verification: see "Verified" below

## Verified

| Check | Result |
|---|---|
| 3 GB integrity over loopback | SHA-256 match, 366 MB/s |
| Server memory during 3 GB transfer | peak 8 MB — does not track file size |
| Resume after interruption | killed at 167 MB, `curl -C -` → 206, final hash matches |
| Mid-file range, loopback and public | byte-exact |
| 3 GB `HEAD` through Cloudflare | correct `Content-Length` / `Accept-Ranges` |
| Full download through Cloudflare | SHA-256 match |
| Public throughput | 409 Mbps up from this PC |
| 404 parity (unknown / revoked / expired / bad path) | all 404, identical 1265 bytes |
| HEAD and Range do not spend the cap | cap 2 survived 1 HEAD + 1 Range, then 2 GETs, 3rd → 404 |
| Cap through the public URL | 3 × 200 then 404 |
| Expiry | 200 before, 404 after, both endpoints |
| Landing page on a phone | 375×812 screenshot, no JS |
| X button hides to tray, keeps serving | process alive, window hidden, still answering |
| `App::logic()` runs while hidden | second WM_CLOSE still intercepted — tray Exit will be handled |
| Idle cost hidden in tray | 11 MB, 0% CPU over 10 s |
| No orphan after a force-kill | job object killed cloudflared |
| Bundled cloudflared preferred over PATH | child was `dist\cloudflared.exe`, not system32 |
| PATH fallback still works | fst.exe alone in a temp dir used the system32 copy |
| Public link via the bundled binary | landing 200, download SHA-256 match |

## Resource cost (measured on this box: i7-6700, 8 logical cores, 64 GB)

| State | fst.exe RAM (resident) | cloudflared RAM | CPU |
|---|---|---|---|
| Window open, idle | 241 MB | 34 MB | 0.6% of one core |
| **Hidden in tray, idle** | **13 MB** | 34 MB | ~0% / 0.2% |
| Serving 512 MB at 379 Mbps | 8 MB | 45 MB | 16% + 108% of one core |

Caveat on the hidden figure: the trim releases *resident* pages, which is what
Task Manager's Memory column and real RAM pressure track. Committed private
bytes stay ~406 MB — that is the GPU backend's allocation and it does not shrink.
On 64 GB it is irrelevant; worth knowing it is there.

cloudflared, not FST, is the CPU cost during a transfer: it does the QUIC/TLS
work out to Cloudflare's edge, roughly one core saturated at ~380 Mbps. Scales
with transfer speed, idles at nothing.

## Not covered by automated checks

- [ ] Clicking the tray icon restores the window
- [ ] Tray **Exit** quits (the code path is proven reachable; only the click
      itself is unverified)
- [ ] A real recipient on another network opening a link on a phone

## Not built (deliberate)

- Folder / multi-file sends (would stream a zip built on the fly)
- Inbound drop-box so people can send *you* files
- Launch at login
- Per-share PIN
- Named tunnel for stable `files.yourdomain.com` links that survive restarts —
  `tunnel.rs` is shaped so this is a drop-in; it needs a Cloudflare account and
  a DNS record, so it is mostly configuration rather than code
