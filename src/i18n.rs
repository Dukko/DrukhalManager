//! The app's own UI strings, in the language the system is set to.
//!
//! English is both the source language and the message id, so the code reads
//! the same as the strings translators see. A `t!("...")` call asks gettext
//! for the translation of that exact text and returns it, or the text itself
//! when the catalog has no entry (or no catalog was installed).
//!
//! Strings with values use `{name}` placeholders instead of `format!`'s
//! positional `{}`, so translators can move the values around: `tf!("{count}
//! mods", count = n)`. Plurals go through `tn!`/`tnf!`, and `tc!` adds a
//! context for words that translate differently depending on where they
//! appear. The translation template is extracted from these calls by
//! `xtr` (see `po/README.md`).

use std::path::{Path, PathBuf};

/// The gettext domain, which is also the name of the `.mo` files.
pub const DOMAIN: &str = "drukhal-manager";

/// Sets up gettext and points it at the installed catalogs.
///
/// Call once, before building any UI. The locale comes from the environment
/// (`LC_ALL`, `LC_MESSAGES`, `LANG`, `LANGUAGE`), as the system set it.
pub fn init() {
    // SAFETY: runs before any other thread or library reads the locale.
    unsafe {
        let _ = gettextrs::setlocale(gettextrs::LocaleCategory::LcMessages, "");
    }
    if let Some(dir) = locale_dir() {
        let _ = gettextrs::bindtextdomain(DOMAIN, dir);
    }
    // Rust strings are UTF-8; ask gettext to convert the catalogs to it.
    let _ = gettextrs::bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = gettextrs::textdomain(DOMAIN);
}

/// Where the catalogs are, when there are any.
///
/// The installed layout puts them at `share/locale` next to the binary's
/// `bin` (the Flatpak's `/app`, `/usr` for the Arch package, the Windows
/// bundle's own folder). A source build compiles them into
/// `target/i18n/locale` instead. `DRUKHAL_LOCALE_DIR` overrides both, which
/// is handy for trying a translation without installing it.
fn locale_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DRUKHAL_LOCALE_DIR") {
        return Some(PathBuf::from(dir));
    }
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent()?.parent().map(|prefix| prefix.join("share/locale")));
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/i18n/locale");
    [installed, Some(source)].into_iter().flatten().find(|dir| dir.is_dir())
}

/// Translates a message id. Use the `t!` macro instead.
pub fn text(msgid: &'static str) -> String {
    gettextrs::gettext(msgid)
}

/// Translates a message with a context. Use `tc!` instead.
#[allow(dead_code)] // Only used by `tc!`, which no UI string needs yet.
pub fn text_in(context: &'static str, msgid: &'static str) -> String {
    gettextrs::pgettext(context, msgid)
}

/// Picks the singular or plural message for `count` and translates it.
/// Use `tn!` instead.
pub fn plural(singular: &'static str, plural: &'static str, count: u32) -> String {
    gettextrs::ngettext(singular, plural, count)
}

/// Picks and translates a plural message with a context. Use `tnc!` instead.
#[allow(dead_code)] // Only used by `tnc!`, which no UI string needs yet.
pub fn plural_in(context: &'static str, singular: &'static str, plural: &'static str, count: u32) -> String {
    gettextrs::npgettext(context, singular, plural, count)
}

/// Fills a translated template's `{name}` placeholders.
///
/// A translation with a broken placeholder (a typo such as `{cout}`) would
/// otherwise leave the whole line blank or crash the app, so it falls back
/// to the English text and says so in the log.
pub fn fill(template: &str, english: &str, format: impl Fn(&str) -> Result<String, formatx::Error>) -> String {
    let value = format(template);
    match value {
        Ok(value) => value,
        Err(e) => {
            crate::diagnostics::log!("i18n", "Broken translation \"{template}\": {e}; using the English text");
            format(english).unwrap_or_else(|e| panic!("the English text \"{english}\" does not format: {e}"))
        }
    }
}

/// Translates a string and returns it. The string must be a literal so
/// `xtr` can find it.
#[macro_export]
macro_rules! t {
    ($msgid:literal) => {
        $crate::i18n::text($msgid)
    };
}

/// Translates a string in a context, for words whose translation depends
/// on where they appear ("_Open" as a verb and as an adjective).
#[macro_export]
macro_rules! tc {
    ($context:literal, $msgid:literal) => {
        $crate::i18n::text_in($context, $msgid)
    };
}

/// Translates a string with `{name}` placeholders and fills them in:
/// `tf!("Deleted {count} mods", count = n)`.
#[macro_export]
macro_rules! tf {
    ($msgid:literal, $($name:ident = $value:expr),+ $(,)?) => {{
        #[allow(clippy::redundant_locals)]
        {
            // The values are bound once, so a broken translation can be
            // retried with the English text without evaluating them again.
            $(let $name = $value;)+
            let translated = $crate::i18n::text($msgid);
            #[allow(clippy::needless_borrows_for_generic_args)]
            let format = |template: &str| formatx::formatx!(template, $($name = &$name),+);
            $crate::i18n::fill(&translated, $msgid, format)
        }
    }};
    ($msgid:literal) => {
        $crate::i18n::text($msgid)
    };
}

/// Like `tn!`, and fills the `{name}` placeholders in the chosen form:
/// `tnf!("{count} mod", "{count} mods", count = n)`.
#[macro_export]
macro_rules! tnf {
    ($singular:literal, $plural:literal, $count:ident = $value:expr $(, $name:ident = $v:expr)* $(,)?) => {{
        #[allow(clippy::redundant_locals)]
        {
            let $count = $value;
            $(let $name = $v;)*
            let translated = $crate::i18n::plural($singular, $plural, $count as u32);
            #[allow(clippy::needless_borrows_for_generic_args)]
            let format = |template: &str| formatx::formatx!(template, $count = &$count $(, $name = &$name)*);
            $crate::i18n::fill(&translated, if $count == 1 { $singular } else { $plural }, format)
        }
    }};
}

/// Translates a singular or plural message, without filling placeholders:
/// `tn!("{count} mod", "{count} mods", count)`.
#[macro_export]
macro_rules! tn {
    ($singular:literal, $plural:literal, $count:expr) => {
        $crate::i18n::plural($singular, $plural, $count as u32)
    };
}

/// Like `tn!`, in a context.
#[macro_export]
macro_rules! tnc {
    ($context:literal, $singular:literal, $plural:literal, $count:expr) => {
        $crate::i18n::plural_in($context, $singular, $plural, $count as u32)
    };
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_named_placeholders() {
        // Without a catalog gettext returns the English message id, so this
        // also checks that the fallback text formats.
        assert_eq!(tf!("{name} took {count} of {total}", name = "list", count = 1, total = 2), "list took 1 of 2");
        assert_eq!(tnf!("{count} mod", "{count} mods", count = 2), "2 mods");
        assert_eq!(tnf!("{count} mod", "{count} mods", count = 1), "1 mod");
    }

    #[test]
    fn uses_english_when_a_translation_has_a_broken_placeholder() {
        let text = fill("{naem} took {count}", "{name} took {count}", |template| {
            formatx::formatx!(template, name = &"list", count = &1)
        });
        assert_eq!(text, "list took 1");
    }
}
