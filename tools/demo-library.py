#!/usr/bin/env python3
"""Makes a pretend Stellaris setup to run the app against, for screenshots
and trying the interface without the game:

    python3 tools/demo-library.py /tmp/drukhal-demo
    HOME=/tmp/drukhal-demo/home XDG_CONFIG_HOME= XDG_DATA_HOME= cargo run

It writes a home folder with the app's settings pointing at a game data
folder (mod descriptors, their files, dlc_load.json) and a stand-in game
executable whose launcher-settings.json says it's Stellaris v4.1.2.
"""

import json
import shutil
import sys
from pathlib import Path

MODS = [
    # name, supported version, enabled, tags, dependencies
    ("UI Overhaul Dynamic", "v4.1.*", True, ["Graphics", "Fixes"], []),
    ("Gigastructural Engineering & More", "v4.1.*", True, ["Gameplay", "Buildings"], ["UI Overhaul Dynamic"]),
    ("Unofficial Patch Collection", "v4.0.*", True, ["Fixes"], []),
    ("Guilli's Planet Modifiers", "v4.1.*", True, ["Gameplay"], []),
    ("Expanded Stellaris Traditions", "v4.1.*", True, ["Gameplay", "Balance"], []),
    ("Ethics & Civics Classic", "v4.1.*", True, ["Gameplay", "Government"], []),
    ("Ancient Cache of Technologies", "v4.0.*", True, ["Technologies"], []),
    ("NSC3 - New Ship Classes", "v4.1.*", True, ["Military", "Spaceships"], ["UI Overhaul Dynamic"]),
    ("Real Space 4.0", "v4.1.*", True, ["Graphics"], []),
    ("Dark UI", "v4.1.*", False, ["Graphics", "Interface"], ["UI Overhaul Dynamic"]),
    ("Starnet AI", "v4.1.*", False, ["AI"], []),
    ("Planetary Diversity", "v3.14.*", False, ["Graphics", "Gameplay"], []),
]


def main(root: Path) -> None:
    if root.exists():
        shutil.rmtree(root)
    home = root / "home"
    user = home / ".local/share/Paradox Interactive/Stellaris"
    (user / "mod").mkdir(parents=True)
    workshop = root / "workshop"
    enabled = []
    for n, (name, version, on, tags, deps) in enumerate(MODS):
        steam_id = 2_100_000_000 + n
        content = workshop / str(steam_id)
        (content / "common/buildings").mkdir(parents=True)
        (content / "common/buildings/demo.txt").write_text(f"demo_building_{n} = {{}}\n")
        body = (
            f'name="{name}"\n'
            f'supported_version="{version}"\n'
            f'version="1.{n}"\n'
            "tags={\n" + "".join(f'\t"{t}"\n' for t in tags) + "}\n"
            + ("dependencies={\n" + "".join(f'\t"{d}"\n' for d in deps) + "}\n" if deps else "")
            + f'remote_file_id="{steam_id}"\n'
        )
        (content / "descriptor.mod").write_text(body)
        (user / f"mod/ugc_{steam_id}.mod").write_text(body + f'path="{content}"\n')
        if on:
            enabled.append(f"mod/ugc_{steam_id}.mod")
    (user / "dlc_load.json").write_text(json.dumps({"enabled_mods": enabled, "disabled_dlcs": []}))

    game = root / "game"
    game.mkdir()
    exe = game / "stellaris"
    exe.write_text("#!/bin/sh\necho \"Stellaris (pretend) started with: $*\"\n")
    exe.chmod(0o755)
    (game / "launcher-settings.json").write_text(json.dumps({"exePath": "./stellaris", "exeArgs": [], "rawVersion": "v4.1.2"}))

    config = home / ".config/DrukhalManager"
    config.mkdir(parents=True)
    (config / "settings.json").write_text(json.dumps({"game": {"executable": str(exe), "data_dir": str(user)}}))
    print(f"HOME={home} XDG_CONFIG_HOME= XDG_DATA_HOME= XDG_STATE_HOME=")


if __name__ == "__main__":
    main(Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/drukhal-demo"))
