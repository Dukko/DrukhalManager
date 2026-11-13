# Load Order and Smart Sort

## Moving mods

* Drag a row by its handle, or type a new position into its number.
* Right-click a mod for **Move to Top**, **Move to Bottom**, its Workshop page, a note and more.
* **Undo** and **Redo** are the arrow buttons at the top left (Ctrl+Z and Ctrl+Shift+Z).

## Changing several mods at once

Ctrl+click mods to select several, or Shift+click to select a range. A plain click selects just that mod again. They can be in both the enabled and disabled lists. While a view, category or search narrows the list, **Select All Shown** next to the Load Order heading selects everything listed, such as every mod not updated for your game.

With two or more selected, a bar at the bottom changes them all:

* **Enable** and **Disable**.
* The arrow buttons **Move to Top** and **Move to Bottom**, keeping their order among themselves.
* In the **⋯** menu, **Category** puts them in a Smart Sort section, or lets Smart Sort decide again, and **Add to Collection** adds them to another collection, without enabling them here.

Each change is one step to undo. Press Escape or ✕ to clear the selection.

## Smart Sort

**Smart Sort** puts mods in the order community guides suggest:

1. unofficial patches (names starting with `~`)
2. graphics and sound
3. flavor and cosmetics
4. shipsets
5. galaxy generation
6. gameplay
7. large overhauls
8. compatibility patches
9. UI mods
10. UI patches
11. universal patches (names starting with `!`)

Sub-mods go under the mod they belong to, and every mod goes after its dependencies. It decides from a mod's name, its dependencies and the folders it ships, because Workshop tags are often wrong.

You see a preview before anything moves. If a mod lands in the wrong section, move it by hand and Smart Sort remembers.

The arrow next to Smart Sort has **Sort A–Z, Dependencies First**, and a switch for the category headers in the list.

## The health strip

The cards above the list count what needs a look:

* **Not updated for your game version.** **Show** lists them. **Disable for Now…** turns them off until their authors catch up, after keeping your full load order as a collection called "Before Stellaris vX.Y", so you can switch back in one step.
* **Required mods missing.** **Show** lists the mods that need something you don't have.
* **Loading before a dependency.** **Sort** fixes the order.
* **Updated since you last played.** Mods Steam updated after your last launch from DrukhalManager. See [[Launching and Saves]].
* **Achievements.** Whether your mods turn achievements off, and which ones do.

## Searching

The search box (Ctrl+F) takes filters as well as names:

* `enabled:yes conflicts:yes` lists enabled mods with conflicts.
* `source:local || giga` finds local mods, or anything called giga.
* `--` leaves things out.

The **Filters** button builds a filter for you.

## Notes

Right-click a mod and choose **Add Note…**, for things like "keep this below UIOD". The note shows on the row and in the inspector, where you can edit it. Search finds notes.
