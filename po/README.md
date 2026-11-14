# Translations

The app's own strings are in English in the source and looked up in gettext
catalogs at run time. English is the message id, so `t!("Save")` translates
"Save" and shows it unchanged when no catalog has an entry for it.

| Call | Use |
| --- | --- |
| `t!("Save")` | A string without values. |
| `tf!("Saved {name}", name = collection)` | A string with `{name}` placeholders. Translators can reorder them. |
| `tn!("{count} mod", "{count} mods", n)` | A singular/plural choice. |
| `tnf!("{count} mod", "{count} mods", count = n)` | Plural, with placeholders filled in. |
| `tc!("Open", "verb")`, `tnc!(...)` | Words whose translation depends on the context. |

Use `tf!`/`tnf!` instead of `format!` for anything a translator sees, and
build the sentence in one string rather than gluing translated fragments
together. `msgid`s must be string literals: `xtr` finds them by reading the
source, so it cannot see a string built at run time.

## For translators

The catalogs are `.po` files in this folder. To add a language, copy the
template:

```sh
msginit --input po/drukhal-manager.pot --locale=de_DE.UTF-8 --output-file=po/de.po
```

and edit `po/de.po` with any PO editor (Poedit, Lokalize, or Weblate). Then
try it without installing:

```sh
msgfmt --check -o target/i18n/locale/de/LC_MESSAGES/drukhal-manager.mo po/de.po
LANG=de_DE.UTF-8 cargo run
```

`build.rs` compiles every `po/*.po` into `target/i18n/locale`, which the app
uses when it runs from the source tree. The packaged builds install the same
`.mo` files under `share/locale`.

Named placeholders (`{count}`, `{name}`) must match the English text exactly.
`po/check-placeholders.sh` checks that, and that plural entries have as many
`msgstr[n]` lines as the catalog's `Plural-Forms` header says:

```sh
po/check-placeholders.sh po/de.po
```

A translation with a missing or mistyped placeholder falls back to the English
sentence at run time, so the app stays usable, but the checker catches it
before release.

Some button labels start with `_`, for example `_Cancel` or `_Save`: the letter
after the underscore is the keyboard shortcut. Keep exactly one underscore,
before a letter that suits the translation (and that no other button on the
same dialog uses).

Translations are collected on
[Weblate](https://hosted.weblate.org/projects/drukhalmanager/drukhalmanager/); its pull
requests land `.po` files here and usually need no code review.

### Weblate component (project owner)

The component is set up at
[hosted.weblate.org/projects/drukhalmanager/drukhalmanager](https://hosted.weblate.org/projects/drukhalmanager/drukhalmanager/).
These are its settings, also useful if it ever has to be recreated:

| Setting | Value |
| --- | --- |
| File format | Gettext PO file (bilingual) |
| Source language | English |
| File mask | `po/*.po` |
| Monolingual base language file | *(leave empty)* |
| Template for new translations | `po/drukhal-manager.pot` |
| Repository | `https://github.com/Dukko/DrukhalManager` |
| Branch | `translations` until PR #160 merges, then `master` |
| Push | not configured yet (see the follow-ups below) |

A `.pot` cannot go in **Monolingual base language file**: that field belongs to
Weblate's monolingual workflow, which expects a real `.po` for the source
language. English is the msgid here, so the template belongs in **Template for
new translations** and the base file stays empty.

The "Update PO files to match POT (msgmerge)" add-on is enabled, so the
catalogs follow `po/drukhal-manager.pot` automatically. Once git push is
connected, new `.po` files arrive as pull requests: CI runs
`po/check-pot.sh` and `po/check-placeholders.sh` on them, and `build.rs`
compiles every `.po` it finds, so no other change is needed to ship a
language.

Two follow-ups for the owner:

- The component currently tracks the `translations` branch, because `master`
  gains `po/` only when PR #160 merges. Switch it to `master` after that.
- Pushing is not configured yet, so translations stay in Weblate. Connect the
  Weblate GitHub app (or a deploy key) to have them arrive as pull requests;
  until then, download them from the component's *Files* page.

A repository browser of
`https://github.com/Dukko/DrukhalManager/blob/master/{{filename}}#L{{line}}`
links each string to its source.

For the component's **Translation instructions**, this is enough:

> English is the source and the message id. Keep every `{name}` placeholder
> exactly as it is; you can move it within the sentence. Keep exactly one `_`
> before a letter in button labels such as `_Cancel`: that letter is the
> keyboard shortcut. Keep button labels short, and ask in a comment when a
> string needs more context.

## For developers

`po/drukhal-manager.pot` is generated, never edited by hand:

```sh
cargo install xtr --version 0.1.11 --locked   # once
po/update-pot.sh                              # rewrites the template
po/check-pot.sh                               # verifies it is current
po/check-placeholders.sh                      # checks every po/*.po
```

Commit the template with the code that changed the strings. CI runs
`po/check-pot.sh` and `po/check-placeholders.sh` and fails when they disagree,
so translations are never offered for strings that no longer exist, and a
broken placeholder is caught before release.

When a release changes strings, run `msgmerge --update po/<lang>.po
po/drukhal-manager.pot` for the languages already there (Weblate does this
itself); `build.rs` then compiles the catalogs and package builds install
them next to the binary.
