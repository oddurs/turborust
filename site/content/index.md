---
title: "turborust — a dev orchestrator for full-Rust stacks"
description: "One binary that supervises your services, watches the right files, caches your builds, and tells you exactly why anything re-ran."
---

<section class="hero">
  <div class="hero-inner">
    <p class="eyebrow">low tide · one binary</p>
    <h1>Your Rust stack, <em>watched properly</em>.</h1>
    <p class="lede">turborust supervises your services, watches the files that actually matter, caches what it can, and <strong>tells you exactly why anything re-ran</strong>. Watch globs come from cargo, so touching a shared crate rebuilds everything that depends on it — without you listing a thing.</p>
    <div class="cta">
      <a class="btn btn-primary" href="/docs.html">Read the docs</a>
      <a class="btn btn-ghost" href="https://github.com/oddurs/turborust">GitHub</a>
      <button class="install" data-copy="cargo install turborust" title="Copy">
        <b>$</b> cargo install turborust <span class="copy-label quiet">copy</span>
      </button>
    </div>
  </div>

  <div class="tide-wrap">
    <div class="crab-stage" aria-hidden="true">
      <svg viewBox="0 0 200 120">
        <path class="crab-line" d="M46 62 Q22 52 26 30 Q28 18 40 20"/>
        <path class="crab-line" d="M154 62 Q178 52 174 30 Q172 18 160 20"/>
        <path class="crab-line" d="M60 88 47 108M78 94 70 114M122 94 130 114M140 88 153 108"/>
        <ellipse class="crab-body" cx="100" cy="72" rx="46" ry="30"/>
        <ellipse class="crab-shine" cx="86" cy="60" rx="16" ry="8"/>
        <circle class="crab-body" cx="34" cy="22" r="11"/>
        <circle class="crab-body" cx="166" cy="22" r="11"/>
        <circle class="crab-eye-white" cx="84" cy="62" r="9"/>
        <circle class="crab-eye-white" cx="116" cy="62" r="9"/>
        <circle class="crab-pupil" cx="84" cy="62" r="4"/>
        <circle class="crab-pupil" cx="116" cy="62" r="4"/>
      </svg>
    </div>
    <canvas id="tide" aria-hidden="true"></canvas>
  </div>
</section>

<svg class="tide-edge" viewBox="0 0 1440 54" preserveAspectRatio="none" aria-hidden="true"><path d="M0 30 C 120 6, 240 6, 360 24 C 480 42, 600 48, 720 34 C 840 20, 960 4, 1080 14 C 1200 24, 1320 44, 1440 32 L1440 54 L0 54 Z"/></svg>

<div class="band">
<section>
<div class="wrap">

## Three things, done properly

<p class="section-lede">Not a bundler, not a compiler. It orchestrates the tools you already use and makes everything <em>around</em> the compile instant.</p>

<div class="pebbles">
  <div class="pebble">
    <span class="mark" aria-hidden="true"><svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"><path d="M3 12h4l3-8 4 16 3-8h4"/></svg></span>
    <h3>Supervises</h3>
    <p><code>depends_on</code> waits for readiness, not for a process to exist. A dependent starts when its dependency answers, and stops when it goes away.</p>
  </div>
  <div class="pebble">
    <span class="mark" aria-hidden="true"><svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="M16.5 16.5 21 21"/></svg></span>
    <h3>Watches</h3>
    <p>Globs derived from your cargo dependency closure. Edit <code>crates/shared</code> and every consumer rebuilds, because cargo says they should.</p>
  </div>
  <div class="pebble">
    <span class="mark" aria-hidden="true"><svg width="26" height="26" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round"><path d="M12 3v18M5 8l7-5 7 5v8l-7 5-7-5z"/></svg></span>
    <h3>Explains</h3>
    <p>Content-addressed cache with a strict environment, so a hit is a real replay. <code>why</code> names the file and the hash that caused it.</p>
  </div>
</div>

</div>
</section>
</div>

<svg class="band-out" viewBox="0 0 1440 54" preserveAspectRatio="none" aria-hidden="true"><path d="M0 30 C 140 10, 280 44, 420 36 C 560 28, 700 6, 840 16 C 980 26, 1120 46, 1260 38 C 1340 34, 1400 26, 1440 22 L1440 54 L0 54 Z"/></svg>

<section>
<div class="wrap split">
<div>

## The question every build tool dodges

<p class="section-lede">“Why did that rebuild?” usually has no answer. Here it is a command.</p>

<pre><code><span class="c">$</span> turborust why check

  <span class="k">check</span>
  <span class="c">inputs derived from crate `api` and its path deps: api, shared</span>
  <span class="c">5 input files, key b3:614d4a52304e</span>

  <span class="k">cache MISS</span> — b3:9068e57b411c → b3:614d4a52304e

      ~ crates/shared/src/lib.rs  b3:5f16c070 -> b3:d33db911
</code></pre>

<p class="quiet">Content hashes, not mtimes — so <code>git checkout</code> does not invalidate the world.</p>

</div>
<div>

## One file

<pre><code><span class="c"># turborust.toml</span>
<span class="k">[services.api]</span>
cargo = <span class="s">"api"</span>       <span class="c"># globs come from cargo</span>
port  = <span class="s">8788</span>
health = { http = <span class="s">"/healthz"</span> }

<span class="k">[services.web]</span>
depends_on = [<span class="s">"api"</span>]
serve = { dir = <span class="s">"dist"</span>, port = <span class="s">8789</span> }
proxy = [{ path = <span class="s">"/api"</span>, to = <span class="s">"http://127.0.0.1:8788"</span> }]
</code></pre>

<p class="quiet">The proxy is not decoration: it gives the page and the API one origin, so there is no CORS in development and no API host to swap for production.</p>

</div>
</div>
</section>

<svg class="tide-edge" viewBox="0 0 1440 54" preserveAspectRatio="none" aria-hidden="true"><path d="M0 22 C 160 40, 320 8, 480 18 C 640 28, 800 48, 960 38 C 1120 28, 1280 6, 1440 18 L1440 54 L0 54 Z"/></svg>

<div class="band">
<section>
<div class="wrap narrow">

## A crab in the corner of your page

<p class="section-lede">When a build breaks, the browser normally shows you nothing. turborust injects an overlay that shows real rustc diagnostics — message, code, <code>file:line:col</code>, and the code frame with rustc’s own hint — with the location as a <code>vscode://</code> link.</p>

<ol class="steps">
  <li><strong>Issues</strong><span>Parsed from cargo’s output. A failure takes over the viewport at 94% over a blur, so a ghost of your app stays visible.</span></li>
  <li><strong>Processes</strong><span>Health, port, uptime, pid, restarts, and the file that caused the last one.</span></li>
  <li><strong>Last build</strong><span>Cache hit or miss, the blake3 key, and the inputs that changed. <code>why</code>, in the browser.</span></li>
  <li><strong>Output</strong><span>Merged process output, streamed incrementally, filtered per process.</span></li>
</ol>

<p class="quiet">It mounts in a shadow root and loads no web fonts. An injected overlay that inherits your <code>* { box-sizing }</code> is unshippable, and pulling fonts into a page you don’t own is rude.</p>

</div>
</section>
</div>

<svg class="band-out" viewBox="0 0 1440 54" preserveAspectRatio="none" aria-hidden="true"><path d="M0 34 C 120 18, 240 4, 360 12 C 520 22, 640 46, 800 40 C 960 34, 1080 10, 1240 18 C 1340 23, 1400 30, 1440 34 L1440 54 L0 54 Z"/></svg>

<section>
<div class="wrap narrow">

## Honest limits

<p class="section-lede">turborust cannot make <code>rustc</code> fast, and neither can anything else. It avoids compiling when nothing changed, compiles exactly the right things, and overlaps the rest — but the floor is your build. <code>turborust doctor</code> targets that floor directly.</p>

<p>The browser reload is a <strong>full page reload</strong>, not HMR — a wasm binary has no module boundaries left to patch, and calling it HMR would be a lie. Stylesheets are swapped in place, so a colour change keeps your app’s state.</p>

<p>On Windows, 165 unit tests pass; the pty tests and the integration suites are Unix-gated, so pty behaviour there is <em>unverified</em> rather than working. That is written down where a reader will see it rather than where it is technically true.</p>

<div class="cta" style="margin-top:28px">
  <a class="btn btn-primary" href="/docs.html">Get started</a>
  <a class="btn btn-ghost" href="https://github.com/oddurs/turborust">Read the source</a>
</div>

</div>
</section>
