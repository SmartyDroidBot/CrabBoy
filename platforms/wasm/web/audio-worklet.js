// Ring-buffer audio output for CrabBoy.
//
// The main thread posts interleaved stereo Float32Array chunks; this
// processor plays them back, outputting silence on underrun and dropping
// the oldest samples if the buffer overflows (so a stalled tab never builds
// up seconds of latency).

class CrabBoyOutput extends AudioWorkletProcessor {
  constructor() {
    super();
    // ~250 ms of stereo audio at 32768 Hz.
    this.capacity = 16384;
    this.buffer = new Float32Array(this.capacity * 2);
    this.read = 0;
    this.write = 0;
    this.size = 0;
    this.port.onmessage = (e) => {
      if (e.data === "clear") {
        this.read = this.write = this.size = 0;
        return;
      }
      this.push(e.data);
    };
  }

  push(samples) {
    const frames = samples.length >> 1;
    if (frames > this.capacity) return;
    // Drop the oldest audio to make room.
    const overflow = this.size + frames - this.capacity;
    if (overflow > 0) {
      this.read = (this.read + overflow) % this.capacity;
      this.size -= overflow;
    }
    for (let i = 0; i < frames; i++) {
      const w = this.write * 2;
      this.buffer[w] = samples[i * 2];
      this.buffer[w + 1] = samples[i * 2 + 1];
      this.write = (this.write + 1) % this.capacity;
    }
    this.size += frames;
  }

  process(_inputs, outputs) {
    const out = outputs[0];
    const left = out[0];
    const right = out[1] || out[0];
    const n = left.length;
    for (let i = 0; i < n; i++) {
      if (this.size > 0) {
        const r = this.read * 2;
        left[i] = this.buffer[r];
        right[i] = this.buffer[r + 1];
        this.read = (this.read + 1) % this.capacity;
        this.size--;
      } else {
        left[i] = 0;
        right[i] = 0;
      }
    }
    return true;
  }
}

registerProcessor("crabboy-output", CrabBoyOutput);
