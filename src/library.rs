//! What the app works on: the game, the installed mods, the load order
//! and the collections, loaded at start-up the way the GTK version did,
//! and the steps that change them on disk (saving, switching collection,
//! launching). UI-free; the views only read it and call these.

use std::collections::HashMap;
use std::path::PathBuf;

use drukhal_core::load_order_state::State;
use drukhal_core::mods::collections::{self, Collections};
use drukhal_core::mods::{self, Mod, checks};
use drukhal_core::paradox::{game, launcher_db, steam};
use drukhal_core::settings::{self, Settings};
use drukhal_core::{diagnostics, platform};

pub struct Library {
    pub user_dir: PathBuf,
    pub backup_dir: PathBuf,
    pub game: Option<game::GameInstall>,
    pub settings: Settings,
    pub collections: Collections,
    pub state: State,
    /// Why the mods couldn't be read, if they couldn't.
    pub error: Option<String>,
    /// Each mod's problems (outdated, a requirement off or missing), by
    /// index into `state.mods`, for the current load order.
    pub issues: HashMap<usize, Vec<checks::Issue>>,
}

impl Library {
    /// Finds the game and the mods, and reads the load order and the
    /// collections. Blocking: a few hundred small files.
    pub fn load() -> Self {
        let home = platform::home_dir();
        let settings = Settings::load(&settings::path());
        let options = &settings.game;
        let user_dir = options.data_dir.clone().unwrap_or_else(steam::stellaris_user_dir);
        let workshop_dirs = steam::workshop_dirs(&home, steam::STELLARIS_APP_ID);
        // Mods subscribed or updated since the Paradox Launcher last ran.
        for error in mods::sync::sync(&user_dir, &workshop_dirs).errors {
            diagnostics::log!("mods", "{error}");
        }
        for error in mods::extra::link(&user_dir, options.extra_mod_dir.as_deref()).errors {
            diagnostics::log!("mods", "{error}");
        }
        let game = match &options.executable {
            Some(exe) => game::from_executable(exe).ok(),
            None => game::find(&steam::library_folders(&home)),
        };
        let (found, error) = match mods::discover(&user_dir, &workshop_dirs) {
            Ok(found) => (found, None),
            Err(e) => {
                diagnostics::log!("app", "Could not read the mods: {e}");
                (Vec::new(), Some(e.to_string()))
            }
        };
        diagnostics::log!("app", "{} mods found; game {}", found.len(), game.as_ref().and_then(|g| g.version.as_deref()).unwrap_or("not found"));
        let state = State::new(found);
        let collections = load_collections(&state);
        let mut library = Self { user_dir, backup_dir: platform::data_dir().join("backups"), game, settings, collections, state, error, issues: HashMap::new() };
        library.recheck();
        library
    }

    /// Works the mods' problems out again, after the load order changed.
    pub fn recheck(&mut self) {
        let version = self.game.as_ref().and_then(|g| g.version.as_deref());
        self.issues = checks::check(&self.state.mods, &self.state.enabled, version);
    }

    pub fn game_version(&self) -> Option<&str> {
        self.game.as_ref().and_then(|g| g.version.as_deref())
    }

    /// Saves the load order to the game's `dlc_load.json`, the active
    /// collection, and the Paradox Launcher's playset. Returns what to
    /// tell the player.
    pub fn save(&mut self) -> Result<String, String> {
        let ids = self.state.enabled_ids();
        mods::load_order::save(&self.user_dir, &self.backup_dir, &ids).map_err(|e| tf!("Could not save load order: {error}", error = e))?;
        self.state.dirty = false;
        let active = self.collections.active.clone();
        if let Some(c) = self.collections.get_mut(&active) {
            c.update(ids, &self.state.mods);
        }
        if let Err(e) = self.collections.save(&collections::path()) {
            return Err(tf!("Could not save collections: {error}", error = e));
        }
        Ok(self.sync_launcher())
    }

    /// Mirrors the saved load order into the Paradox Launcher's playsets.
    fn sync_launcher(&self) -> String {
        let playset: Vec<launcher_db::PlaysetMod> = self
            .state
            .enabled
            .iter()
            .map(|&i| {
                let m = &self.state.mods[i];
                launcher_db::PlaysetMod {
                    registry_id: m.id.clone(),
                    steam_id: m.descriptor.remote_file_id,
                    name: mods::display_name(m).to_owned(),
                    dir: m.content.clone(),
                    tags: m.descriptor.tags.clone(),
                    supported_version: m.descriptor.game_version().to_owned(),
                }
            })
            .collect();
        let name = collections::playset_name(&self.collections.active);
        match launcher_db::sync(&self.user_dir, &self.backup_dir, &name, &playset) {
            Ok(None) => t!("Load order saved"),
            Ok(Some(synced)) if synced.missing.is_empty() => tf!("Load order saved and set as the “{playset}” playset in the Paradox Launcher", playset = name),
            Ok(Some(synced)) => tnf!(
                "Load order saved. The Paradox Launcher doesn't know {count} mod yet",
                "Load order saved. The Paradox Launcher doesn't know {count} mods yet",
                count = synced.missing.len()
            ),
            Err(e) => tf!("Load order saved, but the Paradox Launcher's playset wasn't updated: {error}", error = e),
        }
    }

    /// Makes `name` the active collection and its mods the load order,
    /// saving the current one first if it has changes.
    pub fn switch_collection(&mut self, name: &str) -> Result<String, String> {
        if name == self.collections.active || self.collections.get(name).is_none() {
            return Ok(String::new());
        }
        if self.state.dirty {
            self.save()?;
        }
        self.collections.active = name.to_owned();
        let mods = self.collections.active().mods.clone();
        let missing = self.state.set_enabled_ids(&mods);
        let saved = self.save()?;
        self.recheck();
        Ok(if missing.is_empty() {
            saved
        } else {
            tnf!(
                "{count} mod in this collection isn't installed; it keeps its place for when it's back",
                "{count} mods in this collection aren't installed; they keep their place for when they're back",
                count = missing.len()
            )
        })
    }

    /// Starts a collection holding the current load order, and switches to it.
    pub fn new_collection(&mut self) -> Result<String, String> {
        if self.state.dirty {
            self.save()?;
        }
        let name = self.collections.unique_name(&t!("New Collection"));
        let name = self.collections.add(&name, self.state.enabled_ids());
        self.collections.active = name.clone();
        self.save()?;
        Ok(tf!("Started the collection “{name}” from this load order", name = name))
    }

    /// Saves if needed and starts the game: through Steam in the Flatpak
    /// (which can't start it itself), else directly with the launch
    /// options from Preferences.
    pub fn launch(&mut self, resume: bool) -> Result<String, String> {
        let Some(game) = self.game.clone() else { return Err(t!("Stellaris wasn't found. Set where it is in Preferences.")) };
        if self.state.dirty {
            self.save()?;
        }
        diagnostics::log!("game", "Launching with {} enabled mods{}", self.state.enabled.len(), if resume { ", continuing the last save" } else { "" });
        if platform::is_flatpak() {
            let id = steam::STELLARIS_APP_ID;
            let uri = if resume { format!("steam://run/{id}//{}/", game::CONTINUE) } else { format!("steam://rungameid/{id}") };
            platform::open(&uri).map_err(|e| tf!("Could not start Stellaris: {error}", error = e))?;
            return Ok(t!("Starting Stellaris through Steam…"));
        }
        let mut extra = game::parse_launch_args(&self.settings.game.launch_args).map_err(|error| tf!("Check the game launch options in Preferences: {error}", error = error))?;
        if resume {
            extra.push(game::CONTINUE.to_owned());
        }
        let mut child = game.launch(&extra).map_err(|e| tf!("Could not start Stellaris: {error}", error = e))?;
        // Reaped when the game exits.
        std::thread::spawn(move || drop(child.wait()));
        Ok(t!("Starting Stellaris…"))
    }

    pub fn mod_at(&self, i: usize) -> &Mod {
        &self.state.mods[i]
    }
}

/// The collections file, or a first collection holding the current load
/// order. An unreadable file is set aside rather than overwritten.
fn load_collections(state: &State) -> Collections {
    let path = collections::path();
    match Collections::load(&path) {
        Ok(Some(c)) => c,
        Ok(None) => {
            let c = Collections::starting_with(state.enabled_ids());
            if let Err(e) = c.save(&path) {
                diagnostics::log!("app", "Could not save collections: {e}");
            }
            c
        }
        Err(e) => {
            diagnostics::log!("app", "Could not read collections ({e}); keeping the file aside");
            let _ = platform::rename(&path, path.with_extension("json.unreadable"));
            Collections::starting_with(state.enabled_ids())
        }
    }
}
