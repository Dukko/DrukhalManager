# Native UI prototype (iced)

A GTK-free mockup of DrukhalManager's main window in
[iced](https://iced.rs) 0.14, the toolkit COSMIC is built on. It shows
sample data only; it doesn't read your mods.

## Two modes that don't overlap

The app splits into two halves, switched from the header (Ctrl+1 / Ctrl+2):

- **Mods** is for everyone: the load order, playsets and collections,
  launching and the game's logs. It never mentions conflicts: no counts,
  no win/lose bars, no merge buttons. Someone who never resolves a
  conflict can use the whole app from here.
- **Conflicts** is for those who want it: the queue the last scan found,
  each conflict's versions side by side with the one the game uses
  marked, and keeping a version, merging with AI or ignoring it. The
  merged patches and ignore rules live here too.

The mode switch carries no badge, so the Mods side doesn't nudge anyone
towards conflicts. Ctrl+K opens a command palette with only the current
mode's commands, plus one to go to the other mode.

## Building a merge, and reordering

- In Conflicts, the version the game uses is the merge being built.
  Right-click a highlighted block in another version for **Add above** or
  **Add below**: it's inserted before or after the merge's matching block
  (the menu says at which line). Undo and Start over are in the action
  bar, and Save merge finishes the conflict.
- In Mods, drag a mod by its handle. A line shows where it will land, its
  old place is outlined, and dropping it into another category's group
  moves it into that category. Escape cancels. Dragging is off while
  searching, since hidden rows would make a drop land somewhere unseen.

## Colours

`palette/` reads the same sources the GTK app does, in the same order, and
reloads them live:

1. `~/.config/gtk-4.0/gtk.css`: the `@define-color` lines matugen,
   DankMaterialShell or Gradience write (`accent_bg_color`,
   `window_bg_color`, …).
2. Omarchy's active theme: `~/.local/state/omarchy/current/theme/` or
   `~/.config/omarchy/current/theme/` (`colors.toml`, `light.mode`).
3. A built-in Material 3 dark palette.

The tonal surfaces are worked out from those as `src/theme/material.css`
does, and the fonts and Material icons are the GTK app's own.

## Screenshots

| | Mods | Conflicts |
| --- | --- | --- |
| Omarchy Tokyo Night | ![](shots/mods-tokyo-night.png) | ![](shots/conflicts-tokyo-night.png) |
| Omarchy Catppuccin Latte | ![](shots/mods-catppuccin-latte.png) | ![](shots/conflicts-catppuccin-latte.png) |
| Command palette | ![](shots/mods-matugen-commands.png) (matugen) | ![](shots/conflicts-gruvbox-commands.png) (Omarchy Gruvbox) |
| Dragging / Add above or below | ![](shots/mods-dragging.png) | ![](shots/conflicts-add-menu.png) |
| A block added | | ![](shots/conflicts-added.png) |

## Running

```bash
cd prototypes/iced-workbench && cargo run --release
```

- `--mode conflicts` starts in the Conflicts mode.
- `--palette <file>` uses a specific `gtk.css`, `colors.toml` or Omarchy
  theme folder instead of the desktop's; `samples/` has a few.
- `--commands` opens the command palette.
- `--screenshot <out.png>` saves the first frame and quits; with it,
  `--show menu|added|drag` sets up the context menu, an added block or a
  drag under way.

Without a GPU (a VM, CI), `ICED_BACKEND=tiny-skia` uses the software
renderer.
