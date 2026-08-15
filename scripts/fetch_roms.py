#!/usr/bin/env python3
"""Fetch prebuilt mooneye Test Suite ROMs into a directory.

Downloads the latest prebuilt build from gekkio.fi and extracts the ROMs so
`run_accuracy` can run them.

Usage:
    python fetch_roms.py <dest-dir>
"""
import os
import re
import sys
import urllib.request
import zipfile

INDEX_URL = "https://gekkio.fi/files/mooneye-test-suite/"


def read(url: str) -> bytes:
    req = urllib.request.Request(url, headers={"User-Agent": "crabboy-fetch"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read()


def main() -> int:
    if len(sys.argv) < 2:
        print(__doc__)
        return 2
    dest = sys.argv[1]
    os.makedirs(dest, exist_ok=True)

    index = read(INDEX_URL).decode("utf-8", "replace")
    builds = sorted(set(re.findall(r"mts-\d{8}-\d{4}-[0-9a-f]+/", index)))
    if not builds:
        print("could not find any builds at", INDEX_URL, file=sys.stderr)
        return 1
    latest = builds[-1].rstrip("/")
    print(f"latest build: {latest}")

    base = f"{INDEX_URL}{latest}/"
    zip_url = f"{base}{latest}.zip"
    print(f"downloading {zip_url} ...")
    data = read(zip_url)
    zip_path = os.path.join(dest, f"{latest}.zip")
    with open(zip_path, "wb") as f:
        f.write(data)

    with zipfile.ZipFile(zip_path) as z:
        names = [n for n in z.namelist() if not n.endswith("/")]
        roms = [n for n in names if n.lower().endswith(".gb")]
        z.extractall(dest)
    print(f"extracted {len(roms)} ROMs to {dest}")
    print(f"archive kept at {zip_path} (delete after use if desired)")
    return 0


if __name__ == "__main__":
    sys.exit(main())