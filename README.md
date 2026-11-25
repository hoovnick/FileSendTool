# FileSendTool (FST)

Send a file of any size straight off your PC. No upload wait, no cloud account,
no permission grants — and nothing for the recipient to install.

Pick a file, get an HTTPS link, send it. They click it and the file streams off
your drive, in whatever browser they already have, on a phone or a desktop.

![FST tray app](assets/icon.png)

## Why

Discord caps attachments at 20 MB. Email at ~25 MB. So the routine becomes:
upload to a cloud drive, wait out the upload, fix the sharing permissions, copy
the link, paste it. Four steps to move bytes that are already sitting on your
disk.

FST skips all of it. The file never moves and never gets copied anywhere.

## How it works

1. A tiny HTTP server binds to `127.0.0.1:0` — **loopback only**, so the open
   internet can never reach it directly.
2. `cloudflared` opens an outbound-only [quick tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/do-more-with-tunnels/trycloudflare/)
   and returns a public `https://….trycloudflare.com` hostname. No Cloudflare
   account, no inbound firewall port, no router configuration.
3. Each share gets a 32-character random token. The link you send is a small
   landing page; the bytes live behind a second URL.
4. Close the window and it drops to the tray and keeps serving. Exit from the
   tray menu and every link dies with it.

## Install

Grab the latest release, unzip, run `fst.exe`. It bundles `cloudflared`, so
there is nothing else to install and no account to create.

Or build it:

```powershell
cargo build --release
.\package.ps1          # assembles dist\ with cloudflared and licences
```

Requires a Rust toolchain and, if you are packaging, a `cloudflared` binary on
PATH (or pass `-Cloudflared <path>`).

## Security

- **Unguessable links.** 32 alphanumeric characters from the OS CSPRNG.
- **Expiry by default**, 24 hours, configurable per share down to an hour.
- **Optional download cap**, off by default.
- **Revoke** any share instantly.
- **Uniform 404s.** Unknown, expired, revoked and exhausted tokens all return a
  byte-identical page, so probing cannot tell a real token from a dead one.
- **No path ever comes from the URL.** The share table holds a resolved absolute
  path and the URL carries only an opaque token, so traversal is not something
  that has to be filtered out — it is not expressible.
- **Loopback-only origin.** If the tunnel is down, nothing is reachable.
- **Nothing outlives the app.** cloudflared runs in a job object and is killed
  even if FST crashes or is force-killed.

Worth knowing: **this is not end-to-end encrypted.** TLS terminates at
Cloudflare's edge, so Cloudflare can see the file in transit. The trade buys
"works on any phone with no install". Also, the link *is* the credential —
anyone who has it can download, so treat it like a password.

## Costs

Measured on an i7-6700 while serving a real 3 GB file:

| State | FST RAM | cloudflared RAM | CPU |
|---|---|---|---|
| Hidden in tray, idle | 13 MB | 34 MB | ~0% |
| Window open, idle | 241 MB | 34 MB | 0.6% of one core |
| Serving at 379 Mbps | 8 MB | 45 MB | 16% + 108% of one core |

Memory does not track file size — a 3 GB transfer peaks the server at 8 MB,
because the file is streamed rather than buffered. cloudflared, not FST, is the
CPU cost during a transfer; it does the QUIC/TLS work out to the edge.

## Limits

- The link works only while the app is running and the PC is awake. That is the
  trade for zero upload wait.
- Recipient download speed is capped by your **upload** bandwidth.
- Quick-tunnel hostnames are random, carry no uptime guarantee, and change when
  the app restarts — so links do not survive a restart. A named tunnel on your
  own domain fixes that; `src/tunnel.rs` is shaped so it is a drop-in.
- Single files only. Folders and multi-file sends are not implemented.

## Built with

Rust — [`eframe`/`egui`](https://github.com/emilk/egui) for the window,
[`tray-icon`](https://github.com/tauri-apps/tray-icon),
[`axum`](https://github.com/tokio-rs/axum) and
[`tower-http`](https://github.com/tower-rs/tower-http) for the server, and
[`cloudflared`](https://github.com/cloudflare/cloudflared) for the tunnel.

`tower-http`'s `ServeFile` handles HTTP Range properly, which is what makes a
14 GB download resumable after a phone drops off Wi-Fi.

See [lessons.md](lessons.md) for the things that were not obvious, and
[tasks.md](tasks.md) for what is verified and what is not.

## Licence

MIT — see [LICENSE](LICENSE). Bundled `cloudflared` is Apache 2.0; attribution
and licence text ship in `dist/licenses/`.
