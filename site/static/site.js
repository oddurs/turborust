// The tide, the crab's eyes, and the occasional bubble.
//
// One orchestrated moment rather than scattered effects: the waterline breathes,
// the crab looks where you look, and bubbles drift up rarely enough to be
// noticed rather than watched. All of it stops under prefers-reduced-motion.
(function () {
  "use strict";
  var still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  function token(name, fallback) {
    var v = getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    return v || fallback;
  }

  // A stable hash-to-unit-interval, so the glitter and foam sit in the same
  // places every frame instead of flickering like static.
  function rnd(i) {
    var s = Math.sin(i * 127.1 + 3.7) * 43758.5453;
    return s - Math.floor(s);
  }

  // ------------------------------------------------------------- the tide ---
  var canvas = document.getElementById("tide");
  if (canvas) {
    var ctx = canvas.getContext("2d");
    var w = 0, h = 0;

    function resize() {
      var dpr = Math.min(window.devicePixelRatio || 1, 2);
      w = canvas.clientWidth;
      h = canvas.clientHeight;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    resize();
    window.addEventListener("resize", resize);

    // Water fills from the top of the strip down to the wave; sand is the page
    // beneath it, so the crab can stand in front of the water rather than in it.
    // Three harmonics per band, at frequencies that do not share a period, so
    // the surface never visibly repeats.
    var bands = [
      { y: 0.16, amp: [11, 5, 2.4], k: [0.0044, 0.0093, 0.0181], sp: [0.016, -0.027, 0.041] },
      { y: 0.42, amp: [10, 5, 2.6], k: [0.0036, 0.0079, 0.0152], sp: [0.022, -0.035, 0.053] },
      { y: 0.72, amp: [12, 6, 3.2], k: [0.0030, 0.0068, 0.0131], sp: [0.029, -0.045, 0.064] }
    ];

    function heightAt(band, x, t) {
      return band.y * h
        + Math.sin(x * band.k[0] + t * band.sp[0]) * band.amp[0]
        + Math.sin(x * band.k[1] + t * band.sp[1]) * band.amp[1]
        + Math.sin(x * band.k[2] + t * band.sp[2]) * band.amp[2];
    }

    // A sheet of water bounded by two waves — a far edge and a near one. A
    // straight far edge would make this a rectangle that happens to be blue.
    function sheet(top, bottom, t, lag, drop) {
      ctx.beginPath();
      ctx.moveTo(0, heightAt(top, 0, t));
      for (var x = 3; x <= w; x += 3) ctx.lineTo(x, heightAt(top, x, t));
      for (var b = w; b >= 0; b -= 3) ctx.lineTo(b, heightAt(bottom, b, t - (lag || 0)) + (drop || 0));
      ctx.closePath();
    }

    function edge(band, t) {
      ctx.beginPath();
      for (var x = 0; x <= w; x += 3) {
        var y = heightAt(band, x, t);
        x === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y);
      }
    }

    function frame(now) {
      var t = now / 1000;
      var tide = token("--tide", "#2b8183");
      var deep = token("--deep", "#17595c");
      var foam = token("--foam", "#fefbf5");
      var wet = token("--wet", "#e5dbc9");

      ctx.clearRect(0, 0, w, h);

      // Wet sand first: the sheet the last wave left behind, lagging the water
      // it came from. It is what makes the edge read as a tide, not a border.
      ctx.globalAlpha = 0.9;
      ctx.fillStyle = wet;
      sheet(bands[2], bands[2], t, 2.6, 30);
      ctx.fill();

      // The body of the water, deep at the far edge and shallow at the sand.
      var g = ctx.createLinearGradient(0, bands[0].y * h, 0, bands[2].y * h);
      g.addColorStop(0, deep);
      g.addColorStop(1, tide);
      ctx.globalAlpha = 1;
      ctx.fillStyle = g;
      sheet(bands[0], bands[2], t);
      ctx.fill();

      // One interior swell, so the surface has a middle as well as two edges.
      ctx.globalAlpha = 0.22;
      ctx.fillStyle = deep;
      sheet(bands[1], bands[2], t);
      ctx.fill();

      // Sun glitter: short dashes lying flat on the surface, each fading on its
      // own clock. This is what stops the water reading as coloured paper.
      ctx.strokeStyle = foam;
      ctx.lineCap = "round";
      var count = Math.round(w / 30);
      for (var i = 0; i < count; i++) {
        var gx = ((rnd(i) * w) + t * (6 + rnd(i + 90) * 10)) % w;
        var band = bands[i % 2];
        var gy = heightAt(band, gx, t) + 10 + rnd(i + 40) * (h * 0.22);
        if (gy > heightAt(bands[2], gx, t) - 6) continue;
        var pulse = Math.sin(t * (0.7 + rnd(i + 7) * 0.9) + rnd(i + 20) * 6.3);
        if (pulse <= 0) continue;
        ctx.globalAlpha = pulse * 0.28;
        ctx.lineWidth = 1.6;
        ctx.beginPath();
        ctx.moveTo(gx - 5 - rnd(i + 3) * 8, gy);
        ctx.lineTo(gx + 5 + rnd(i + 4) * 8, gy);
        ctx.stroke();
      }

      // The foam line is the whole point of a tide: the edge, not the body.
      // The far edge gets a thinner, fainter one so the near edge stays nearest.
      ctx.globalAlpha = 0.35;
      ctx.strokeStyle = foam;
      ctx.lineWidth = 1.4;
      edge(bands[0], t);
      ctx.stroke();

      ctx.globalAlpha = 0.95;
      ctx.lineWidth = 2.6;
      edge(bands[2], t);
      ctx.stroke();

      // Foam breaking along the crests, thickest where the wave is highest.
      var pops = Math.round(w / 20);
      for (var j = 0; j < pops; j++) {
        var px = ((rnd(j + 200) * w) + t * (3 + rnd(j + 210) * 5)) % w;
        var py = heightAt(bands[2], px, t);
        var lift = (bands[2].y * h - py) / (bands[2].amp[0] + bands[2].amp[1]);
        if (lift <= 0.1) continue;
        ctx.globalAlpha = Math.min(lift, 1) * 0.6;
        ctx.fillStyle = foam;
        ctx.beginPath();
        ctx.arc(px, py + 1 + rnd(j + 220) * 4, 0.9 + rnd(j + 230) * 1.9, 0, 6.283);
        ctx.fill();
      }

      ctx.globalAlpha = 1;
      if (!still) requestAnimationFrame(frame);
    }
    // Drawn once even when still, so the shape is there without the motion.
    still ? frame(0) : requestAnimationFrame(frame);
  }

  // -------------------------------------------------------- the crab eyes ---
  var pupils = document.querySelectorAll(".pupil");
  if (pupils.length && !still) {
    window.addEventListener("pointermove", function (e) {
      pupils.forEach(function (p) {
        var box = p.getBoundingClientRect();
        var dx = e.clientX - (box.left + box.width / 2);
        var dy = e.clientY - (box.top + box.height / 2);
        var d = Math.hypot(dx, dy) || 1;
        // Clamped hard: a pupil that travels far reads as a googly eye.
        var r = Math.min(d / 40, 2.4);
        p.style.transform = "translate(" + (dx / d) * r + "px," + (dy / d) * r + "px)";
      });
    }, { passive: true });
  }

  // ----------------------------------------------------------- the bubbles ---
  if (!still) {
    setInterval(function () {
      if (document.hidden) return;
      var b = document.createElement("div");
      b.className = "bubble";
      var size = 4 + Math.random() * 9;
      b.style.width = b.style.height = size + "px";
      b.style.left = Math.random() * 100 + "vw";
      document.body.appendChild(b);
      b.animate(
        [
          { transform: "translateY(0) translateX(0)", opacity: 0 },
          { opacity: 0.45, offset: 0.15 },
          { transform: "translateY(-102vh) translateX(" + (Math.random() * 60 - 30) + "px)", opacity: 0 }
        ],
        { duration: 4200 + Math.random() * 3200, easing: "cubic-bezier(.3,.1,.5,1)" }
      ).onfinish = function () { b.remove(); };
    }, 1600);
  }

  // -------------------------------------------------------------- copy cmd ---
  document.querySelectorAll("[data-copy]").forEach(function (el) {
    el.addEventListener("click", function () {
      navigator.clipboard.writeText(el.getAttribute("data-copy")).then(function () {
        el.setAttribute("data-copied", "");
        el.setAttribute("aria-label", "Copied");
        setTimeout(function () {
          el.removeAttribute("data-copied");
          el.setAttribute("aria-label", "Copy install command");
        }, 1400);
      });
    });
  });
})();
