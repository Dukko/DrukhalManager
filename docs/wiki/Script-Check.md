# Script Check

The game quietly skips script it doesn't understand. A mod that calls an effect or trigger removed in a Stellaris update, or fires an event nothing defines, keeps loading, but the feature it belongs to stops working without an error.

Script Check runs as part of the conflicts scan, once the base game's files have been read. When it finds something, the top of the **Conflicts** page (Alt+2) says how many problems there are and in how many mods, for example **12 script problems in 3 mods…**. Click it to open the list. When everything is fine, the button doesn't appear.

## Before the first check

Start Stellaris once with DrukhalManager's launch button (or from Steam). The game writes its own scripting reference, `effects.log` and `triggers.log`, to `logs/script_documentation` in your Stellaris user folder, and Script Check reads them. Without them the check is skipped.

If the reference is older than the game's executable, the dialog says so: start the game once, then **Rescan**.

## What it looks at

For each file of each enabled mod, Script Check reads the blocks that hold effects and triggers, the way the base game itself uses them. It reports:

* **Unknown** effects or triggers, which this version of the game doesn't have.
* **Wrong kind**: a trigger used where an effect belongs, or the reverse.
* **Capitalisation**: a name that would be known in lowercase.
* **Missing events**: an event fired from a namespace that exists, but no enabled mod or the game defines that event, so nothing happens.
* **Unused objects** under **Not used**: nothing in the game or your mods calls them, so fixing them is optional.

Findings it isn't sure about are kept apart, so they don't hide the real problems:

* **Probably from another mod**: names that look like another mod's scripted effects or triggers, and no installed mod defines them. A companion mod may be missing.
* **Less certain**: settings this kind of block doesn't take in the base game, but which may still be fine.

## Fixing an object

Pick an object to see each mod's version beside the base game's, with the problems listed. **Fix with AI** asks your model to rewrite the object using only effects, triggers and scopes the game has. **Edit by Hand** opens the same left/right editor starting from the mod's copy, so you can correct it yourself without a provider. Either way, review the result and **Save to Merged Patch**.

A saved fix goes into the merged patch, like any other merge, and the row then says **Fixed in the merged patch**. Nothing in your mods is changed; rescan to see anything still left.

## Ignore rules

Rules hide conflicts and problems you don't want to see. The rule editor is **Rules…** at the bottom of the Conflicts list.

One rule per line:

```text
# Comments start with #.
gfx/**                              every file override under gfx
localisation/*                      every localisation key
common/defines                      a folder, and everything below it
common/buildings/building_lab       one object conflict
interface/*.gui | Mod A | Mod B     only when exactly these mods conflict
common/traditions | won by Mod A    only while Mod A's version is used
!gfx/portraits/**                   shows these again
```

* A **file override** matches by its path. An **object conflict** matches by `<folder>/<key>`, or by its files when every one of them matches.
* `*` matches within a folder and `**` across folders. A folder on its own matches everything below it.
* `| Mod A | Mod B` narrows a rule to conflicts between exactly those mods.
* `| won by Mod A` applies it only while Mod A's version is the one the game loads. When another mod wins, the conflict is shown again.
* A line starting with `!` shows matching conflicts again, and `#` starts a comment.
* **Later lines win**, so a narrow `!` rule after a broad rule brings some conflicts back.

Names and paths are compared ignoring case. The editor shows how many conflicts each rule hides.

### The rule builder

**Add Rule…** in the rule editor, and **Ignore Where … Wins…** in a conflict's or folder's menu, open the rule builder. Pick a folder and the mod whose version wins; it shows which conflicts the rule would hide and which stay shown because another mod wins, then adds a `| won by Mod` rule for you.

**Show Ignored** at the bottom of the Conflicts list brings hidden conflicts back without changing the rules.
