// CrabBoy browser frontend: a minimal vanilla-JS shell around the wasm
// `Emulator` binding. No bundler; serve this directory over HTTP after
// generating `pkg/` (see README.md).

import init, { Emulator, button_names, detect } from "./pkg/crab_wasm.js";

const $ = (id) => document.getElementById(id);
const canvas = $("screen");
const ctx = canvas.getContext("2d");
const screenWrap = $("screen-wrap");
const dropHint = $("drop-hint");
const status = $("status");
const romInput = $("rom-input");
const pauseBtn = $("pause-btn");
const resetBtn = $("reset-btn");
const ffBtn = $("ff-btn");
const saveBtn = $("save-btn");
const loadBtn = $("load-btn");
const muteBtn = $("mute-btn");
const shoulders = $("shoulders");

const wasm = await init();
const BUTTON_INDEX = Object.fromEntries(
  button_names()
    .split(",")
    .map((n, i) => [n, i])
);

const KEYMAP = {
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  KeyZ: "A",
  KeyX: "B",
  Enter: "Start",
  Backspace: "Select",
  KeyA: "L",
  KeyS: "R",
};

// Standard gamepad mapping (https://w3c.github.io/gamepad/#remapping).
const GAMEPAD_BUTTONS = {
  0: "A",
  1: "B",
  2: "B",
  3: "A",
  4: "L",
  5: "R",
  6: "L",
  7: "R",
  8: "Select",
  9: "Start",
  12: "Up",
  13: "Down",
  14: "Left",
  15: "Right",
};

const state = {
  emu: null,
  romKey: null,
  title: "",
  paused: false,
  fastForward: false,
  muted: false,
  accum: 0,
  lastTime: 0,
  fps: 0,
  fpsFrames: 0,
  fpsTime: 0,
  held: new Set(),
  gamepadHeld: new Set(),
  saveTimer: null,
};

// ---------------------------------------------------------------------------
// Persistence (IndexedDB keyed by the SHA-256 of the ROM).

// Storage must never block playback: every operation gives up after a
// short timeout (private windows and some headless browsers stall IndexedDB).
function withTimeout(promise, ms) {
  return Promise.race([
    promise,
    new Promise((_, reject) => setTimeout(() => reject(new Error("storage timeout")), ms)),
  ]);
}

function openDb() {
  return withTimeout(
    new Promise((resolve, reject) => {
      const req = indexedDB.open("crabboy", 1);
      req.onupgradeneeded = () => req.result.createObjectStore("saves");
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(req.error);
      req.onblocked = () => reject(new Error("database blocked"));
    }),
    2000
  );
}

async function dbGet(key) {
  try {
    const db = await openDb();
    return await withTimeout(
      new Promise((resolve, reject) => {
        const req = db.transaction("saves").objectStore("saves").get(key);
        req.onsuccess = () => resolve(req.result || null);
        req.onerror = () => reject(req.error);
      }),
      2000
    );
  } catch {
    return null;
  }
}

async function dbPut(key, value) {
  try {
    const db = await openDb();
    await withTimeout(
      new Promise((resolve, reject) => {
        const tx = db.transaction("saves", "readwrite");
        tx.objectStore("saves").put(value, key);
        tx.oncomplete = resolve;
        tx.onerror = () => reject(tx.error);
      }),
      2000
    );
  } catch (e) {
    setStatus(`Could not store save: ${e.message || e}`);
  }
}

async function romKeyFor(bytes) {
  if (crypto.subtle) {
    const digest = await crypto.subtle.digest("SHA-256", bytes);
    return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
  }
  // Insecure contexts have no SubtleCrypto; fall back to FNV-1a over the ROM.
  let h = 0x811c9dc5;
  for (const b of bytes) h = Math.imul(h ^ b, 0x01000193) >>> 0;
  return `fnv-${h.toString(16)}-${bytes.length}`;
}

async function flushSave() {
  const { emu, romKey } = state;
  if (!emu || !romKey || !emu.battery_backed()) return;
  const sav = emu.save_data();
  if (sav.length) await dbPut(`${romKey}:sav`, sav);
  const rtc = emu.rtc_data();
  if (rtc.length) await dbPut(`${romKey}:rtc`, rtc);
}

function scheduleSave() {
  if (state.saveTimer) return;
  state.saveTimer = setTimeout(() => {
    state.saveTimer = null;
    flushSave();
  }, 500);
}

// ---------------------------------------------------------------------------
// Audio: an AudioWorklet ring buffer at the emulator's native sample rate,
// with a ScriptProcessor fallback for browsers without worklets.

const audio = {
  ctx: null,
  node: null,
  rate: 0,
  queue: [],
  queued: 0,
};

async function ensureAudio(rate) {
  if (audio.ctx && audio.rate === rate) {
    if (audio.ctx.state === "suspended") audio.ctx.resume();
    return;
  }
  closeAudio();
  let ctx;
  try {
    ctx = new AudioContext({ sampleRate: rate });
  } catch {
    return;
  }
  audio.ctx = ctx;
  audio.rate = rate;
  if (ctx.audioWorklet) {
    try {
      await ctx.audioWorklet.addModule(new URL("./audio-worklet.js", import.meta.url));
      const node = new AudioWorkletNode(ctx, "crabboy-output", {
        outputChannelCount: [2],
      });
      node.connect(ctx.destination);
      audio.node = node;
      return;
    } catch {
      // fall through to the ScriptProcessor path
    }
  }
  const node = ctx.createScriptProcessor(2048, 0, 2);
  node.onaudioprocess = (e) => {
    const l = e.outputBuffer.getChannelData(0);
    const r = e.outputBuffer.getChannelData(1);
    let i = 0;
    while (i < l.length && audio.queue.length) {
      const chunk = audio.queue[0];
      const frames = chunk.length >> 1;
      const take = Math.min(frames - chunk.pos, l.length - i);
      for (let k = 0; k < take; k++, i++) {
        l[i] = chunk[(chunk.pos + k) * 2];
        r[i] = chunk[(chunk.pos + k) * 2 + 1];
      }
      chunk.pos += take;
      audio.queued -= take;
      if (chunk.pos >= frames) audio.queue.shift();
    }
    for (; i < l.length; i++) l[i] = r[i] = 0;
  };
  node.connect(ctx.destination);
  audio.node = node;
}

function pushAudio(samples) {
  if (!audio.ctx || state.muted || samples.length === 0) return;
  if (audio.node instanceof AudioWorkletNode) {
    audio.node.port.postMessage(samples, [samples.buffer]);
  } else {
    if (audio.queued > audio.rate / 4) return; // > 250 ms queued: drop
    samples.pos = 0;
    audio.queue.push(samples);
    audio.queued += samples.length >> 1;
  }
}

function clearAudio() {
  if (audio.node instanceof AudioWorkletNode) audio.node.port.postMessage("clear");
  audio.queue.length = 0;
  audio.queued = 0;
}

function closeAudio() {
  if (audio.ctx) audio.ctx.close();
  audio.ctx = null;
  audio.node = null;
  audio.rate = 0;
  audio.queue.length = 0;
  audio.queued = 0;
}

// ---------------------------------------------------------------------------
// Rendering and the main loop.

function draw() {
  const { emu } = state;
  const w = emu.width();
  const h = emu.height();
  const pixels = new Uint8ClampedArray(wasm.memory.buffer, emu.frame_ptr(), emu.frame_len());
  ctx.putImageData(new ImageData(pixels, w, h), 0, 0);
}

function fitCanvas() {
  const { emu } = state;
  if (!emu) return;
  const w = emu.width();
  const h = emu.height();
  canvas.width = w;
  canvas.height = h;
  // Largest integer scale that fits the viewport, at least 1x.
  const maxW = Math.min(window.innerWidth - 48, 960);
  const maxH = window.innerHeight * 0.6;
  const scale = Math.max(1, Math.floor(Math.min(maxW / w, maxH / h)));
  canvas.style.width = `${w * scale}px`;
  canvas.style.height = `${h * scale}px`;
}

function runFrame() {
  const { emu } = state;
  emu.run_frame();
  const samples = emu.take_audio();
  if (!state.fastForward) pushAudio(samples);
  if (emu.sram_changed()) scheduleSave();
}

function loop(now) {
  requestAnimationFrame(loop);
  const { emu } = state;
  if (!emu) return;
  pollGamepad();
  if (state.paused) return;
  const dt = Math.min((now - state.lastTime) / 1000, 0.25);
  state.lastTime = now;
  let frames = 0;
  if (state.fastForward) {
    frames = 8;
    state.accum = 0;
  } else {
    const period = 1 / emu.frame_rate();
    state.accum += dt;
    while (state.accum >= period && frames < 4) {
      state.accum -= period;
      frames++;
    }
    if (state.accum >= period) state.accum = 0; // too far behind: drop
  }
  if (frames === 0) return;
  for (let i = 0; i < frames; i++) runFrame();
  draw();
  state.fpsFrames += frames;
  if (now - state.fpsTime >= 1000) {
    state.fps = (state.fpsFrames * 1000) / (now - state.fpsTime);
    state.fpsFrames = 0;
    state.fpsTime = now;
    updateStatus();
  }
}

function setStatus(text) {
  status.textContent = text;
}

function updateStatus() {
  const { emu } = state;
  if (!emu) return setStatus("No ROM loaded");
  const parts = [`${state.title || emu.name().toUpperCase()} (${emu.name()})`];
  parts.push(state.paused ? "paused" : `${state.fps.toFixed(1)} fps`);
  if (state.fastForward) parts.push("fast-forward");
  if (state.muted) parts.push("muted");
  setStatus(parts.join(" · "));
}

// ---------------------------------------------------------------------------
// ROM loading.

async function loadRom(file) {
  const bytes = new Uint8Array(await file.arrayBuffer());
  const kind = detect(bytes);
  if (!kind) {
    setStatus(`${file.name} is not a Game Boy, Game Boy Color or GBA ROM`);
    return;
  }
  await flushSave();
  let emu;
  try {
    emu = new Emulator(bytes);
  } catch (e) {
    setStatus(`Failed to load ${file.name}: ${e}`);
    return;
  }
  if (state.emu) state.emu.free();
  state.emu = emu;
  state.title = emu.title();
  state.romKey = await romKeyFor(bytes);
  state.paused = false;
  state.fastForward = false;
  state.accum = 0;
  state.lastTime = performance.now();
  state.fpsTime = state.lastTime;
  state.fpsFrames = 0;
  releaseAll();

  if (emu.battery_backed()) {
    const sav = await dbGet(`${state.romKey}:sav`);
    if (sav) emu.load_data(sav);
    const rtc = await dbGet(`${state.romKey}:rtc`);
    if (rtc) emu.load_rtc(rtc);
  }

  shoulders.classList.toggle("hidden", kind !== "gba");
  dropHint.classList.add("hidden");
  for (const b of [pauseBtn, resetBtn, ffBtn, saveBtn, loadBtn, muteBtn]) b.disabled = false;
  pauseBtn.textContent = "Pause";
  ffBtn.classList.remove("active");
  fitCanvas();
  draw();
  clearAudio();
  await ensureAudio(emu.audio_rate());
  updateStatus();
}

// ---------------------------------------------------------------------------
// Input.

function setButton(name, pressed) {
  const { emu } = state;
  const index = BUTTON_INDEX[name];
  if (!emu || index === undefined) return;
  emu.set_button(index, pressed);
  for (const el of document.querySelectorAll(`.key[data-btn="${name}"]`)) {
    el.classList.toggle("pressed", pressed);
  }
}

function releaseAll() {
  for (const name of Object.keys(BUTTON_INDEX)) setButton(name, false);
  state.held.clear();
  state.gamepadHeld.clear();
}

function togglePause() {
  if (!state.emu) return;
  state.paused = !state.paused;
  pauseBtn.textContent = state.paused ? "Resume" : "Pause";
  if (state.paused) {
    clearAudio();
  } else {
    state.lastTime = performance.now();
    state.accum = 0;
    ensureAudio(state.emu.audio_rate());
  }
  updateStatus();
}

function setFastForward(on) {
  if (!state.emu || state.fastForward === on) return;
  state.fastForward = on;
  ffBtn.classList.toggle("active", on);
  if (on) clearAudio();
  updateStatus();
}

function resetEmu() {
  if (!state.emu) return;
  state.emu.reset();
  clearAudio();
  draw();
  setStatus("Reset");
}

async function quickSave() {
  const { emu, romKey } = state;
  if (!emu) return;
  const data = emu.save_state();
  if (!data.length) return setStatus("Save states are not supported for this system");
  await dbPut(`${romKey}:state`, data);
  setStatus("Quick save stored");
}

async function quickLoad() {
  const { emu, romKey } = state;
  if (!emu) return;
  const data = await dbGet(`${romKey}:state`);
  if (!data) return setStatus("No quick save for this ROM");
  try {
    emu.load_state(data);
    clearAudio();
    draw();
    setStatus("Quick save loaded");
  } catch (e) {
    setStatus(`Load failed: ${e}`);
  }
}

window.addEventListener("keydown", (e) => {
  if (e.repeat) return;
  const name = KEYMAP[e.code];
  if (name) {
    e.preventDefault();
    setButton(name, true);
    return;
  }
  switch (e.code) {
    case "KeyP":
      togglePause();
      break;
    case "KeyR":
      resetEmu();
      break;
    case "KeyF":
      setFastForward(true);
      break;
    case "F5":
      e.preventDefault();
      quickSave();
      break;
    case "F9":
      e.preventDefault();
      quickLoad();
      break;
    default:
      return;
  }
});

window.addEventListener("keyup", (e) => {
  const name = KEYMAP[e.code];
  if (name) {
    e.preventDefault();
    setButton(name, false);
  } else if (e.code === "KeyF") {
    setFastForward(false);
  }
});

window.addEventListener("blur", releaseAll);

for (const el of document.querySelectorAll(".key[data-btn]")) {
  const name = el.dataset.btn;
  const press = (e) => {
    e.preventDefault();
    el.setPointerCapture?.(e.pointerId);
    setButton(name, true);
    if (audio.ctx && audio.ctx.state === "suspended") audio.ctx.resume();
  };
  const release = (e) => {
    e.preventDefault();
    setButton(name, false);
  };
  el.addEventListener("pointerdown", press);
  el.addEventListener("pointerup", release);
  el.addEventListener("pointercancel", release);
  el.addEventListener("pointerleave", release);
  el.addEventListener("contextmenu", (e) => e.preventDefault());
}

function pollGamepad() {
  const pads = navigator.getGamepads ? navigator.getGamepads() : [];
  const pad = Array.from(pads).find((p) => p && p.connected);
  if (!pad) return;
  const now = new Set();
  pad.buttons.forEach((b, i) => {
    const name = GAMEPAD_BUTTONS[i];
    if (name && (b.pressed || b.value > 0.5)) now.add(name);
  });
  const [x, y] = pad.axes;
  if (x < -0.5) now.add("Left");
  if (x > 0.5) now.add("Right");
  if (y < -0.5) now.add("Up");
  if (y > 0.5) now.add("Down");
  for (const name of now) if (!state.gamepadHeld.has(name)) setButton(name, true);
  for (const name of state.gamepadHeld) if (!now.has(name)) setButton(name, false);
  state.gamepadHeld = now;
}

// ---------------------------------------------------------------------------
// UI wiring.

romInput.addEventListener("change", () => {
  const file = romInput.files && romInput.files[0];
  if (file) loadRom(file);
  romInput.value = "";
});

for (const ev of ["dragenter", "dragover"]) {
  window.addEventListener(ev, (e) => {
    e.preventDefault();
    screenWrap.classList.add("dragover");
  });
}
window.addEventListener("dragleave", (e) => {
  if (e.relatedTarget === null) screenWrap.classList.remove("dragover");
});
window.addEventListener("drop", (e) => {
  e.preventDefault();
  screenWrap.classList.remove("dragover");
  const file = e.dataTransfer && e.dataTransfer.files[0];
  if (file) loadRom(file);
});

pauseBtn.addEventListener("click", togglePause);
resetBtn.addEventListener("click", resetEmu);
ffBtn.addEventListener("click", () => setFastForward(!state.fastForward));
saveBtn.addEventListener("click", quickSave);
loadBtn.addEventListener("click", quickLoad);
muteBtn.addEventListener("click", () => {
  state.muted = !state.muted;
  muteBtn.textContent = state.muted ? "Unmute" : "Mute";
  muteBtn.classList.toggle("active", state.muted);
  if (state.muted) clearAudio();
  updateStatus();
});

// Browsers only start audio after a user gesture.
window.addEventListener("pointerdown", () => {
  if (audio.ctx && audio.ctx.state === "suspended") audio.ctx.resume();
});
window.addEventListener("resize", fitCanvas);
document.addEventListener("visibilitychange", () => {
  if (document.hidden) flushSave();
});
window.addEventListener("pagehide", flushSave);

requestAnimationFrame(loop);

// `?rom=<url>` auto-loads a ROM served next to the page (demo kiosks, tests).
const autoRom = new URLSearchParams(location.search).get("rom");
if (autoRom) {
  try {
    const res = await fetch(autoRom);
    if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
    const blob = await res.blob();
    await loadRom(new File([blob], autoRom.split("/").pop() || "rom"));
  } catch (e) {
    setStatus(`Could not fetch ${autoRom}: ${e}`);
  }
}
