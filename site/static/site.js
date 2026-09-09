// The tide, the crab's eyes, and the occasional bubble.
//
// One orchestrated moment rather than scattered effects: the waterline breathes,
// the crab looks where you look, and bubbles drift up rarely enough to be
// noticed rather than watched. All of it stops under prefers-reduced-motion.
(function () {
  "use strict";
  var still = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  // ------------------------------------------------------------- the tide ---
  var canvas = document.getElementById("tide");
  if (canvas) {
    var ctx = canvas.getContext("2d");
    var dpr = Math.min(window.devicePixelRatio || 1, 2);
    var w = 0, h = 0;

    function resize() {
      w = canvas.clientWidth;
      h = canvas.clientHeight;
      canvas.width = w * dpr;
      canvas.height = h * dpr;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }
    resize();
    window.addEventListener("resize", resize);

    function token(name) {
      return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
    }

    // Three layers at different speeds. Depth on a flat surface comes from
    // parallax, not from gradients.
    var layers = [
      { amp: 13, len: 0.0055, speed: 0.00022, y: 0.52, alpha: 0.20 },
      { amp: 10, len: 0.0080, speed: 0.00037, y: 0.63, alpha: 0.32 },
      { amp: 7,  len: 0.0125, speed: 0.00055, y: 0.74, alpha: 1.00 }
    ];

    function wave(layer, t, fill, foam) {
      ctx.beginPath();
      ctx.moveTo(0, h);
      for (var x = 0; x <= w; x += 4) {
        var y = layer.y * h
          + Math.sin(x * layer.len + t * layer.speed) * layer.amp
          + Math.sin(x * layer.len * 2.3 + t * layer.speed * 1.6) * (layer.amp * 0.35);
        ctx.lineTo(x, y);
      }
      ctx.lineTo(w, h);
      ctx.closePath();
      ctx.globalAlpha = layer.alpha;
      ctx.fillStyle = fill;
      ctx.fill();

      // The foam line is the whole point of a tide: the edge, not the body.
      if (foam) {
        ctx.globalAlpha = 0.9;
        ctx.beginPath();
        for (var fx = 0; fx <= w; fx += 4) {
          var fy = layer.y * h
            + Math.sin(fx * layer.len + t * layer.speed) * layer.amp
            + Math.sin(fx * layer.len * 2.3 + t * layer.speed * 1.6) * (layer.amp * 0.35);
          fx === 0 ? ctx.moveTo(fx, fy) : ctx.lineTo(fx, fy);
        }
        ctx.strokeStyle = foam;
        ctx.lineWidth = 2;
        ctx.stroke();
      }
      ctx.globalAlpha = 1;
    }

    function frame(t) {
      ctx.clearRect(0, 0, w, h);
      var tide = token("--tide") || "#2e8c8c";
      var shell = token("--shell") || "#fff";
      layers.forEach(function (l, i) {
        wave(l, t, tide, i === layers.length - 1 ? shell : null);
      });
      if (!still) requestAnimationFrame(frame);
    }
    // Drawn once even when still, so the shape is there without the motion.
    still ? frame(0) : requestAnimationFrame(frame);
  }

  // -------------------------------------------------------- the crab eyes ---
  var pupils = document.querySelectorAll(".crab-pupil");
  if (pupils.length && !still) {
    window.addEventListener("pointermove", function (e) {
      pupils.forEach(function (p) {
        var box = p.getBoundingClientRect();
        var dx = e.clientX - (box.left + box.width / 2);
        var dy = e.clientY - (box.top + box.height / 2);
        var d = Math.hypot(dx, dy) || 1;
        // Clamped hard: a pupil that travels far reads as a googly eye.
        var r = Math.min(d / 40, 2.6);
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
      var rise = 4200 + Math.random() * 3200;
      b.animate(
        [
          { transform: "translateY(0) translateX(0)", opacity: 0 },
          { opacity: 0.5, offset: 0.15 },
          { transform: "translateY(-102vh) translateX(" + (Math.random() * 60 - 30) + "px)", opacity: 0 }
        ],
        { duration: rise, easing: "cubic-bezier(.3,.1,.5,1)" }
      ).onfinish = function () { b.remove(); };
    }, 1400);
  }

  // -------------------------------------------------------------- copy cmd ---
  document.querySelectorAll("[data-copy]").forEach(function (el) {
    el.addEventListener("click", function () {
      navigator.clipboard.writeText(el.getAttribute("data-copy")).then(function () {
        var was = el.querySelector(".copy-label");
        if (!was) return;
        var text = was.textContent;
        was.textContent = "copied";
        setTimeout(function () { was.textContent = text; }, 1200);
      });
    });
  });
})();
