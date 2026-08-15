// WASM smoke test for CrabBoy against the salvaged Pokémon Red ROM.
// Loads Red.gb, runs the same input script + frame count as the native
// test_runner run (GB_INPUT="START@400", GB_FRAMES=560), dumps the frame-300
// framebuffer as a PPM, and prints a framebuffer hash for cross-checking
// against native output.
//
// Run from the repo root:
//   node platforms/wasm/tests/run.js <path-to-Red.gb> <out-dir>

const fs = require("fs");
const path = require("path");

const { Gb } = require(path.resolve(__dirname, "../../../target/wasm-pkg/crab_wasm.js"));

const romPath = process.argv[2];
const outDir = process.argv[3] || "target/wasm-pkg/out";

if (!romPath) {
  console.error("usage: node run.js <Red.gb> [out-dir]");
  process.exit(2);
}

function parseScript(s) {
  const out = [];
  for (const raw of s.split(/[;,]/)) {
    const ev = raw.trim();
    if (!ev) continue;
    const at = ev.lastIndexOf("@");
    const name = ev.slice(0, at).trim();
    const frame = parseInt(ev.slice(at + 1).trim(), 10);
    const press = !name.startsWith("RELEASE");
    const btnName = press ? name.trim() : name.slice("RELEASE".length).trim();
    out.push({ frame, press, btn: btnName });
  }
  return out;
}

const BUTTONS = { A: 1, B: 2, SELECT: 4, START: 8, RIGHT: 0x10, LEFT: 0x20, UP: 0x40, DOWN: 0x80 };
const shadeToRgb = (s) =>
  [[0xe0, 0xf8, 0xd0], [0x88, 0xc0, 0x70], [0x34, 0x68, 0x56], [0x08, 0x18, 0x20]][s & 3];

function main() {
  const rom = fs.readFileSync(romPath);
  const gb = new Gb(rom);

  const GB_FRAMES = parseInt(process.env.GB_FRAMES || "560", 10);
  const GB_DUMP_FRAME = parseInt(process.env.GB_DUMP_FRAME || "300", 10);
  const script = parseScript(process.env.GB_INPUT || "START@400");

  let held = [];
  let lastDump = null;

  for (let f = 0; f < GB_FRAMES; f++) {
    for (const e of script) {
      if (e.frame === f) {
        const mask = BUTTONS[e.btn];
        if (!mask) {
          if (!e.press) {
            // sentinel release-all
            for (const b of held) gb.set_button(b, false);
            held = [];
          }
        } else if (e.press) {
          gb.set_button(mask, true);
          held.push(mask);
        } else {
          gb.set_button(mask, false);
          held = held.filter((b) => b !== mask);
        }
      }
    }
    gb.step_frame();
    if (f === GB_DUMP_FRAME) {
      lastDump = gb.framebuffer();
    }
  }

  if (!lastDump) {
    console.error("dump frame not reached");
    process.exit(1);
  }

  // Write PPM (identical format to the native dump_ppm).
  fs.mkdirSync(outDir, { recursive: true });
  const ppmPath = path.join(outDir, `wasm_f${GB_DUMP_FRAME}.ppm`);
  const lines = ["P3", "160 144", "255"];
  let i = 0;
  for (let y = 0; y < 144; y++) {
    let row = "";
    for (let x = 0; x < 160; x++) {
      const [r, g, b] = shadeToRgb(lastDump[i++]);
      row += `${r} ${g} ${b} `;
    }
    lines.push(row.trimEnd());
  }
  fs.writeFileSync(ppmPath, lines.join("\n") + "\n");

  // Print a stable framebuffer hash (same pixels as the PPM).
  let hash = 0;
  for (const s of lastDump) hash = (hash * 31 + s) >>> 0;
  console.log(`WASM framebuffer hash @f${GB_DUMP_FRAME}: ${hash.toString(16)}`);
  console.log(`WASM ppm written: ${ppmPath}`);
}

main();