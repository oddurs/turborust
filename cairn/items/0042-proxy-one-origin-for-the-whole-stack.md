---
id: 42
title: 'Proxy: one origin for the whole stack'
type: feature
status: done
milestone: v1.1
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p0
effort: l
area: serve
---

## Problem

Our own demo is the evidence: a page served on `:8789` does
`fetch("http://127.0.0.1:8788/api/hello")`. That is cross-origin, so a real app
needs CORS headers in the backend that exist only for development, or a hardcoded
API origin that has to change for production. Both are the thing a dev server is
supposed to remove.

trunk and cargo-leptos both ship a proxy (`proxy_backend`, `proxy_rewrite`,
`proxy_ws`). For a tool that pitches itself at full-Rust stacks, not having one is
a hole in the core promise.

## Design

```toml
[services.web]
serve = { dir = "dist", port = 8789 }
proxy = [
  { path = "/api", to = "http://127.0.0.1:8788" },
  { path = "/ws",  to = "http://127.0.0.1:8788", websocket = true },
  { path = "/v2",  to = "http://127.0.0.1:8788", strip_prefix = true },
]
```

Rules are checked before the static handler, longest prefix first, so `/api/deep`
beats `/api`. `strip_prefix` removes the matched prefix before forwarding, which
is what people expect from `/api` fronting a backend that does not know it is
mounted under one.

Implementation: axum already brings hyper, so use `hyper-util`'s client rather
than adding a second HTTP stack. Forward method, path, query, headers and body;
strip hop-by-hop headers (`Connection`, `Keep-Alive`, `Transfer-Encoding`,
`Upgrade`, `Proxy-*`) rather than blindly copying, and stream the response body
instead of buffering — an SSE endpoint behind the proxy must still stream.

WebSocket needs the upgrade dance handled explicitly: accept the client upgrade,
dial the backend, and copy frames both ways until either side closes.

**The interesting case is the backend being down.** During a restart, the target
refuses connections for a second or two. Returning a bare connection error makes
the app look broken when it is merely rebuilding. Instead: retry briefly, and if
it is still down return a 502 whose body the overlay can recognise, so the page
says "backend restarting" rather than showing a failed fetch. The supervisor knows
the backend's status; the proxy should use it rather than guessing.

## Acceptance criteria

- [x] Prefix rules forward method, headers, query and body
- [x] Longest prefix wins
- [x] `strip_prefix` works
- [x] Hop-by-hop headers are dropped, not forwarded
- [x] Response bodies stream (an SSE endpoint behind the proxy still streams)
- [x] WebSocket upgrades are proxied
- [x] A restarting backend yields a recognisable 502, not a raw connection error
- [x] The demo drops its cross-origin fetch and uses a relative path

## 2026-09-08

Not blocked on 0041. The proxy needs to know when the backend is restarting, and that is already in AppState — the supervisor knows each node status. The control socket is unrelated.

## 2026-09-08

Proxy rules on a serve node, checked before the static handler.

Three details that were worth getting right rather than assuming:

1. Prefixes match on SEGMENT boundaries, not raw string prefixes. /api must not swallow /apidocs. That has its own test because it is the classic proxy bug and it fails silently — requests vanish into a backend that has never heard of them.

2. The fallback route became `any` rather than `get`. A proxied POST has to reach the backend; with `get` it would have 405d, and the failure would have looked like the backend rejecting the method. Static files stay GET-only and say so.

3. The response is rebuilt from parts rather than collected. Buffering would have broken SSE behind the proxy, which is exactly what our own overlay uses.

Hop-by-hop headers are dropped by name — Connection, Transfer-Encoding, Upgrade and the rest describe one connection and forwarding them onto another produces failures that look like the backend misbehaving. A test asserts Authorization survives, since over-stripping would be just as bad.

The unreachable case is labelled, not merely failed: 502 with x-turborust-proxy: upstream-unreachable and Retry-After: 1, so the overlay can say "backend restarting" instead of showing a failed fetch. Verified against a rule pointing at a dead port.

The demo now uses a relative /api/hello path and its cross-origin fetch is gone — which was the evidence the item was filed on.

Not blocked on 0041 after all: the note on this item already recorded that the supervisor state was the source of truth for restart status, and no control socket was needed.
