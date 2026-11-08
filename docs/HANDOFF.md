# Handoff: continuing DrukhalManager

For a coding agent (or person) picking up the work. It covers where the project stands as of v0.1.0 (2026-11-08), how the code is laid out, what's been verified on the owner's machine, the rules to follow, and what to do next.

## Where it stands

- **Released:** v0.1.0 on GitHub (https://github.com/Dukko/DrukhalManager/releases/tag/v0.1.0), with a Flatpak bundle attached. `master` is the default branch and is current. `load-order-editing` is an old branch at an earlier commit; leave it unless the owner says to delete it.
- **Build:** `cargo build` gives no warnings. `cargo test` has 52 passing unit tests.
- **Stack:** native Linux, Rust (edition 2024), GTK 4.14+ and libadwaita 1.6+. Keep it native: don't propose Electron or web stacks.
- **Licence:** MIT. `LICENSE` keeps Mario's Irony Mod Manager copyright, and `NOTICE` explains the fork. Code ported from Irony carries an attribution comment at the top of its file; keep doing that.

## Code map

| Area | Files |
|---|---|
| Entry, sandbox paths | `main.rs`, `sandbox.rs` (host directories when running in a Flatpak) |
| Paradox and Steam | `paradox/script.rs` (Clausewitz parser with byte spans), `steam.rs` (libraries, unreachable Steam roots), `game.rs` (find and launch the game), `launcher_db.rs` (Paradox Launcher playsets in SQLite), `error_log.rs` |
| Mods | `mods/mod.rs` (discovery), `descriptor.rs`, `load_order.rs` (`dlc_load.json`), `checks.rs` (dependency and version checks), `collections.rs`, `repair.rs` (stale or orphaned descriptors) |
| Merging | `merge/conflicts.rs` (scan), `source.rs` (cut out one object's text), `ai.rs` (Claude and OpenAI-compatible providers, streaming), `file_merge.rs` (per-object merge of a whole file), `lint.rs` (brace repair), `patch.rs` (merged patch mod, fingerprints, staleness) |
| UI | `window.rs` (main window and `State`), `window/collections_ui.rs`, `conflicts_view.rs`, `merge_dialog.rs`, `file_merge_dialog.rs`, `fix_dialog.rs`, `error_report_dialog.rs`, `thinking_view.rs`, `preferences.rs`, `settings.rs` |
| Packaging | `data/` (desktop entry, icon, metainfo), `packaging/arch/PKGBUILD` (+ `.SRCINFO`), `flatpak/` (manifest, `generate-cargo-sources.py`, `cargo-sources.json`) |

Key invariants:
- Every load order change goes through `State` in `window.rs`, and each mutation calls `pin_patch()`. The merged patch must always load last.
- Saving writes `dlc_load.json`, then the active collection, then the launcher playset.
- Each collection syncs to a playset named by `collections::playset_name()`.
- Conflict analysis excludes the patch mod. Ignored conflicts are keys stored in the settings, built by the key helpers in `conflicts_view.rs`.

## Facts verified on the owner's machine

- The Paradox Launcher reads `launcher-v2.sqlite` playsets, not `dlc_load.json`, and opening it empties `dlc_load.json`. Syncing playsets fixed both. The restore banner covers the reset.
- The patch's file naming (prefix `伞伞伞_` for last-wins folders, `!!!!!!_` for first-wins) works in-game: the game used the patch's definitions. The first-wins folders (`events`, `section_templates`, `traits`, `strategic_resources`) come from Irony's own output.
- `~/.local/share/Steam` links to `/mnt/lean/home-share/Steam`, outside home. The Flatpak needs a `flatpak override --filesystem=…`, and the app shows the command for it.
- The desktop theme asks for "Google Sans Flex", which isn't installed. The native app falls back to Noto Sans and the Flatpak to DejaVu Sans. That's a host setting, not an app bug.
- No `ANTHROPIC_API_KEY` is set in the agent's environment. The owner ran real AI merges through the UI. A real streaming request hasn't been confirmed by the agent.

## Rules to follow

- **Don't drive the owner's desktop.** No `wtype` or synthetic input, and no `niri msg action screenshot-window`, which also overwrites the clipboard. The owner is usually using the machine. Verify with unit tests and backend end-to-end runs (scratch copies of the data under the job's temp dir, and a mock AI server), then ask the owner to check the UI.
- **Never write to the owner's real Stellaris data while testing.** Use copies of `launcher-v2.sqlite` and the user dir, or set `XDG_DATA_HOME`/`XDG_CONFIG_HOME` to a sandbox.
- **Don't run scripts downloaded from the internet.** The permission system blocked this once. That's why `flatpak/generate-cargo-sources.py` exists.
- **Flathub's generative AI policy rules out Flathub.** Manifests there can't be AI-generated, AI tools can't open or write the submission PR, and heavy AI-generated code is likely to be rejected. Distribution is self-hosted: GitHub releases, and possibly a Flatpak repo on GitHub Pages.
- **Ask the owner before outward actions:** posting anywhere, new repos, deleting branches. Commits end with `Co-Authored-By: Claude …`. Pushing to `master` after a passing build and tests has been the norm.
- **Real AI calls cost the owner money.** Ask before making one.

## Next steps, in priority order

1. **Confirm the Flatpak fixes.** Ask the owner to install the v0.1.0 bundle, run the override command the app shows, and confirm that Launch works through Steam and the Steam libraries are found.
2. **Confirm a real AI stream.** With the owner's go-ahead and key, run one merge with each provider. Check that the reasoning box fills in, and that Claude's adaptive thinking with `display: "summarized"`, structured output and `fallbacks: "default"` are accepted (`merge/ai.rs`). Fix whatever the API rejects.
3. **Automatic Flatpak updates.** Publish a signed Flatpak repo on GitHub Pages (`flatpak build-update-repo`, a GPG key from the owner, a `.flatpakrepo` file), plus a GitHub Actions workflow that builds the bundle on each tag. Document `flatpak remote-add`.
4. **CI.** A GitHub Actions workflow running `cargo build`, `cargo test` and `cargo clippy` on push. It needs the GTK 4 and libadwaita dev packages; consider the GNOME SDK container.
5. **Remaining features the owner was offered:**
   - undo/redo for the load order (Ctrl+Z), beyond the one-shot Undo toasts;
   - an AI merge for a whole folder, with a call and cost estimate first, and token usage shown;
   - mod thumbnails from the descriptor `picture`;
   - API keys in the system keyring instead of `settings.json`;
   - Irony's remaining features: merge the collection into one mod, and a multiplayer hash report.
6. **Known gaps:**
   - Zipped (`archive=`) mods aren't scanned.
   - `.gui`/`.gfx` files and files that repeat one type keyword can't be merged object by object.
   - Localisation files with several languages aren't supported.
   - Merges saved before v0.1.0 have no fingerprints, so they aren't checked for staleness.
   - The after-game error toast doesn't work in the Flatpak, because the game is launched through Steam.
7. **Split `window.rs`.** At about 1,600 lines, it would be easier to work in if the load order list, launching and the patch handling were split into `window/` submodules, as `collections_ui.rs` already is.

## Useful commands

```sh
cargo test                                   # unit tests
cargo run --release                          # native build
python3 flatpak/generate-cargo-sources.py Cargo.lock flatpak/cargo-sources.json   # after dependency changes
flatpak run org.flatpak.Builder --user --force-clean --repo=flatpak/repo flatpak/build flatpak/io.github.dukko.DrukhalManager.yml
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo flatpak/repo flatpak/DrukhalManager.flatpak io.github.dukko.DrukhalManager
```

The GNOME 51 SDK, the rust-stable 26.08 extension and `org.flatpak.Builder` are installed for the owner's user, and Flathub is configured as a user remote.
