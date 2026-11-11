<div align="center">

<img src="data/icons/io.github.dukko.DrukhalManager.svg" width="112" alt="">

# DrukhalManager

**A native Linux mod manager for Stellaris, with AI-assisted conflict merging.**

[![Latest release](https://img.shields.io/github/v/release/Dukko/DrukhalManager?label=release&color=4a86cf)](https://github.com/Dukko/DrukhalManager/releases/latest)
[![CI](https://github.com/Dukko/DrukhalManager/actions/workflows/ci.yml/badge.svg)](https://github.com/Dukko/DrukhalManager/actions/workflows/ci.yml)
[![Flatpak](https://img.shields.io/badge/install-Flatpak-4a90d9?logo=flatpak&logoColor=white)](#installing)
[![License: MIT](https://img.shields.io/badge/license-MIT-green)](LICENSE)

Built with Rust, GTK 4 and libadwaita · A fork of [Irony Mod Manager](https://github.com/bcssov/IronyModManager)

<img src="docs/screenshots/hero.png" alt="The load order after Smart Sort: graphics mods first, each with its thumbnail, game version and tags, and the Smart Sort button in the header" width="860">

</div>

## Why DrukhalManager?

- 🐧 **Feels at home on Linux.** A native GTK 4 app that follows your desktop's theme, installed as a Flatpak that keeps itself up to date.
- ⚔️ **Sees every conflict.** Files and objects that more than one mod changes, side by side, with the version the game actually uses marked and the reason it wins.
- 🤖 **Merges them for you.** Send two conflicting versions, plus the base game's, to Claude or any OpenAI-compatible model, and save the result to a patch mod that loads last.
- ⚡ **Skips the AI when it can.** Conflicts where only one mod changed anything are merged directly, at no cost.
- 🧹 **Hides the noise.** Ignore one conflict, a whole folder, or anything matching a rule like `gfx/**`.
- 🗂️ **Keeps your load orders.** Named collections that sync to Paradox Launcher playsets, with Workshop collection import, dependency checks and descriptor repair.
- 🧭 **Sorts them for you.** Smart Sort orders your mods the way the Stellaris community's load order guides suggest, keeping sub-mods below their mods and every mod after its dependencies.

## Installing

### Flatpak (any distro)

```sh
flatpak install --user https://dukko.github.io/DrukhalManager/io.github.dukko.DrukhalManager.flatpakref
```

Updates arrive with `flatpak update` or your software centre. DrukhalManager also tells you when a new version is out and can install it for you (**Preferences → Updates**). The GNOME runtime comes from Flathub, and the repository is signed with [`flatpak/drukhal-manager.gpg`](flatpak/drukhal-manager.gpg).

<details>
<summary>Offline installs, self-updates, and Steam on another drive</summary>

<br>

**Offline:** download `DrukhalManager.flatpak` from the [latest release](https://github.com/Dukko/DrukhalManager/releases/latest) and run `flatpak install --user DrukhalManager.flatpak`. It updates from the same repository. If you installed the 0.1.0 bundle, install once more by either method to get updates.

**Updating from inside the app** works from 0.2.2 on; on 0.2.0 or 0.2.1, run `flatpak update` once. The first time, Flatpak asks whether DrukhalManager may update itself. Desktops without a portal for that question, such as niri or sway, can't show it, so allow it yourself:

```sh
flatpak permission-set flatpak updates io.github.dukko.DrukhalManager yes
```

**Steam elsewhere:** the sandbox can see your home folder. If Steam or a Steam library lives somewhere else, such as another drive, DrukhalManager shows the command that grants access:

```sh
flatpak override --user --filesystem=/path/to/Steam io.github.dukko.DrukhalManager
```

In the Flatpak, **Launch** starts the game through Steam.

</details>

### Arch or CachyOS

```sh
cd packaging/arch
makepkg -si
```

This installs `drukhal-manager` with a desktop entry and icon.

## A tour

### 🗂️ Your load order

Drag mods into order, type a position, or let DrukhalManager sort them. Thumbnails come from each mod's folder, or from its Workshop preview. Each mod is marked when it turns off achievements, is waiting on a Steam update, or was unsubscribed in Steam.

- **Smart Sort** groups your mods into sections, following the community's load order guides: unofficial patches (`~`) first, then graphics, flavor, shipsets, galaxy generation, gameplay and large overhauls, then compatibility patches, UI mods such as UI Overhaul Dynamic and their patches, and universal patches (`!`) last. Sub-mods go directly below the mod they need, and every mod still loads after its dependencies. A mod's section comes from its name, its dependencies and the folders it ships, not its often-wrong tags. A preview shows where each mod goes and why, and you can move any mod to another section; your choice is remembered. Applying it can be undone.
- **Other sorts:** **Sort A–Z, Dependencies First** (in Smart Sort's drop-down) sorts by name, and **Sort by Dependencies** appears when a mod loads before one it needs.
- **Collections:** several named load orders, switched from the header, each synced to its own Paradox Launcher playset. Import a launcher playset (Irony's, for example), a shared file, or a **Workshop collection** from its link. Mods you don't have yet are listed with links to subscribe.
- **Checks:** warns when a mod loads before one it depends on, and notes missing dependencies and mods made for an older game version.
- **Descriptor repair:** fixes `.mod` files that point at folders that no longer exist, and removes those whose mod is gone. Everything is backed up first.
- **Reload** (F5) picks up mods subscribed or removed while the app is open, and **Merge into One Mod** copies a collection into a single local mod, the way the game would load it.

### ⚔️ Conflicts, side by side

<img src="docs/screenshots/conflicts-framed.png" alt="The Conflicts page: a sidebar of conflicts by folder, and two versions of one object side by side, the in-game one marked">

Every file and object that more than one enabled mod changes is listed by folder, with a search and a filter by mod. Pick one to see two versions side by side, with the changed lines and words highlighted.

- The version the game uses today is marked **In game**, with the reason: its file name sorts last, the folder keeps the first definition it reads, or the load order decides.
- ⚡ **Simple** conflicts need no AI, because only one mod changed the object from the base game, or every mod ships the same version.
- The left card is view only and the right one is editable, so you can adjust a version by hand. **Take Change** copies single differences across, and **Base Game** compares each version with the game's own instead.

### 🤖 Merging with AI

**Merge with AI** sends both versions, plus the base game's when it can be found, to the model you choose. The result stays on the left while the next mod's version slides in on the right, until one result is left to review and **Save to Patch**.

- **Merge All** merges a whole folder, several objects at once, with an estimate of the calls and cost first. It works on folders of overridden files too, with one AI call per object the copies disagree on.
- A result that comes back broken is sent back to the model once with the error, before it counts as failed.
- **Overridden files** are split into their objects. Objects the copies agree on, or that only one mod changed, are combined directly, and the AI is only asked about real disagreements.
- **Broken braces** are listed under **Problems**. **Fix…** repairs them automatically, and **Fix with AI** restores the intended nesting.

Everything is saved to **DrukhalManager Merged Patch**, a local mod that loads last, with files named so its definitions win. Merges whose mods or the base game changed since are flagged as outdated, and your mods are never modified. The patch is backed up before each Merge All and before merges are removed, and **Merged Patch Backups…** in the main menu restores any of the last ten.

<table>
<tr>
<td width="50%"><img src="docs/screenshots/preferences-ai.png" alt="AI Merging preferences: choose Claude or an OpenAI-compatible endpoint such as OpenRouter"></td>
<td width="50%"><img src="docs/screenshots/ignore-rules.png" alt="The Ignore Rules editor"></td>
</tr>
<tr>
<td><b>Choose a provider</b>: Claude (<code>claude-opus-5-5</code>), OpenRouter, or a local server such as Ollama or LM Studio.</td>
<td><b>Ignore rules</b> hide conflicts by path, like <code>gfx/**</code>, optionally only for an exact set of mods, with a live count of what each rule hides.</td>
</tr>
</table>

Text boxes, such as the model's reasoning, merged results and the file editors, can be made taller or shorter by dragging the grip below them.

API keys are kept in your system keyring, or in a settings file only you can read when there's no keyring. Merging sends only the conflicting definitions to the provider you choose.

## Building

Requires Rust, GTK 4.14+ and libadwaita 1.8+ development files.

```sh
cargo run --release
```

To install by hand elsewhere, copy `target/release/drukhal-manager` to your `PATH`, `data/io.github.dukko.DrukhalManager.desktop` to `share/applications`, and the icon from `data/icons` to `share/icons/hicolor/scalable/apps`.

<details>
<summary>Releasing</summary>

<br>

1. Bump the version in `Cargo.toml` and add a `<release>` to `data/io.github.dukko.DrukhalManager.metainfo.xml`.
2. If the release adds a permission to `flatpak/io.github.dukko.DrukhalManager.yml`, say so in the release notes: Flatpak won't let the app update itself into it, so users update with `flatpak update` or their software centre instead.
3. If dependencies changed, regenerate `flatpak/cargo-sources.json` (see `flatpak/generate-cargo-sources.py`); the build fails if it is stale.
4. Merge it into `master` through a pull request. The *Tag release* workflow tags `vX.Y.Z`, builds the Flatpak, publishes the signed repository to GitHub Pages and attaches `DrukhalManager.flatpak` to the release.

</details>

## Credits

DrukhalManager is a fork of [Irony Mod Manager](https://github.com/bcssov/IronyModManager) by Mario (bcssov), and wouldn't exist without it. Irony already ships a native Linux build with Proton support; DrukhalManager is a Rust and GTK alternative focused on AI-assisted merging. Irony's design and behaviour are the reference for this rewrite: its conflict solver, override rules, collections, load order checks and descriptor handling are the models for DrukhalManager's, and code ported from it is marked at the top of the relevant source files. Thank you, Mario.

### Built with AI assistance

Most of DrukhalManager's code was written by Claude (Anthropic's AI model) working as a coding assistant: I chose what to build, reviewed it, and tested it with my own mods. Commits written with it are marked `Co-Authored-By: Claude`. AI merging is a separate feature that uses whichever provider you pick in Preferences.

## License

MIT, like Irony Mod Manager. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
