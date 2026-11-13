# Load Order and Smart Sort

## Moving mods

* Drag a row by its handle, or type a new position into its number.
* Right-click a mod for **Move to Top**, **Move to Bottom**, its Workshop page, a note and more.
* **Undo** and **Redo** are the arrow buttons at the top left (Ctrl+Z and Ctrl+Shift+Z).

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
* **Achievements.** Whether your mods turn achievements off, and which ones do.

## Searching

The search box (Ctrl+F) takes filters as well as names:

* `enabled:yes conflicts:yes` lists enabled mods with conflicts.
* `source:local || giga` finds local mods, or anything called giga.
* `--` leaves things out.

The **Filters** button builds a filter for you.

## Notes

Right-click a mod and choose **Add Note…**, for things like "keep this below UIOD". The note shows on the row and in the inspector, where you can edit it. Search finds notes.
