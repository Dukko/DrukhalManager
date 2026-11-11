#!/usr/bin/env python3
"""Opens an issue to review the conflict rules when Stellaris is patched,
or when Irony Mod Manager's Stellaris rules, which ours follow, change.

The game-specific part of conflict resolution is three folder lists in
src/merge/conflicts.rs. Patch notes rarely say which folders keep the first
definition, so this compares our lists with Irony's and leaves the
decision to a person. Uses only the standard library and the `gh` CLI.

DRY_RUN=1 prints the issues instead of opening them.
"""

import json
import os
import re
import subprocess
import sys
import urllib.request

STELLARIS = 281990
NEWS = f"https://api.steampowered.com/ISteamNews/GetNewsForApp/v2/?appid={STELLARIS}&count=30&maxlength=300&feeds=steam_community_announcements"
IRONY_REPO = "bcssov/IronyModManager"
IRONY_FILE = "src/IronyModManager.IO/Mods/InfoProviders/StellarisDefinitionInfoProvider.cs"
IRONY_RAW = f"https://raw.githubusercontent.com/{IRONY_REPO}/master/{IRONY_FILE}"
IRONY_PAGE = f"https://github.com/{IRONY_REPO}/blob/master/{IRONY_FILE}"
OURS = "src/merge/conflicts.rs"
LABEL = "game-update"
RULES_TITLE = "Conflict rules differ from Irony Mod Manager's"
DRY_RUN = os.environ.get("DRY_RUN") == "1"


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "DrukhalManager game update watch"})
    with urllib.request.urlopen(request, timeout=30) as reply:
        return reply.read().decode("utf-8")


def released_patches(news):
    """(version, title, url, unix time) for "patch released" announcements,
    newest first. Experimental branches and dev diaries are left out."""
    found = []
    for item in news["appnews"]["newsitems"]:
        m = re.search(r"Stellaris (\d+\.\d+(?:\.\d+)*) .*patch released", item["title"], re.IGNORECASE)
        if m and "experimental" not in item["title"].lower():
            found.append((m.group(1), item["title"], item["url"], item["date"]))
    return found


def irony_rules(source):
    """Irony's first-in-only-served folders and flat-merged folders, as
    paths like ours (`common/traits`, `events`)."""

    def path(name):
        name = name.replace("\\\\", "/").replace("\\", "/")
        return name if name == "events" else f"common/{name}"

    fios = re.search(r"FIOSPaths\s*=>\s*\[(.*?)\];", source, re.S)
    merges = re.search(r"MergeTypes\s*=>(.*?);", source, re.S)
    if not fios:
        raise SystemExit("Irony's FIOSPaths list wasn't found; its file may have moved or changed shape")
    return (
        {path(n) for n in re.findall(r'"([^"]+)"', fios.group(1))},
        {path(n) for n in re.findall(r'"([^"]+)"', merges.group(1))} if merges else set(),
    )


def our_rules(source):
    """Our first-in-only-served and merged folder lists."""

    def listed(name):
        m = re.search(rf"const {name}: &\[&str\] = &\[(.*?)\];", source, re.S)
        if not m:
            raise SystemExit(f"{name} wasn't found in {OURS}")
        return set(re.findall(r'"([^"]+)"', m.group(1)))

    return listed("FIOS_FOLDERS"), listed("MERGED_FOLDERS")


def compare():
    """Markdown describing how our rules differ from Irony's; empty if they
    don't."""
    theirs_fios, theirs_merged = irony_rules(fetch(IRONY_RAW))
    with open(OURS, encoding="utf-8") as f:
        ours_fios, ours_merged = our_rules(f.read())
    lines = []
    for title, items in [
        ("First-in-only-served in Irony, not here (`FIOS_FOLDERS`)", theirs_fios - ours_fios),
        ("First-in-only-served here, not in Irony", ours_fios - theirs_fios),
        ("Merged by Irony, not here (`MERGED_FOLDERS`)", theirs_merged - ours_merged),
    ]:
        if items:
            lines.append(f"**{title}:**")
            lines.extend(f"- `{i}`" for i in sorted(items))
            lines.append("")
    return "\n".join(lines)


def gh(*args):
    return subprocess.run(["gh", *args], check=True, capture_output=True, text=True).stdout


def issue_exists(title):
    """Whether an issue with exactly this title was ever opened."""
    found = json.loads(gh("issue", "list", "--state", "all", "--search", f'"{title}" in:title', "--json", "title", "--limit", "50"))
    return any(i["title"] == title for i in found)


def open_issue(title, body):
    if DRY_RUN:
        print(f"--- would open: {title}\n{body}\n")
        return
    if issue_exists(title):
        print(f"Already filed: {title}")
        return
    print(gh("issue", "create", "--title", title, "--body", body, "--label", LABEL).strip())


CHECKLIST = """\
- [ ] Read the patch notes for changes to how folders load: definitions that now merge, or keep the first one read.
- [ ] Compare the folder lists in `src/merge/conflicts.rs` with Irony's (above).
- [ ] Launch the game with the merged patch and check `error.log` for new errors from merged files.
- [ ] Check that Smart Sort still recognises mods that use new folders (`src/mods/smart_sort.rs`).
"""


def main():
    differences = compare()
    rules_note = differences.strip() or "Our folder rules match Irony Mod Manager's."

    patches = released_patches(json.loads(fetch(NEWS)))
    if patches:
        # Only the newest: older ones were reviewed, or are long past.
        version, title, url, _ = patches[0]
        body = (
            f"[{title}]({url}) is out.\n\n"
            f"### Conflict rules\n\n{rules_note}\n\n"
            f"Irony's rules: [{IRONY_FILE.rsplit('/', 1)[-1]}]({IRONY_PAGE})\n\n"
            f"### Review\n\n{CHECKLIST}"
        )
        open_issue(f"Stellaris {version} released: review the conflict rules", body)
    else:
        print("No patch announcements found")

    if differences:
        body = (
            f"Irony Mod Manager's [Stellaris rules]({IRONY_PAGE}), which `src/merge/conflicts.rs` follows, "
            f"don't match ours:\n\n{differences}\n"
            "Check Irony's history for why, and update our lists (or note why not) before closing this."
        )
        open_issue(RULES_TITLE, body)
    return 0


if __name__ == "__main__":
    sys.exit(main())
