// Headless smoke test for the CrabBoy wasm bindings.
//
// Runs a ROM for N frames with the same scripted-input grammar as the native
// `crab run` and prints the FNV-1a-32 hash of the final RGBA frame in the
// same `hash @f<N>: <hex>` format, so `crab run --hash` and this script must
// agree bit for bit.
//
// Build the Node package first (from the repo root):
//   cargo build -p crab-wasm --release --target wasm32-unknown-unknown
//   wasm-bindgen --target nodejs --out-dir target/wasm-node \
//     target/wasm32-unknown-unknown/release/crab_wasm.wasm
// Then:
//   node platforms/wasm/tests/run.js <rom> [--frames N] [--input SPEC]
//                                           [--dump out.ppm] [--hash] [--pkg DIR]

const fs = require("fs");
const path = require("path");

function usage(code) {
  console.error(
    "usage: node run.js <rom> [--frames N] [--input SPEC] [--dump out.ppm] [--hash] [--pkg DIR]"
  );
  process.exit(code);
}

const args = process.argv.slice(2);
let rom = null;
let frames = 600;
let input = "";
let dump = null;
let hash = false;
let pkg = path.resolve(__dirname, "../../../target/wasm-node/crab_wasm.js");
for (let i = 0; i < args.length; i++) {
  const a = args[i];
  const value = () => {
    if (i + 1 >= args.length) usage(2);
    return args[++i];
  };
  if (a === "--frames") frames = parseInt(value(), 10);
  else if (a === "--input") input = value();
  else if (a === "--dump") dump = value();
  else if (a === "--hash") hash = true;
  else if (a === "--pkg") pkg = path.resolve(value());
  else if (a === "-h" || a === "--help") usage(0);
  else if (a.startsWith("-")) usage(2);
  else if (rom === null) rom = a;
  else usage(2);
}
if (rom === null || !Number.isFinite(frames)) usage(2);

const { Emulator, button_names } = require(pkg);
const BUTTONS = button_names()
  .split(",")
  .map((n) => n.toUpperCase());

// `START@400,!START@410` (also `;` separators and `RELEASE START@410`).
function parseScript(spec) {
  const out = [];
  for (const raw of spec.split(/[;,]/)) {
    const item = raw.trim();
    if (!item) continue;
    const at = item.lastIndexOf("@");
    if (at < 0) throw new Error(`bad input event ${JSON.stringify(item)}`);
    let name = item.slice(0, at).trim();
    const frame = parseInt(item.slice(at + 1).trim(), 10);
    let pressed = true;
    if (name.startsWith("!")) {
      pressed = false;
      name = name.slice(1);
    } else if (name.startsWith("RELEASE")) {
      pressed = false;
      name = name.slice("RELEASE".length);
    }
    const index = BUTTONS.indexOf(name.trim().toUpperCase());
    if (index < 0) throw new Error(`unknown button ${JSON.stringify(name)}`);
    out.push({ frame, index, pressed });
  }
  out.sort((a, b) => a.frame - b.frame);
  return out;
}

function fnv1a32(bytes) {
  let h = 0x811c9dc5;
  for (const b of bytes) {
    h ^= b;
    h = Math.imul(h, 0x01000193) >>> 0;
  }
  return h >>> 0;
}

const emu = new Emulator(fs.readFileSync(rom));
const script = parseScript(input);
let next = 0;
for (let f = 0; f < frames; f++) {
  while (next < script.length && script[next].frame <= f) {
    emu.set_button(script[next].index, script[next].pressed);
    next++;
  }
  emu.run_frame();
  emu.take_audio();
}

const rgba = emu.frame_rgba();
if (dump) {
  const w = emu.width();
  const h = emu.height();
  const rgb = Buffer.alloc(w * h * 3);
  for (let i = 0, j = 0; i < rgba.length; i += 4, j += 3) {
    rgb[j] = rgba[i];
    rgb[j + 1] = rgba[i + 1];
    rgb[j + 2] = rgba[i + 2];
  }
  fs.mkdirSync(path.dirname(path.resolve(dump)), { recursive: true });
  fs.writeFileSync(dump, Buffer.concat([Buffer.from(`P6\n${w} ${h}\n255\n`), rgb]));
  console.log(`dumped frame ${frames} to ${dump}`);
}
if (hash) {
  console.log(`hash @f${frames}: ${fnv1a32(rgba).toString(16).padStart(8, "0")}`);
}
console.log(`ran ${frames} frames of ${emu.title()} (${emu.name()})`);
