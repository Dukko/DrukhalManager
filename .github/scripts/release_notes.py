"""Prints a version's release notes, from its <release> in the metainfo, as
Markdown for the GitHub release. The app shows the same text on its Updates
page with the ** removed, so it has to read well as plain text too.

    python3 release_notes.py 0.8.0 [metainfo.xml]
"""

import sys
import xml.etree.ElementTree as ET

METAINFO = "data/io.github.dukko.DrukhalManager.metainfo.xml"


def inline(el):
    """An element's text, with <em> in bold and <code> in backticks."""
    parts = [el.text or ""]
    for child in el:
        inner = inline(child)
        if child.tag == "em":
            inner = f"**{inner.strip()}**"
        elif child.tag == "code":
            inner = f"`{inner.strip()}`"
        parts.append(inner)
        parts.append(child.tail or "")
    return " ".join("".join(parts).split())


def notes(version, path=METAINFO):
    for release in ET.parse(path).getroot().iter("release"):
        if release.get("version") != version:
            continue
        blocks = []
        for el in release.find("description") if release.find("description") is not None else []:
            if el.tag == "p":
                blocks.append(inline(el))
            elif el.tag in ("ul", "ol"):
                bullet = "-" if el.tag == "ul" else "1."
                blocks.append("\n".join(f"{bullet} {inline(li)}" for li in el.iter("li")))
        return "\n\n".join(b for b in blocks if b)
    return ""


if __name__ == "__main__":
    text = notes(*sys.argv[1:3])
    if text:
        print(text)
