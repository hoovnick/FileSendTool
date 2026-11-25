# FST — lessons

## cloudflared silently inherits `~/.cloudflared/config.yml`

Cost the most time of anything here. Quick tunnels came up clean — hostname
issued, `Registered tunnel connection` logged, DNS resolving — and every single
request still returned an **empty 404 from Cloudflare's edge**.

The cause was not in this code. If a `config.yml` already exists for an
unrelated *named* tunnel, it will typically end with a catch-all ingress rule:

```yaml
ingress:
  - hostname: something.example.com
    service: http://localhost:3000
  - service: http_status:404
```

cloudflared auto-loads that file **even when started with `--url` for an
account-less quick tunnel**, and those `ingress` rules then govern the quick
tunnel too. Every hostname that does not match the named rule falls through to
`http_status:404`.

The tell is in cloudflared's own startup log: a `cred-file:` entry appearing in
the `Settings: map[...]` line, which should never be there for a quick tunnel.

Fix: FST writes its own empty config under `%LOCALAPPDATA%\FST\` and passes
`--config` so only `--url` decides routing. Any existing user config is read-only
as far as FST is concerned — it is never edited or moved. See
`write_isolated_config()` in `src/tunnel.rs`.

## eframe 0.37 split `App::update` into `logic` and `ui`

`update()` no longer exists. `logic()` runs before every `ui()` **and** whenever
`request_repaint()` is called with the window hidden; `ui()` is skipped entirely
while hidden. For a tray app this is exactly the hook you want — tray menu
handling and close-interception go in `logic()`, and Exit keeps working with
nothing on screen. There is also `ViewportCommand::RequestPaintWhileHidden` if a
full pass is genuinely needed while hidden; it wasn't here.

Verified directly: sending a second `WM_CLOSE` while hidden was still
intercepted, which proves `logic()` is running.

## axum 0.8 `get()` already routes HEAD

`get(handler)` registers `GET` **and** `HEAD`. Adding `.head(handler)` panics at
startup with an overlapping-method-route error. Also, axum 0.8 path params are
`/{token}`, not the old `/:token`.

## Holding a lock guard across `.await` breaks axum handlers

`std::sync::RwLockReadGuard` is not `Send`, so any `return gone().await` taken
while the share-table guard was alive made the whole future non-`Send` and the
handler stopped satisfying `Handler`. The error surfaces as an inscrutable
"trait bound `fn(...) {landing}: Handler<_, _>` is not satisfied" pointing at the
`.route(...)` line, not at the actual await. Fix was making the 404 a plain
synchronous function.

## A GPU-backed window costs ~240 MB resident even when hidden

eframe's glow backend on the NVIDIA driver sits at ~241 MB working set. For
something that lives in the tray that is nearly all dead weight.
`SetProcessWorkingSetSize(GetCurrentProcess(), -1, -1)` on hide drops it to
~11 MB; the pages fault back in when the window is shown. Measured, not assumed.

## Download caps count requests, not people

A resumed transfer, a double-tapped button, or a browser that opens and retries
each arrive as separate requests. Counting them all would lock a recipient out
mid-file. FST counts only full, non-`HEAD`, non-`Range` GETs, and the cap is off
by default with expiry doing the real work.

## A force-killed parent orphans cloudflared

Demonstrated: `Stop-Process -Force` on the app left cloudflared running. Not a
data leak — the origin dies with the app, so the tunnel serves 502 — but a stray
process holding a public ingress is bad hygiene. Fixed with a job object carrying
`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, verified by force-killing again.

## Share the landing page, never the byte URL

Discord, iMessage and Slack prefetch pasted URLs to build previews. If the shared
link were the stream, every paste would start a multi-GB transfer and spend a
download. `/d/{token}` is HTML (~1.6 KB to an unfurl bot); `/f/{token}/{name}` is
only reached by a human click.

## Shipping cloudflared alongside the app

cloudflared is Apache 2.0, so redistributing the unmodified binary is fine with
the licence text and an attribution note (`dist/licenses/`). Bundling also pins a
known-good version instead of inheriting whatever is on PATH.

Resolution order in `cloudflared_program()` is: next to `fst.exe`, then
`vendor\cloudflared.exe`, then the bare name so the OS searches PATH. Both ends
were tested — the bundled copy wins when present, and a lone `fst.exe` in an
empty folder still falls back to the system install.

Cost is honest: 14 MB becomes 80 MB, almost all of it cloudflared.
