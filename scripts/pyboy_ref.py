#!/usr/bin/env python3
"""Generate a reference frame from PyBoy for differential PPU testing.

Writes a grayscale PPM (P3) of the given frame, mapping each pixel to one of the
four shade levels [0, 85, 170, 255] ranked by luminance, so it can be compared
byte-for-byte against `test_runner --grayscale`.

Usage:
    python pyboy_ref.py <rom> <frame> <out.ppm> [button@frame [button@frame ...]]

Buttons: A B SELECT START RIGHT LEFT UP DOWN (e.g. START@400). Buttons are held
from their frame onward (released with "R:NAME@frame").
"""
import sys

from pyboy import PyBoy

SHADES = [0, 85, 170, 255]


def parse_buttons(script: list[str], frames: int) -> list[tuple[int, str, bool]]:
    held: list[str] = []
    events: list[tuple[int, str, bool]] = []
    for ev in script:
        name, frame = ev.rsplit("@", 1)
        frame = int(frame)
        if name.startswith("R:"):
            events.append((frame, name[2:], False))
        else:
            events.append((frame, name.upper(), True))
    return events


def main() -> int:
    if len(sys.argv) < 4:
        print(__doc__)
        return 2
    rom, frame, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
    buttons = parse_buttons(sys.argv[4:], frame)

    pyboy = PyBoy(rom, window="null")
    try:
        events = sorted(buttons, key=lambda e: e[0])
        for f in range(frame + 1):
            for (ef, name, press) in events:
                if ef == f:
                    pyboy.button(name, press)
            pyboy.tick()
            if f == frame:
                rgba = pyboy.screen.ndarray  # (144, 160, 4) uint8
                break
    finally:
        pyboy.stop()

    # Discover the rendered palette colors and rank them by luminance.
    colors = {}
    for y in range(144):
        for x in range(160):
            colors[tuple(rgba[y, x][:3])] = True
    palette = sorted(colors.keys(), key=lambda c: 0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2])
    lut = {color: i for i, color in enumerate(palette)}
    if len(lut) > 4:
        print(f"warning: {len(lut)} distinct colors (DMG should be <=4); mapping will be lossy", file=sys.stderr)

    with open(out, "w") as f:
        f.write("P3\n160 144\n255\n")
        for y in range(144):
            row = []
            for x in range(160):
                key = tuple(rgba[y, x][:3])
                shade = SHADES[lut.get(key, 0) % 4]
                row.append(f"{shade} {shade} {shade}")
            f.write(" ".join(row) + "\n")
    print(f"wrote {out} (frame {frame}, {len(lut)} palette colors)")
    return 0


if __name__ == "__main__":
    sys.exit(main())