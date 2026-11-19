/* Ember wallpaper runtime: video layer, particle canvas and pause/resume logic.
   Idle until window.__emberSetWallpaper() receives an active config. */
(function () {
  "use strict";
  if (window.__emberSetWallpaper) return;
  var doc = document, root = doc.documentElement;
  var cfg = { active: false };
  var host = { focused: true, minimized: false };
  var layer = null, video = null, canvas = null, ctx = null;
  var listening = false, running = false, raf = 0, lastFrame = 0;
  // Particles: 6 floats each (x, y, vx, vy, size, phase), allocated only when the kind/count changes.
  var P = null, pn = 0, kind = "", color = "#fff", speed = 1, W = 0, H = 0, dpr = 1;
  var FRAME_MS = 1000 / 30;
  var mq = window.matchMedia ? window.matchMedia("(prefers-reduced-motion: reduce)") : null;

  function reduced() {
    return !!(cfg.reduced || (mq && mq.matches) || root.getAttribute("data-reduced-motion") === "true");
  }
  function shouldRun() {
    return !!cfg.active && !reduced() && !doc.hidden && !host.minimized && (!cfg.focus_only || host.focused);
  }

  function getLayer() {
    if (layer && layer.isConnected) return layer;
    layer = doc.getElementById("ember-wp");
    if (!layer && doc.body) {
      layer = doc.createElement("div");
      layer.id = "ember-wp";
      layer.setAttribute("aria-hidden", "true");
      doc.body.insertBefore(layer, doc.body.firstChild);
    }
    return layer;
  }

  function setVideo(url) {
    if (typeof url !== "string" || !url) {
      if (video) {
        video.pause();
        video.removeAttribute("src");
        video.load();
        video.remove();
        video = null;
      }
      return;
    }
    var l = getLayer();
    if (!l) return;
    if (!video) {
      video = doc.createElement("video");
      video.muted = true;
      video.defaultMuted = true;
      video.loop = true;
      video.playsInline = true;
      video.preload = "auto";
      video.tabIndex = -1;
      video.setAttribute("muted", "");
      video.setAttribute("playsinline", "");
      video.setAttribute("disablepictureinpicture", "");
      video.setAttribute("aria-hidden", "true");
      l.appendChild(video);
    }
    if (video.getAttribute("src") !== url) video.setAttribute("src", url);
  }

  function rnd(a, b) { return a + Math.random() * (b - a); }

  function seed(i, anywhere) {
    var o = i * 6;
    P[o] = rnd(0, W);
    P[o + 5] = rnd(0, 6.283);
    if (kind === "rain") {
      P[o + 1] = anywhere ? rnd(0, H) : rnd(-H, 0);
      P[o + 2] = -25; P[o + 3] = rnd(260, 420); P[o + 4] = rnd(6, 12);
    } else if (kind === "stars") {
      P[o + 1] = rnd(0, H); P[o + 2] = 0; P[o + 3] = 0; P[o + 4] = rnd(0.6, 1.8);
    } else if (kind === "fireflies") {
      P[o + 1] = rnd(0, H); P[o + 2] = rnd(-12, 12); P[o + 3] = rnd(-6, 6); P[o + 4] = rnd(1.2, 2.4);
    } else {
      P[o + 1] = anywhere ? rnd(0, H) : rnd(-10, 0);
      P[o + 2] = rnd(-8, 8); P[o + 3] = rnd(10, 28); P[o + 4] = rnd(0.8, 2.4);
    }
  }

  function resize() {
    if (!canvas || !ctx || !P) return;
    dpr = Math.min(window.devicePixelRatio || 1, 3);
    W = canvas.clientWidth || 1;
    H = canvas.clientHeight || 1;
    var cw = Math.round(W * dpr), ch = Math.round(H * dpr);
    if (canvas.width !== cw || canvas.height !== ch) {
      canvas.width = cw;
      canvas.height = ch;
    }
    // Resizing resets the context state.
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = color;
    ctx.strokeStyle = color;
    ctx.lineWidth = 1;
    for (var i = 0; i < pn; i++) {
      var o = i * 6;
      if (P[o] > W || P[o + 1] > H) seed(i, true);
    }
    if (!running) draw(0);
  }

  function setParticles(k, count) {
    if (k !== "snow" && k !== "stars" && k !== "fireflies" && k !== "rain") {
      kind = ""; P = null; pn = 0;
      if (canvas) { canvas.style.display = "none"; canvas.width = 0; canvas.height = 0; }
      return;
    }
    canvas = canvas || doc.getElementById("ember-wp-fx");
    if (!canvas) return;
    ctx = ctx || canvas.getContext("2d");
    if (!ctx) return;
    canvas.style.display = "block";
    color = typeof cfg.pcolor === "string" ? cfg.pcolor : "#fff";
    speed = Math.max(0.2, Math.min(3, +cfg.pspeed || 1));
    var n = Math.max(1, Math.min(150, Math.round(+count || 60)));
    if (k !== kind || n !== pn || !P) {
      kind = k; pn = n; P = new Float32Array(n * 6);
      W = canvas.clientWidth || 1; H = canvas.clientHeight || 1;
      for (var i = 0; i < n; i++) seed(i, true);
    }
    resize();
  }

  function step(dt, t) {
    var s = dt * speed;
    for (var i = 0; i < pn; i++) {
      var o = i * 6;
      if (kind === "stars") continue;
      if (kind === "fireflies") {
        P[o] += (P[o + 2] + Math.sin(t * 0.7 + P[o + 5]) * 10) * s;
        P[o + 1] += (P[o + 3] + Math.cos(t * 0.9 + P[o + 5]) * 6) * s;
        if (P[o] < -5) P[o] = W + 5; else if (P[o] > W + 5) P[o] = -5;
        if (P[o + 1] < -5) P[o + 1] = H + 5; else if (P[o + 1] > H + 5) P[o + 1] = -5;
        continue;
      }
      var drift = kind === "snow" ? Math.sin(t + P[o + 5]) * 6 : 0;
      P[o] += (P[o + 2] + drift) * s;
      P[o + 1] += P[o + 3] * s;
      if (P[o + 1] > H + 12 || P[o] < -20 || P[o] > W + 20) seed(i, false);
    }
  }

  function draw(t) {
    if (!ctx || !P) return;
    ctx.clearRect(0, 0, W, H);
    var i, o;
    if (kind === "rain") {
      ctx.globalAlpha = 0.45;
      ctx.beginPath();
      for (i = 0; i < pn; i++) {
        o = i * 6;
        ctx.moveTo(P[o], P[o + 1]);
        ctx.lineTo(P[o] + P[o + 2] * 0.03, P[o + 1] + P[o + 4]);
      }
      ctx.stroke();
    } else if (kind === "snow") {
      ctx.globalAlpha = 0.85;
      ctx.beginPath();
      for (i = 0; i < pn; i++) {
        o = i * 6;
        ctx.moveTo(P[o] + P[o + 4], P[o + 1]);
        ctx.arc(P[o], P[o + 1], P[o + 4], 0, 6.283);
      }
      ctx.fill();
    } else if (kind === "stars") {
      for (i = 0; i < pn; i++) {
        o = i * 6;
        ctx.globalAlpha = 0.25 + 0.75 * Math.abs(Math.sin(t * 1.3 * speed + P[o + 5]));
        ctx.fillRect(P[o], P[o + 1], P[o + 4], P[o + 4]);
      }
    } else {
      for (i = 0; i < pn; i++) {
        o = i * 6;
        var a = 0.35 + 0.65 * Math.abs(Math.sin(t * 0.8 * speed + P[o + 5]));
        ctx.globalAlpha = a * 0.18;
        ctx.beginPath();
        ctx.arc(P[o], P[o + 1], P[o + 4] * 3, 0, 6.283);
        ctx.fill();
        ctx.globalAlpha = a;
        ctx.beginPath();
        ctx.arc(P[o], P[o + 1], P[o + 4], 0, 6.283);
        ctx.fill();
      }
    }
    ctx.globalAlpha = 1;
  }

  function frame(now) {
    raf = 0;
    if (!running || !P) return;
    raf = requestAnimationFrame(frame);
    if (lastFrame && now - lastFrame < FRAME_MS - 1) return; // hard 30 FPS cap
    var dt = lastFrame ? Math.min(0.1, (now - lastFrame) / 1000) : 0;
    lastFrame = now;
    step(dt, now / 1000);
    draw(now / 1000);
  }

  function update() {
    if (!cfg.active && !running && !video) {
      root.style.removeProperty("--ember-wp-play");
      return;
    }
    var run = shouldRun();
    root.style.setProperty("--ember-wp-play", run ? "running" : "paused");
    if (video) {
      if (run) {
        var p = video.play();
        if (p && p.catch) p.catch(function () {});
      } else {
        video.pause();
      }
    }
    if (run && P) {
      if (!running) {
        running = true;
        lastFrame = 0;
        raf = requestAnimationFrame(frame);
      }
    } else {
      running = false;
      if (raf) cancelAnimationFrame(raf);
      raf = 0;
    }
  }

  function listen() {
    if (listening) return;
    listening = true;
    doc.addEventListener("visibilitychange", update);
    if (mq) {
      if (mq.addEventListener) mq.addEventListener("change", update);
      else if (mq.addListener) mq.addListener(update);
    }
    if (window.MutationObserver) {
      new MutationObserver(update).observe(root, { attributes: true, attributeFilter: ["data-reduced-motion"] });
    }
    window.addEventListener("resize", function () { if (P) resize(); });
  }

  window.__emberSetWallpaper = function (c) {
    cfg = c && typeof c === "object" ? c : { active: false };
    if (cfg.active) listen();
    setVideo(cfg.active ? cfg.video : null);
    setParticles(cfg.active ? cfg.particles : null, cfg.count);
    update();
  };
  // Host signals from Rust: window focus / minimize.
  window.__emberWallpaperHost = function (h) {
    if (h && typeof h.focused === "boolean") host.focused = h.focused;
    if (h && typeof h.minimized === "boolean") host.minimized = h.minimized;
    if (cfg.active) update();
  };
  if (window.__emberWallpaperInit) window.__emberSetWallpaper(window.__emberWallpaperInit);
})();
