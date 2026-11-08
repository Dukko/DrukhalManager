# DrukhalManager

A native Linux mod manager for Stellaris, with AI-assisted mod merging.

Built with Rust, GTK4 and libadwaita.

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

Choose the AI provider in Preferences (Ctrl+,):

- **Claude (Anthropic API)**: uses `claude-opus-5-5`. Enter an API key, or
  leave it empty to use `ANTHROPIC_API_KEY`.
- **OpenAI-compatible**: OpenRouter by default, or any server with a
  `/chat/completions` endpoint, such as Ollama or LM Studio.

Keys are stored in `~/.config/DrukhalManager/settings.json`, readable only by
you. Merging sends the conflicting definitions to the provider you choose.

## Credits

DrukhalManager stands on the shoulders of
[Irony Mod Manager](https://github.com/bcssov/IronyModManager) by Mario
(bcssov). Irony's design and behaviour are the reference for this rewrite,
and code ported from it is marked at the top of the relevant source files.
See [NOTICE](NOTICE) for its license.

## Building

Requires Rust, GTK 4.14+ and libadwaita 1.6+ development files.

```sh
cargo run
```
