#!/usr/bin/env python3
"""Writes cargo-sources.json for the Flatpak build from Cargo.lock.

Flatpak builds run offline, so every crate is listed as a source to
download ahead of time (checked against Cargo.lock's checksums) and unpacked
into a vendor directory that Cargo is pointed at. This produces the same
layout as flatpak-builder-tools' flatpak-cargo-generator for crates.io
dependencies, which is all DrukhalManager uses.

Run after changing dependencies:

    python3 flatpak/generate-cargo-sources.py Cargo.lock flatpak/cargo-sources.json
"""

import json
import sys
import tomllib

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR = "cargo/vendor"


def main(lock_path: str, out_path: str) -> None:
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)

    sources = []
    for package in lock.get("package", []):
        source = package.get("source")
        if source is None:
            continue  # the app itself
        if source != CRATES_IO:
            sys.exit(f"{package['name']} comes from {source}; only crates.io dependencies are supported")
        name, version, checksum = package["name"], package["version"], package["checksum"]
        dest = f"{VENDOR}/{name}-{version}"
        sources.append({
            "type": "archive",
            "archive-type": "tar-gzip",
            "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
            "sha256": checksum,
            "dest": dest,
        })
        # Cargo checks vendored crates against this file.
        sources.append({
            "type": "inline",
            "contents": json.dumps({"package": checksum, "files": {}}),
            "dest": dest,
            "dest-filename": ".cargo-checksum.json",
        })

    sources.append({
        "type": "inline",
        "contents": f'[source.vendored-sources]\ndirectory = "{VENDOR}"\n\n[source.crates-io]\nreplace-with = "vendored-sources"\n',
        "dest": "cargo",
        "dest-filename": "config.toml",
    })

    with open(out_path, "w") as f:
        json.dump(sources, f, indent=4)
        f.write("\n")
    print(f"{len(sources) // 2} crates written to {out_path}")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
