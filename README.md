# DrukhalManager

A native Linux mod manager for Stellaris, with AI-assisted mod merging.

DrukhalManager is a fork of [Irony Mod Manager](https://github.com/bcssov/IronyModManager)
by Mario (bcssov), rewritten in Rust with GTK 4 and libadwaita. Irony already
ships a native Linux build with Linux/Proton support; DrukhalManager is a
Rust/GTK alternative focused on AI-assisted merging.

Built with Rust, GTK4 and libadwaita.

## Managing mods

- **Collections**: keep several named load orders and switch between them
  from the header. Each one syncs to its own Paradox Launcher playset.
  Import a launcher playset (Irony's, for example) or a shared file, and
  export a collection to share. Shared files match mods by Steam ID.
- **Load order checks**: warns when a mod loads before a mod it depends on
  (**Sort by Dependencies** fixes it), and notes missing dependencies and
  mods made for an older game version.
- **Descriptor repair**: fixes `.mod` files that point at folders that no
  longer exist, and removes descriptors whose mod is gone. Everything is
  backed up first.
- **Search** (Ctrl+F), and **Open Folder** or **Open Workshop Page** from
  each mod's menu.
- If something else changes the load order (the Paradox Launcher resets it
  when opened), a banner offers to restore your collection.

## Merging conflicts

The Conflicts page lists every file and object that more than one enabled mod
changes. For an object conflict, **Merge…** sends each mod's version, plus the
base game's version when it can be found, to an AI model. You get back a
combined definition that you can review and edit before saving it.

When several mods ship the same file, the game only uses the last copy.
**Merge…** on an overridden script or localisation file splits every copy
into its objects. Objects the copies agree on, or that only one copy has, are
combined automatically, and so are objects that only one mod changed from
the base game. AI is only asked about objects the mods really disagree on.

Saved merges go into a local mod, **DrukhalManager Merged Patch**, which is
added to your load order. Merged objects go in files named to sort so they
win in-game. Merged files keep their original path, so the patch is kept at
the end of the load order, and a warning shows if it's moved.

Files with brace mistakes are listed under **Problems**. The game keeps
reading after a missing or extra `}`, but everything after it gets nested
inside the broken object and stops working. **Fix…** repairs the braces
automatically, using the convention that top-level objects start at column 0,
and flags the objects it touched. **Fix with AI** then restores the intended
nesting of just those objects. The fixed copy is saved through the merged
patch, so the mod itself is never modified.

Without AI, **Use This Version** keeps one mod's version as it is, and the
eye button ignores a conflict, or a whole folder, so it no longer shows or
counts.

Choose the AI provider in Preferences (Ctrl+,):

- **Claude (Anthropic API)**: uses `claude-opus-5-5`. Enter an API key, or
  leave it empty to use `ANTHROPIC_API_KEY`.
- **OpenAI-compatible**: OpenRouter by default, or any server with a
  `/chat/completions` endpoint, such as Ollama or LM Studio.

Keys are stored in `~/.config/DrukhalManager/settings.json`, readable only by
you. Merging sends the conflicting definitions to the provider you choose.

## Credits

DrukhalManager is a fork of
[Irony Mod Manager](https://github.com/bcssov/IronyModManager) by Mario
(bcssov), and wouldn't exist without it. Irony already provides a native Linux
build with Linux/Proton support. Irony's design and behaviour are the
reference for this rewrite, its conflict solver, collections, load order
checks and descriptor handling are the models for DrukhalManager's, and code
ported from it is marked at the top of the relevant source files. Thank you,
Mario.

## Built with AI assistance

Most of DrukhalManager's code was written by Claude (Anthropic's AI model),
working as a coding assistant: I chose what to build, reviewed it and tested
it with my own mods. Commits written with it are marked
`Co-Authored-By: Claude`. AI merging is a separate feature that uses whichever
AI provider you pick in Preferences.

## License

MIT, like Irony Mod Manager. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Installing

### Flatpak (any distro)

Install it from DrukhalManager's own Flatpak repository:

```sh
flatpak install --user https://dukko.github.io/DrukhalManager/io.github.dukko.DrukhalManager.flatpakref
```

Updates then arrive with `flatpak update`, or through your software centre,
like any other Flatpak. DrukhalManager also tells you when a new version is
out and installs it for you (Preferences → Updates). It uses the GNOME runtime from Flathub, which Flatpak
installs for you if needed. The repository is signed with the key in
[`flatpak/drukhal-manager.gpg`](flatpak/drukhal-manager.gpg).

To install offline, download `DrukhalManager.flatpak` from the
[latest release](https://github.com/Dukko/DrukhalManager/releases/latest) and
run `flatpak install --user DrukhalManager.flatpak`. It updates from the same
repository. If you installed the 0.1.0 bundle, install once again by either
method to get updates.

Installing updates from inside the app works from 0.2.2 on; on 0.2.0 or 0.2.1,
run `flatpak update` once. The first time, Flatpak asks whether
DrukhalManager may update itself. Desktops without a portal for that question,
such as niri or sway, can't show it, so allow it once yourself:

```sh
flatpak permission-set flatpak updates io.github.dukko.DrukhalManager yes
```

The sandbox can see your home folder. If Steam or a Steam library lives
elsewhere, such as another drive, DrukhalManager shows the command that
grants access. It looks like this:

```sh
flatpak override --user --filesystem=/path/to/Steam io.github.dukko.DrukhalManager
```

In the Flatpak, **Launch** starts the game through Steam.

### Arch or CachyOS

Build and install the package from the repository:

```sh
cd packaging/arch
makepkg -si
```

This installs `drukhal-manager` with a desktop entry and icon, so it shows up
in your app launcher.

## Building

Requires Rust, GTK 4.14+ and libadwaita 1.8+ development files.

```sh
cargo run --release
```

To install by hand elsewhere, copy `target/release/drukhal-manager` to your
`PATH`, `data/io.github.dukko.DrukhalManager.desktop` to
`share/applications`, and the icon from `data/icons` to
`share/icons/hicolor/scalable/apps`.

## Releasing

1. Bump the version in `Cargo.toml` and add a `<release>` to
   `data/io.github.dukko.DrukhalManager.metainfo.xml`.
2. If dependencies changed, regenerate `flatpak/cargo-sources.json` (see
   `flatpak/generate-cargo-sources.py`); the release fails if it is stale.
3. Push a `vX.Y.Z` tag. The *Flatpak release* workflow builds the Flatpak,
   publishes the signed repository to GitHub Pages, and attaches
   `DrukhalManager.flatpak` to the release (creating it if needed).
