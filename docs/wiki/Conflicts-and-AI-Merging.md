# Conflicts and AI Merging

## Reading conflicts

The **Conflicts** page (Alt+2) lists every file and object more than one mod changes. Pick one to see two versions side by side, with the differences highlighted. The version the game loads is marked **In game**, with why: its file name sorts last, the folder keeps the first definition it reads, or it's later in the load order.

Conflicts marked ⚡ are the simple kind, where only one mod changed anything, or every mod ships the same thing. Those merge without AI.

## Fixing one by hand

The right-hand version is editable.

* **Take Change** copies one difference across.
* **Changed Lines Only** hides the lines that match.
* **Base Game** compares against the vanilla file.
* **Merge Tool…** opens both versions in Meld, KDiff3, VS Code or another tool, chosen in **Preferences → Merging**.

| Shortcut | Does |
| --- | --- |
| Alt+Page Down / Alt+Page Up | Next / previous conflict |
| Alt+Left / Alt+Right | Use the left / right version |
| Alt+M | Merge with AI |
| Alt+I | Ignore the conflict |

## Ignoring noise

Ignore a single conflict, a folder, or a pattern like `gfx/**`. **Rules…**, at the bottom of the conflicts list, edits the patterns and says how many conflicts each one hides. **Show Ignored** brings them back into view.

## Merging with AI

Pick a model in **Main Menu → Preferences → AI Merging**: Claude, OpenRouter, or a local server such as Ollama or LM Studio.

* **Merge with AI** sends both versions, and the base game's when there is one, and shows you the result to review before saving. With three or more mods it merges them one at a time.
* **Merge All** does a whole folder, and tells you roughly how many calls and how much money that will be first.

Each merge is a paid API call (or a local model you run), so read what comes back before saving it.

## The merged patch

Merges are saved into a merged patch mod that loads last, one per collection. Your mods are never changed. When a mod or the game updates, the merges that depended on it are flagged as outdated.

**Use This Version** choices are easier to bring up to date. When the mod you picked updates and nothing else in that conflict changed, the Outdated Merges page says "you picked *mod*'s version" and offers **Use New Version** on that row, or **Use N New Versions** at the top for all of them at once. The patch is backed up first. If the other mods or the base game changed too, re-merge it as usual.

The patch is backed up before every **Merge All**. The backups button at the bottom of the conflicts list, next to **Rules…** and **Rescan**, shows the last ten, each with **Restore…**.
