/* turborust dev overlay.
 *
 * Mounted into a shadow root so it shares no CSS with the host app. Exposed as
 * `window.__turborustMount(options)` rather than auto-running, so the same file
 * drives both the live overlay and an offline design demo — one implementation,
 * no second copy to drift.
 */
(function () {
  "use strict";

  var POSITIONS = ["top-left", "top-right", "bottom-left", "bottom-right"];
  var PREF_KEY = "turborust.overlay.prefs";

  function esc(s) {
    return String(s == null ? "" : s).replace(/[&<>"']/g, function (c) {
      return { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c];
    });
  }

  function dur(ms) {
    if (ms == null) return "";
    if (ms < 1000) return Math.round(ms) + "ms";
    if (ms < 60000) return (ms / 1000).toFixed(1) + "s";
    var s = Math.round(ms / 1000);
    return Math.floor(s / 60) + "m" + String(s % 60).padStart(2, "0") + "s";
  }

  function loadPrefs() {
    try {
      return JSON.parse(localStorage.getItem(PREF_KEY)) || {};
    } catch (e) {
      return {};
    }
  }

  function savePrefs(p) {
    try {
      localStorage.setItem(PREF_KEY, JSON.stringify(p));
    } catch (e) {
      /* private mode: preferences just do not persist */
    }
  }

  /** Matches a KeyboardEvent against a shortcut like "ctrl+`" or "shift+alt+d". */
  function matchesShortcut(ev, spec) {
    var parts = String(spec || "").toLowerCase().split("+").map(function (s) { return s.trim(); });
    var key = parts.pop();
    var need = { ctrl: false, alt: false, shift: false, meta: false };
    parts.forEach(function (m) {
      if (m === "cmd" || m === "meta") need.meta = true;
      else if (m in need) need[m] = true;
    });
    return (
      ev.ctrlKey === need.ctrl &&
      ev.altKey === need.alt &&
      ev.shiftKey === need.shift &&
      ev.metaKey === need.meta &&
      ev.key.toLowerCase() === key
    );
  }

  var TEMPLATE =
    '<div class="root">' +
    '<div class="panel" role="dialog" aria-label="turborust dev tools">' +
    '<div class="head">' +
    '<span class="face" aria-hidden="true"></span>' +
    '<span class="name">turborust</span>' +
    '<span class="chip"><span class="dot"></span><span class="chiptext">ready</span></span>' +
    '<span class="spacer"></span>' +
    '<button class="icon-btn close" title="Close" aria-label="Close">' +
    '<svg width="11" height="11" viewBox="0 0 11 11" aria-hidden="true"><path d="M1 1l9 9M10 1l-9 9" stroke="currentColor" stroke-width="1.6" stroke-linecap="round"/></svg>' +
    "</button></div>" +
    '<div class="body">' +
    '<button class="row" data-drawer="issues" aria-expanded="false">' +
    '<span class="lead"></span><span class="label">Issues</span>' +
    '<span class="value"></span><span class="caret">' + caret() + "</span></button>" +
    '<div class="drawer" data-for="issues"></div>' +
    '<button class="row" data-drawer="services" aria-expanded="false">' +
    '<span class="lead"></span><span class="label">Processes</span>' +
    '<span class="value"></span><span class="caret">' + caret() + "</span></button>" +
    '<div class="drawer" data-for="services"></div>' +
    '<button class="row" data-drawer="build" aria-expanded="false">' +
    '<span class="lead"></span><span class="label">Last build</span>' +
    '<span class="value"></span><span class="caret">' + caret() + "</span></button>" +
    '<div class="drawer" data-for="build"></div>' +
    '<button class="row" data-drawer="logs" aria-expanded="false">' +
    '<span class="lead"></span><span class="label">Output</span>' +
    '<span class="value"></span><span class="caret">' + caret() + "</span></button>" +
    '<div class="drawer" data-for="logs"></div>' +
    '<button class="row" data-drawer="prefs" aria-expanded="false">' +
    '<span class="lead"></span><span class="label">Preferences</span>' +
    '<span class="value"></span><span class="caret">' + caret() + "</span></button>" +
    '<div class="drawer" data-for="prefs"></div>' +
    "</div>" +
    '<div class="foot"><span class="uptime"></span><span class="spacer" style="flex:1"></span><kbd class="sc"></kbd> to toggle</div>' +
    "</div>" +
    '<button class="bubble" title="turborust" aria-label="turborust dev tools">' +
    '<svg class="ring" viewBox="0 0 44 44" aria-hidden="true">' +
    '<rect class="track" x="2" y="2" width="40" height="40" rx="13"/>' +
    '<rect class="arc" x="2" y="2" width="40" height="40" rx="13"/>' +
    "</svg>" +
    '<span class="face" aria-hidden="true"></span>' +
    '<span class="badge">0</span>' +
    "</button>" +
    "</div>" +
    '<div class="takeover" role="alertdialog" aria-label="Build failed"><div class="sheet"></div></div>';

  function caret() {
    return '<svg width="6" height="9" viewBox="0 0 6 9" aria-hidden="true"><path d="M1 1l4 3.5L1 8" stroke="currentColor" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  }

  function mount(opts) {
    opts = opts || {};
    var cfg = Object.assign(
      {
        emoji: "🦀",
        position: "bottom-right",
        theme: "auto",
        shortcut: "ctrl+`",
        errors: "overlay",
        root: "",
        endpoint: "/__turborust/state",
        logs: "/__turborust/logs",
        reload: "/__turborust/reload",
        live: true,
      },
      opts.config || {}
    );
    var prefs = opts.ignorePrefs ? {} : loadPrefs();
    ["position", "theme", "errors"].forEach(function (k) {
      if (prefs[k]) cfg[k] = prefs[k];
    });

    var host = document.createElement("div");
    host.setAttribute("data-turborust", "");
    var shadow = host.attachShadow({ mode: "open" });
    var style = document.createElement("style");
    style.textContent = opts.css || "";
    shadow.appendChild(style);
    var wrap = document.createElement("div");
    wrap.innerHTML = TEMPLATE;
    while (wrap.firstChild) shadow.appendChild(wrap.firstChild);
    (opts.container || document.body).appendChild(host);

    var $ = function (sel) { return shadow.querySelector(sel); };
    var $$ = function (sel) { return Array.prototype.slice.call(shadow.querySelectorAll(sel)); };

    var state = {
      snapshot: null, open: false, takeover: false, issue: 0,
      logFilter: null, drawers: {},
      logs: [], lastSeq: 0, lastReload: null
    };
    // Bounded: a noisy build should not grow the page without limit.
    var LOG_CAP = 2000;

    function applyConfig() {
      host.setAttribute("data-position", POSITIONS.indexOf(cfg.position) >= 0 ? cfg.position : "bottom-right");
      host.setAttribute("data-theme", cfg.theme);
      $$(".face").forEach(function (el) { el.textContent = cfg.emoji; });
      $(".sc").textContent = cfg.shortcut;
    }

    function setOpen(v) {
      state.open = v;
      host.setAttribute("data-open", v ? "yes" : "no");
    }

    function setTakeover(v) {
      state.takeover = v;
      host.setAttribute("data-takeover", v ? "yes" : "no");
      if (v) renderTakeover();
    }

    function errors() {
      var s = state.snapshot;
      return s && s.issues ? s.issues.filter(function (i) { return i.level === "error"; }) : [];
    }

    function render() {
      var s = state.snapshot;
      if (!s) return;
      host.setAttribute("data-status", s.status);
      var errs = errors();
      host.setAttribute("data-issues", errs.length ? "yes" : "no");
      $(".badge").textContent = errs.length > 9 ? "9+" : String(errs.length);
      $(".chiptext").textContent = s.status;

      var warns = (s.issues || []).length - errs.length;
      setRow("issues", errs.length ? errs.length + (errs.length === 1 ? " error" : " errors")
        : warns ? warns + (warns === 1 ? " warning" : " warnings") : "none", errs.length > 0);

      var svcs = (s.nodes || []).filter(function (n) { return n.kind === "service"; });
      var up = svcs.filter(function (n) { return n.status === "healthy"; }).length;
      setRow("services", up + "/" + svcs.length + " healthy", up < svcs.length && s.status !== "building");

      var b = s.build;
      setRow("build", b ? (b.cached ? "cached · " : "") + dur(b.duration_ms) : "—", false);
      setRow("logs", state.logs.length ? state.logs.length + " lines" : "—", false);
      setRow("prefs", cfg.position, false);

      var oldest = (s.nodes || []).reduce(function (m, n) { return Math.max(m, n.uptime_ms || 0); }, 0);
      $(".uptime").textContent = oldest ? "up " + dur(oldest) : "";

      renderDrawer("issues", renderIssues);
      renderDrawer("services", renderServices);
      renderDrawer("build", renderBuild);
      renderDrawer("logs", renderLogs);

      // A fresh error takes over; a fixed one releases the page immediately.
      if (cfg.errors === "overlay") {
        if (errs.length && !state.takeover && state.issue === 0) setTakeover(true);
        if (!errs.length && state.takeover) setTakeover(false);
      }
      if (state.takeover) renderTakeover();
    }

    function setRow(key, value, bad) {
      var row = $('.row[data-drawer="' + key + '"]');
      row.querySelector(".value").textContent = value;
      row.classList.toggle("bad", !!bad);
    }

    function renderDrawer(key, fn) {
      var el = $('.drawer[data-for="' + key + '"]');
      el.classList.toggle("open", !!state.drawers[key]);
      if (state.drawers[key]) el.innerHTML = fn();
    }

    function renderIssues() {
      var list = (state.snapshot.issues || []);
      if (!list.length) return '<div class="empty">Nothing to report.</div>';
      return list
        .map(function (i, n) {
          return (
            '<button class="svc issue" data-issue="' + n + '" style="width:100%;text-align:left">' +
            '<span class="dot ' + (i.level === "error" ? "failed" : "running") + '"></span>' +
            '<span class="who"><span class="nm"><b>' + esc(i.message) + "</b></span>" +
            '<span class="sub">' + esc(i.file ? i.file + ":" + i.line : i.node) + "</span></span>" +
            '<span class="meta">' + esc(i.code || i.level) + "</span></button>"
          );
        })
        .join("");
    }

    function renderServices() {
      var nodes = state.snapshot.nodes || [];
      if (!nodes.length) return '<div class="empty">No processes.</div>';
      return nodes
        .map(function (n) {
          var sub = [];
          if (n.port) sub.push('<a class="port" href="http://localhost:' + n.port + '" target="_blank" rel="noreferrer">:' + n.port + "</a>");
          if (n.reason) sub.push(esc(n.reason));
          else if (n.label && n.label !== n.status) sub.push(esc(n.label));
          var meta = [];
          if (n.uptime_ms) meta.push(dur(n.uptime_ms));
          if (n.restarts) meta.push("&times;" + n.restarts);
          if (n.pid) meta.push("pid " + n.pid);
          return (
            '<div class="svc"><span class="dot ' + esc(n.status) + '"></span>' +
            '<span class="who"><span class="nm"><b>' + esc(n.name) + "</b>" +
            '<span class="kind">' + esc(n.kind) + "</span></span>" +
            '<span class="sub">' + (sub.join(" · ") || "&nbsp;") + "</span></span>" +
            '<span class="meta">' + (meta.join("<br>") || "") +
            '<button class="restart" data-restart="' + esc(n.name) + '" title="Restart ' + esc(n.name) + '">\u21bb</button>' +
            "</span></div>"
          );
        })
        .join("");
    }

    function renderBuild() {
      var b = state.snapshot.build;
      if (!b) return '<div class="empty">No build yet.</div>';
      var html =
        '<dl class="kv">' +
        "<dt>task</dt><dd>" + esc(b.node) + "</dd>" +
        "<dt>result</dt><dd class=\"" + (b.cached ? "hit" : "miss") + '">' +
        (b.cached ? "cache hit" : "rebuilt") + " · " + dur(b.duration_ms) + "</dd>" +
        "<dt>key</dt><dd>b3:" + esc(b.key) + "</dd></dl>";
      if (b.changed && b.changed.length) {
        html +=
          '<div class="diffhead">why it rebuilt</div><div class="diff">' +
          b.changed
            .map(function (c) {
              return '<div><span class="p">' + esc(c.path) + '</span><span class="h">' +
                esc(c.from) + " → " + esc(c.to) + "</span></div>";
            })
            .join("") +
          "</div>";
      }
      return html;
    }

    function renderLogs() {
      var s = state.snapshot;
      var names = (s.nodes || []).map(function (n) { return n.name; });
      var chips =
        '<div class="filters"><button data-filter="" aria-pressed="' + (!state.logFilter) + '">all</button>' +
        names
          .map(function (n) {
            return '<button data-filter="' + esc(n) + '" aria-pressed="' + (state.logFilter === n) + '">' + esc(n) + "</button>";
          })
          .join("") +
        "</div>";
      var lines = state.logs.filter(function (l) {
        return !state.logFilter || l.proc === state.logFilter;
      });
      if (!lines.length) return chips + '<div class="empty">No output yet.</div>';
      var body = lines
        .map(function (l) {
          var t = l.text || "";
          var cls = /^\s*error/i.test(t) ? " err" : /^\s*warning/i.test(t) ? " warn" : "";
          return '<span class="ln' + cls + '">' +
            (state.logFilter ? "" : '<span class="who">' + esc(l.proc.padEnd(8)) + " </span>") +
            esc(t) + "</span>";
        })
        .join("");
      return chips + '<pre class="logs">' + body + "</pre>";
    }

    function renderPrefs() {
      var pos = POSITIONS.map(function (p) {
        var cls = { "top-left": "tl", "top-right": "tr", "bottom-left": "bl", "bottom-right": "br" }[p];
        return '<button class="' + cls + '" data-pos="' + p + '" aria-pressed="' + (cfg.position === p) +
          '" title="' + p + '" aria-label="' + p + '"><i></i></button>';
      }).join("");
      return (
        '<div class="pref"><label>Position</label><div class="grid4">' + pos + "</div></div>" +
        '<div class="pref"><label for="tr-theme">Theme</label><select id="tr-theme" data-pref="theme">' +
        opt(["auto", "dark", "light"], cfg.theme) + "</select></div>" +
        '<div class="pref"><label for="tr-err">On build error</label><select id="tr-err" data-pref="errors">' +
        opt(["overlay", "badge", "silent"], cfg.errors) + "</select></div>"
      );
    }

    function opt(values, sel) {
      return values
        .map(function (v) {
          return '<option value="' + v + '"' + (v === sel ? " selected" : "") + ">" + v + "</option>";
        })
        .join("");
    }

    function renderTakeover() {
      var errs = errors();
      if (!errs.length) return;
      if (state.issue >= errs.length) state.issue = 0;
      var d = errs[state.issue];
      var loc = d.file ? d.file + (d.line ? ":" + d.line + (d.column ? ":" + d.column : "") : "") : null;
      // vscode:// works from any page and is the one editor jump we can offer
      // without a helper process listening on a port.
      var href = loc && cfg.root ? "vscode://file/" + cfg.root + "/" + loc : null;
      var frame = (d.frame || [])
        .map(function (l) {
          var cls = /[\^~]{2,}/.test(l) ? "caret" : /^\s*\d+\s*\|/.test(l) ? "src" : "gutter";
          return '<span class="l ' + cls + '">' + esc(l) + "</span>";
        })
        .join("");
      $(".sheet").innerHTML =
        '<div class="bar"><span class="face">' + esc(cfg.emoji) + "</span>" +
        '<span class="count">' + (state.issue + 1) + " of " + errs.length +
        (errs.length === 1 ? " error" : " errors") + "</span>" +
        '<span class="spacer"></span><div class="nav">' +
        '<button class="prev" ' + (errs.length < 2 ? "disabled" : "") + ' aria-label="Previous error">&#8249;</button>' +
        '<button class="next" ' + (errs.length < 2 ? "disabled" : "") + ' aria-label="Next error">&#8250;</button>' +
        '<button class="dismiss" aria-label="Dismiss">&#10005;</button></div></div>' +
        '<div class="card"><div class="top"><div class="tags">' +
        '<span class="tag">' + esc(d.level) + "</span>" +
        (d.code ? '<span class="tag code">' + esc(d.code) + "</span>" : "") +
        '<span class="tag node">' + esc(d.node) + "</span></div>" +
        "<h2>" + esc(d.message) + "</h2>" +
        (loc ? (href ? '<a class="where" href="' + esc(href) + '">' + esc(loc) + "</a>"
                     : '<span class="where">' + esc(loc) + "</span>") : "") +
        "</div>" +
        (frame ? '<pre class="frame">' + frame + "</pre>" : "") +
        "</div>" +
        '<div class="hint">Fix the error and this closes itself. <kbd>Esc</kbd> to dismiss' +
        (errs.length > 1 ? ', <kbd>←</kbd><kbd>→</kbd> to step through' : "") + ".</div>";
    }

    /* ------------------------------------------------------------ events -- */

    shadow.addEventListener("click", function (ev) {
      var t = ev.target;
      var row = t.closest && t.closest(".row");
      if (row) {
        var key = row.getAttribute("data-drawer");
        state.drawers[key] = !state.drawers[key];
        row.setAttribute("aria-expanded", String(!!state.drawers[key]));
        if (key === "prefs") {
          var el = $('.drawer[data-for="prefs"]');
          el.classList.toggle("open", !!state.drawers.prefs);
          if (state.drawers.prefs) el.innerHTML = renderPrefs();
        } else {
          render();
        }
        return;
      }
      if (t.closest && t.closest(".bubble")) { setOpen(!state.open); render(); return; }
      if (t.closest && t.closest(".close")) { setOpen(false); return; }
      if (t.closest && t.closest(".dismiss")) { setTakeover(false); return; }
      if (t.closest && t.closest(".prev")) { step(-1); return; }
      if (t.closest && t.closest(".next")) { step(1); return; }

      var issue = t.closest && t.closest("[data-issue]");
      if (issue) {
        var idx = Number(issue.getAttribute("data-issue"));
        var all = state.snapshot.issues || [];
        var errs = errors();
        var pos = errs.indexOf(all[idx]);
        if (pos >= 0) { state.issue = pos; setTakeover(true); }
        return;
      }

      var restart = t.closest && t.closest("[data-restart]");
      if (restart) {
        api.restart(restart.getAttribute("data-restart"));
        return;
      }

      var filter = t.closest && t.closest("[data-filter]");
      if (filter) {
        state.logFilter = filter.getAttribute("data-filter") || null;
        render();
        return;
      }

      var pos2 = t.closest && t.closest("[data-pos]");
      if (pos2) {
        cfg.position = pos2.getAttribute("data-pos");
        prefs.position = cfg.position;
        savePrefs(prefs);
        applyConfig();
        $('.drawer[data-for="prefs"]').innerHTML = renderPrefs();
        setRow("prefs", cfg.position, false);
      }
    });

    shadow.addEventListener("change", function (ev) {
      var key = ev.target.getAttribute && ev.target.getAttribute("data-pref");
      if (!key) return;
      cfg[key] = ev.target.value;
      prefs[key] = ev.target.value;
      savePrefs(prefs);
      applyConfig();
      if (key === "errors" && cfg.errors !== "overlay") setTakeover(false);
      render();
    });

    function step(n) {
      var errs = errors();
      if (!errs.length) return;
      state.issue = (state.issue + n + errs.length) % errs.length;
      renderTakeover();
    }

    var onKey = function (ev) {
      if (matchesShortcut(ev, cfg.shortcut)) { ev.preventDefault(); setOpen(!state.open); render(); return; }
      if (!state.takeover) return;
      if (ev.key === "Escape") setTakeover(false);
      else if (ev.key === "ArrowLeft") step(-1);
      else if (ev.key === "ArrowRight") step(1);
    };
    window.addEventListener("keydown", onKey);

    applyConfig();
    setOpen(false);
    host.setAttribute("data-status", "ready");

    var api = {
      update: function (snapshot) { state.snapshot = snapshot; render(); },
      appendLogs: function (lines) {
        if (!lines || !lines.length) return;
        state.logs = state.logs.concat(lines);
        if (state.logs.length > LOG_CAP) state.logs = state.logs.slice(-LOG_CAP);
        state.lastSeq = lines[lines.length - 1].seq || state.lastSeq;
        // Only repaint if the pane is actually on screen.
        if (state.open && state.drawers.logs) renderDrawer("logs", renderLogs);
      },
      lastSeq: function () { return state.lastSeq; },
      reloaded: function (kind) {
        state.lastReload = kind;
        var el = $(".chiptext");
        if (el) el.textContent = kind === "css" ? "css swapped" : el.textContent;
      },
      restart: function (node) {
        return fetch("/__turborust/restart/" + encodeURIComponent(node), { method: "POST" });
      },
      open: function (v) { setOpen(v !== false); render(); },
      config: cfg,
      destroy: function () { window.removeEventListener("keydown", onKey); host.remove(); },
    };

    if (cfg.live) connect(api, cfg);
    return api;
  }

  /** Streams snapshots, and reconnects with backoff — a dev server restarting is
   *  normal, so the overlay must survive it without spinning. */
  function connect(api, cfg) {
    var backoff = 250;
    (function open() {
      var es = new EventSource(cfg.endpoint);
      es.onopen = function () { backoff = 250; };
      es.onmessage = function (ev) {
        try { api.update(JSON.parse(ev.data)); } catch (e) { /* partial frame */ }
      };
      es.onerror = function () {
        es.close();
        api.update({ status: "stopped", nodes: [], issues: [], logs: [], build: null });
        setTimeout(open, backoff);
        backoff = Math.min(backoff * 2, 5000);
      };
    })();

    if (cfg.reload) {
      var rb = 250;
      (function open() {
        var es = new EventSource(cfg.reload);
        es.onopen = function () { rb = 250; };
        es.onmessage = function (ev) {
          // A stylesheet change is swapped in place, so the running app keeps
          // its route, its form state and its scroll position.
          if (ev.data === "css") { swapStylesheets(); api.reloaded("css"); return; }
          location.reload();
        };
        es.onerror = function () {
          es.close();
          setTimeout(open, rb);
          rb = Math.min(rb * 2, 5000);
        };
      })();
    }

    if (cfg.logs) connectLogs(api, cfg);
  }

  // Re-requests every stylesheet with a fresh query string, which is the only
  // portable way to make a browser drop a cached CSS file without a reload.
  function swapStylesheets() {
    var links = document.querySelectorAll('link[rel="stylesheet"]');
    Array.prototype.forEach.call(links, function (link) {
      var href = link.getAttribute("href");
      if (!href || /^(https?:)?\/\//.test(href)) return; // leave third-party CSS alone
      var next = link.cloneNode();
      next.setAttribute("href", href.split("?")[0] + "?turborust=" + Date.now());
      // Swap only once the replacement has loaded, so the page never flashes
      // unstyled.
      next.addEventListener("load", function () { link.remove(); });
      link.parentNode.insertBefore(next, link.nextSibling);
    });
  }

  // Streams output incrementally, resuming from the last line seen.
  function connectLogs(api, cfg) {
    var backoff = 250;
    (function open() {
      var url = cfg.logs + "?after=" + api.lastSeq();
      var es = new EventSource(url);
      es.onopen = function () { backoff = 250; };
      es.onmessage = function (ev) {
        try { api.appendLogs(JSON.parse(ev.data)); } catch (e) { /* partial frame */ }
      };
      es.onerror = function () {
        es.close();
        // Reconnect from where we left off rather than refetching the tail.
        setTimeout(open, backoff);
        backoff = Math.min(backoff * 2, 5000);
      };
    })();
  }

  window.__turborustMount = mount;
})();

/* Auto-boot when injected by turborust: the script tag carries its config, and
 * the stylesheet is a separate request so HTML responses stay small. */
(function () {
  var tag = document.currentScript;
  if (!tag) return;
  var raw = tag.getAttribute("data-turborust");
  if (!raw) return;
  var cfg;
  try { cfg = JSON.parse(raw); } catch (e) { return; }
  var boot = function () {
    fetch(cfg.cssUrl)
      .then(function (r) { return r.text(); })
      .then(function (css) { window.__turborustMount({ config: cfg, css: css }); })
      .catch(function () { /* server gone; the page is still usable */ });
  };
  if (document.body) boot();
  else document.addEventListener("DOMContentLoaded", boot);
})();
