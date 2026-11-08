# DrukhalManager

A native Linux mod manager for Stellaris, with AI-assisted mod merging.

DrukhalManager is a fork of [Irony Mod Manager](https://github.com/bcssov/IronyModManager)
by Mario (bcssov), rewritten in Rust with GTK 4 and libadwaita.

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
(bcssov), and wouldn't exist without it. Irony's design and behaviour are the
reference for this rewrite, its conflict solver, collections, load order
checks and descriptor handling are the models for DrukhalManager's, and code
ported from it is marked at the top of the relevant source files. Thank you,
Mario.

## License

MIT, like Irony Mod Manager. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Installing

On Arch or CachyOS, build and install the package from the repository:

```sh
cd packaging/arch
makepkg -si
```

This installs `drukhal-manager` with a desktop entry and icon, so it shows up
in your app launcher.

## Building

Requires Rust, GTK 4.14+ and libadwaita 1.6+ development files.

```sh
cargo run --release
```

To install by hand elsewhere, copy `target/release/drukhal-manager` to your
`PATH`, `data/io.github.dukko.DrukhalManager.desktop` to
`share/applications`, and the icon from `data/icons` to
`share/icons/hicolor/scalable/apps`.
