# Security policy

## Reporting a vulnerability

Please report privately through GitHub's
[private vulnerability reporting](https://github.com/oddurs/turborust/security/advisories/new)
rather than opening a public issue.

You should get an acknowledgement within a week. If a fix is warranted, expect a
release within thirty days of confirmation, and credit in the release notes
unless you would rather not be named.

## Scope

turborust runs the commands a project configures, serves files, proxies
requests, and replays cached build outputs. Things worth reporting:

- **The static server or the proxy** serving anything outside the configured
  directory or upstream, or being reachable beyond `127.0.0.1` without `host`
  having been set in the `serve` block.
- **The overlay's restart control** acting on a request from beyond loopback
  without `allow_remote_control = true`. It is meant to answer 403.
- **The cache** restoring a file outside the workspace, or replaying a result
  under a key it does not match — including from a shared cache with
  `push = false`, which is meant to be read-only.
- **The control socket** used by `connect` being reachable by another user. It
  is created owner-only (`0700` directory, `0600` socket); anything that gets
  around that is in scope.
- **The overlay** injecting into, or reading from, pages other than those
  turborust itself serves.
- A crafted `turborust.toml` causing memory unsafety, or running something other
  than the commands it declares.

Out of scope, because they are how the tool is designed to work:

- **Configured commands run.** A `turborust.toml` is a build script: opening an
  untrusted project with turborust is equivalent to running its `Makefile`.
- **A shared cache you read from is trusted.** Anyone who can write to it can
  hand your build arbitrary outputs; that is the reason `push` defaults to
  false, and the README's "Sharing a cache" section says so plainly.
- **`serve = { host = "0.0.0.0" }` exposes the dev server** to the network it
  names. That is what it is for.

## Supported versions

Until 1.0, only the latest release is supported.

## Verifying a release

Every archive on a GitHub release is listed in `SHA256SUMS`, and carries a
build-provenance attestation saying which workflow, at which commit, produced
it:

```sh
sha256sum --check --ignore-missing SHA256SUMS
gh attestation verify turborust-<version>-<target>.tar.gz --repo oddurs/turborust
```

The checksums prove a download arrived intact. Only the attestation says where
it came from, because the checksums are produced by the same workflow as the
archives.
