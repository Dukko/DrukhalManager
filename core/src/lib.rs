//! DrukhalManager without a UI: finding the game and the mods, the load
//! order and playsets, conflict scans and merging, and the settings. The
//! app's interface (`src/` at the repository root) is built on it; nothing
//! here may depend on a UI toolkit.

#[macro_use]
pub mod i18n;
pub mod dds;
pub mod diagnostics;
pub mod diff;
pub mod load_order_state;
pub mod merge;
pub mod merge_dialog;
pub mod mods;
pub mod multiplayer;
pub mod paradox;
pub mod platform;
pub mod secrets;
pub mod settings;
pub mod update;

/// For the translation macros, so the app needn't depend on it.
#[doc(hidden)]
pub use formatx;

pub const APP_ID: &str = "io.github.dukko.DrukhalManager";
