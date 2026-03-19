# databeam

databeam is a tiny, peer-to-peer file transfer app you can run yourself. It gives you a 6-digit code to share with someone else; once they enter it, your browsers connect directly and files download automatically. Your data never passes through the server.

## What you get

A clean two-panel web interface and a lightweight signaling worker. The backend only handles WebRTC signaling (`/pin` and `/ws/{pin}`); file bytes move directly between devices over WebRTC data channels.

## Backend stack

The signaling worker is now implemented in Go with:

- lock-free atomic bitmap PIN allocation (1,000,000 PIN space)
- per-room event loops for low-contention fan-out
- websocket signaling via `gorilla/websocket`
- automatic idle room expiry to avoid stale PIN leaks

## Run locally

```bash
go run ./cmd/databeam
```

Open http://localhost:8080 in two browsers or devices on the same network.

Optional logging control:

```bash
LOG_LEVEL=debug go run ./cmd/databeam
```

## Build locally

```bash
go build -o databeam ./cmd/databeam
./databeam
```

## Docker build

```bash
docker build -t databeam .
docker run --rm -p 8080:8080 databeam
```

NOTE: You can also use the deployment helper in `scripts/deploy.sh`.

## Things to know before you deploy

WebRTC works best over HTTPS/WSS outside localhost; for public hosting you should put this behind a reverse proxy with TLS. Some networks also need a TURN server for reliable connectivity; databeam ships with a public STUN server only. The 6-digit code is convenience, not authentication.

**Copyright (c) 2026 hvrsim. Released under the MIT License.**
