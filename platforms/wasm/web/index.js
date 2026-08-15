import init, { Gb } from "./pkg/crab_wasm.js";

const W = 160;
const H = 144;

const canvas = document.getElementById("screen");
const ctx = canvas.getContext("2d");
const romInput = document.getElementById("rom-input");
const pauseBtn = document.getElementById("pause-btn");
const hint = document.querySelector(".hint");

// GB button bitmasks (match gb_core::joypad).
const BUTTON_A = 0x01;
const BUTTON_B = 0x02;
const BUTTON_SELECT = 0x04;
const BUTTON_START = 0x08;
const BUTTON_RIGHT = 0x10;
const BUTTON_LEFT = 0x20;
const BUTTON_UP = 0x40;
const BUTTON_DOWN = 0x80;

// 2-bit shade (0..=3) -> RGB palette (classic green screen).
const PALETTE = [
  [0x9b, 0xbc, 0x0f], // 0: lightest
  [0x8b, 0xac, 0x0f], // 1
  [0x30, 0x62, 0x30], // 2
  [0x0f, 0x38, 0x0f], // 3: darkest
];

const imgData = ctx.createImageData(W, H);
const data = imgData.data;

let gb = null;
let running = false;
let rafId = null;

function drawFrame() {
  if (!gb) return;
  const fb = gb.framebuffer();
  for (let i = 0; i < fb.length; i++) {
    const [r, g, b] = PALETTE[fb[i] & 0x03];
    const j = i * 4;
    data[j] = r;
    data[j + 1] = g;
    data[j + 2] = b;
    data[j + 3] = 0xff;
  }
  ctx.putImageData(imgData, 0, 0);
}

function loop() {
  if (!running) return;
  gb.step_frame();
  drawFrame();
  rafId = requestAnimationFrame(loop);
}

function start() {
  if (running || !gb) return;
  running = true;
  pauseBtn.textContent = "Pause";
  rafId = requestAnimationFrame(loop);
}

function pause() {
  if (!running) return;
  running = false;
  if (rafId !== null) {
    cancelAnimationFrame(rafId);
    rafId = null;
  }
  pauseBtn.textContent = "Resume";
}

function setButton(mask, pressed) {
  if (gb) gb.set_button(mask, pressed);
}

romInput.addEventListener("change", async () => {
  const file = romInput.files && romInput.files[0];
  if (!file) return;
  const bytes = new Uint8Array(await file.arrayBuffer());
  try {
    gb = new Gb(bytes);
  } catch (err) {
    hint.textContent = "Failed to load ROM: " + err;
    return;
  }
  hint.textContent =
    "Arrows = D-pad · Z = A · X = B · Enter = Start · Backspace = Select";
  pause();
  drawFrame();
  start();
});

pauseBtn.addEventListener("click", () => {
  if (running) pause();
  else start();
});

const KEYMAP = {
  ArrowUp: BUTTON_UP,
  ArrowDown: BUTTON_DOWN,
  ArrowLeft: BUTTON_LEFT,
  ArrowRight: BUTTON_RIGHT,
  z: BUTTON_A,
  x: BUTTON_B,
  Enter: BUTTON_START,
  Backspace: BUTTON_SELECT,
};

window.addEventListener("keydown", (e) => {
  const mask = KEYMAP[e.key];
  if (mask) {
    e.preventDefault();
    setButton(mask, true);
  }
});

window.addEventListener("keyup", (e) => {
  const mask = KEYMAP[e.key];
  if (mask) {
    e.preventDefault();
    setButton(mask, false);
  }
});

// Boot the wasm module before anything can be used.
await init();