# databeam

databeam is a tiny, peer‑to‑peer file transfer app you can run yourself. It gives you a 6‑digit code to share with someone else; once they enter it, your browsers connect directly and the files download automatically. Your data never passes through a server.

## What you get

A clean two‑panel web interface and a lightweight server that only handles signaling. The server is there so the two browsers can find each other; actual file bytes move directly between devices over WebRTC.

## Deploying Databeam

### Local build

```bash
cargo run
```

Open http://localhost:8080 in two browsers or devices on the same network.

### Docker build

```bash
docker build -t databeam .
docker run --rm -p 8080:8080 databeam
```

**NOTE: I recommend you use the dedicated deployment script in `scripts/deploy.sh` instead of invoking docker directly**

## Things to know before you deploy

WebRTC works best over HTTPS/WSS outside of localhost; for public hosting you should put this behind a reverse proxy with TLS. Some networks also need a TURN server for reliable connectivity; databeam ships with a public STUN server only. The 6‑digit code is convenience, not authentication.

Copyright (c) 2026 hvrsim. Released under the MIT License.
