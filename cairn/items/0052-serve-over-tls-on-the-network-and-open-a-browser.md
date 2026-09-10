---
id: 52
title: Serve over TLS, on the network, and open a browser
type: feature
status: done
milestone: later
assignee: Oddur Sigurdsson
created: 2026-09-08
updated: 2026-09-08
priority: p3
effort: m
area: serve
---

## Problem

The static server binds loopback, plain HTTP, and does not open anything. Three
consequences:

- Testing on a phone is impossible without a separate tunnel
- Browser APIs gated on a secure context (service workers, camera, geolocation,
  WebAuthn) cannot be exercised at all
- Starting a dev server and then hunting for the URL is a papercut everyone else
  fixed years ago

trunk has `address`, `tls_key_path`, `tls_cert_path` and `open`.

## Proposal

```toml
[services.web]
serve = { dir = "dist", port = 8789, host = "0.0.0.0", open = true }
tls = { cert = "cert.pem", key = "key.pem" }
```

Binding beyond loopback changes the threat model, so it must be explicit and
loud: the overlay's restart endpoint and the control socket must not become
reachable from the network because someone wanted to test on a phone. Same-origin
alone is not sufficient once the origin is routable — non-loopback binding should
disable the mutating endpoints unless separately opted into.

That constraint is the reason this is `later` rather than a quick win.

## Acceptance criteria

- [x] `host` binds beyond loopback and prints the LAN URL
- [x] TLS with a supplied cert and key
- [x] `open` launches a browser once the server is ready, not before
- [x] Non-loopback binding disables mutating endpoints unless explicitly allowed

## 2026-09-08

host, tls and open on the serve block — and the security constraint the item flagged as the reason this was `later`, which turned out to be the most interesting part.

`allow_remote_control` is deliberately SEPARATE from `host`. Same-origin is sufficient protection on loopback because the origin is unreachable; the moment it is routable, every page on the network shares it and the header proves nothing. So binding beyond loopback closes the restart endpoint by default, with an error that names the setting to change. Wanting to test on a phone is not consent to let the network restart your processes, and collapsing the two into one flag would have made it exactly that.

Verified all three states: bound to 0.0.0.0 the restart endpoint 403s with the explanation, with allow_remote_control it 202s, and TLS serves https while plain http on the same port fails to connect.

`open` fires after the bind rather than before — opening a browser at a URL that is not listening yet produces an error page the user has to reload, which is worse than not opening at all. 0.0.0.0 is rewritten to localhost for the URL, since nobody can visit 0.0.0.0.

Certificate and key are resolved relative to the workspace rather than the process working directory, so `turborust up -c path/to/config` behaves the same as running from that directory.
