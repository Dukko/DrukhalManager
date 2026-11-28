//! What the app works on: the game, the installed mods, the load order
//! and the collections, loaded at start-up the way the GTK version did,
//! and the steps that change them on disk (saving, switching collection,
//! launching). UI-free; the views only read it and call these.

use std::collections::HashMap;
use std::path::PathBuf;

use drukhal_core::load_order_state::State;
use drukhal_core::merge::patch::{self, Patch, Print, Target};
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
    /// The active collection's merged patch, once it could be opened.
    pub patch: Option<Patch>,
    /// Its merges whose mods (or the game) changed since, from the last check.
    pub stale: Vec<patch::Stale>,
    /// The last conflict scan, for checking the patch against.
    pub analysis: Option<std::sync::Arc<drukhal_core::merge::scan::Analysis>>,
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
        let mut library = Self { user_dir, backup_dir: platform::data_dir().join("backups"), game, settings, collections, state, error, issues: HashMap::new(), patch: None, stale: Vec::new(), analysis: None };
        library.recheck();
        library.open_patch();
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
        self.open_patch();
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

    pub fn game_dir(&self) -> Option<PathBuf> {
        self.game.as_ref().map(|g| g.dir.clone())
    }

    /// The active collection's merged patch, giving the collection one if
    /// it has none yet.
    pub fn patch_target(&mut self) -> Target {
        let user_dir = self.user_dir.clone();
        let legacy = Target::new(patch::LEGACY_FOLDER, "").root(&user_dir).is_dir();
        let (target, given) = self.collections.patch_target(legacy, |folder| user_dir.join("mod").join(folder).exists());
        if given && let Err(e) = self.collections.save(&collections::path()) {
            diagnostics::log!("patch", "Could not save collections: {e}");
        }
        target
    }

    /// Opens the active collection's patch, so the views know what's merged.
    pub fn open_patch(&mut self) {
        let target = self.patch_target();
        if self.patch.as_ref().is_some_and(|p| *p.target() == target) {
            return;
        }
        self.patch = match Patch::open(&self.user_dir, &target) {
            Ok(p) => Some(p),
            Err(e) => {
                diagnostics::log!("patch", "Could not read the merged patch: {e}");
                None
            }
        };
        self.check_stale();
    }

    /// Each enabled mod's folder by id, and the game's, for checking the
    /// patch's merges against what their sources hold now.
    fn stale_roots(&self) -> HashMap<String, PathBuf> {
        let mut roots: HashMap<String, PathBuf> =
            self.state.enabled.iter().filter_map(|&i| Some((self.state.mods[i].id.clone(), mods::archive::cached(self.state.mods[i].content.as_deref()?)?))).collect();
        if let Some(dir) = self.game_dir() {
            roots.insert(patch::VANILLA.to_owned(), dir);
        }
        roots
    }

    /// Checks the patch's merges against their sources now, marking those
    /// whose only change can be taken as it is (a picked version that
    /// changed). The last scan, when there is one, knows every enabled
    /// mod's definitions, so merges also notice a mod that started defining
    /// their object after they were made.
    pub fn check_stale(&mut self) {
        let roots = self.stale_roots();
        let version = self.game_version().map(str::to_owned);
        let analysis = self.analysis.clone();
        let definitions = |category: &str, key: &str| -> Vec<(String, String, String)> {
            let Some(a) = &analysis else { return Vec::new() };
            a.report.defined.get(&(category.to_owned(), key.to_owned())).map(|defs| defs.iter().filter_map(|d| Some((a.ids.get(d.mod_position)?.clone(), a.name(d.mod_position).to_owned(), d.file.clone()))).collect()).unwrap_or_default()
        };
        let shippers = |path: &str| -> Vec<(String, String)> {
            let Some(a) = &analysis else { return Vec::new() };
            a.report.shippers.get(&path.to_lowercase()).map(|mods| mods.iter().filter_map(|(position, _)| Some((a.ids.get(*position)?.clone(), a.name(*position).to_owned()))).collect()).unwrap_or_default()
        };
        self.stale = match self.patch.as_mut() {
            Some(patch) => patch.check_with(&roots, version.as_deref(), &definitions, &shippers),
            None => Vec::new(),
        };
    }

    /// Takes a merge (object or file) out of the patch.
    pub fn remove_merge(&mut self, object: Option<&(String, String)>, path: &str) -> Result<String, String> {
        let patch = self.patch.as_mut().ok_or_else(|| t!("The merged patch couldn't be opened"))?;
        match object {
            Some((category, key)) => patch.remove(category, key),
            None => patch.remove_file(path),
        }
        .map_err(|e| tf!("Could not change the merged patch: {error}", error = e))?;
        self.check_stale();
        Ok(tf!("Removed {what} from the merged patch", what = object.map_or(path, |(_, k)| k.as_str())))
    }

    /// Replaces a picked version that its mod changed with the new one.
    pub fn update_pick(&mut self, stale: &patch::Stale) -> Result<String, String> {
        let roots = self.stale_roots();
        let version = self.game_version().map(str::to_owned);
        let user_dir = self.user_dir.clone();
        let patch = self.patch.as_mut().ok_or_else(|| t!("The merged patch couldn't be opened"))?;
        let picks: Vec<patch::Pick> = patch.picks_to_update(&roots).into_iter().filter(|p| p.matches(stale)).collect();
        if picks.is_empty() {
            return Err(t!("There's no newer version to take"));
        }
        patch.update_picks(&user_dir, version.as_deref(), picks).map_err(|e| tf!("Could not change the merged patch: {error}", error = e))?;
        self.check_stale();
        Ok(t!("Took the new version"))
    }

    /// Keeps a stale merge as it is: changes from now on are noticed
    /// again, against `prints` (what its sources hold now).
    pub fn keep_merge(&mut self, stale: &patch::Stale, prints: Vec<Print>) -> Result<String, String> {
        let version = self.game_version().map(str::to_owned);
        let patch = self.patch.as_mut().ok_or_else(|| t!("The merged patch couldn't be opened"))?;
        let result = match (&stale.object, stale.content && !stale.untracked) {
            // Only the saved script changed: accept it as it is.
            (Some((category, key)), true) => patch.accept(category, key),
            _ => patch.track(stale.object.as_ref(), &stale.path, prints, version.as_deref()),
        };
        result.map_err(|e| tf!("Could not change the merged patch: {error}", error = e))?;
        self.check_stale();
        Ok(t!("Kept as it is; changes to its mods will be noticed from now on"))
    }

    /// Whether the patch has a merge for this object.
    pub fn merged(&self, category: &str, key: &str) -> bool {
        self.patch.as_ref().is_some_and(|p| p.get(category, key).is_some())
    }

    /// Whether the patch ships its own copy of this file.
    pub fn merged_file(&self, path: &str) -> bool {
        self.patch.as_ref().is_some_and(|p| p.get_file(path).is_some())
    }

    /// Saves a merged object into the patch. Returns what to tell the player.
    pub fn save_merge(&mut self, category: &str, key: &str, script: &str, prints: Vec<Print>) -> Result<String, String> {
        self.open_patch();
        let game_dir = self.game_dir();
        let version = self.game_version().map(str::to_owned);
        let user_dir = self.user_dir.clone();
        let patch = self.patch.as_mut().ok_or_else(|| t!("The merged patch couldn't be opened"))?;
        patch.save_for_game(&user_dir, version.as_deref(), game_dir.as_deref(), category, key, script, prints).map_err(|e| tf!("Could not save the merged patch: {error}", error = e))?;
        self.check_stale();
        self.patch_saved()
    }

    /// Saves a file, as one mod ships it, into the patch.
    pub fn save_file_merge(&mut self, path: &str, contents: &[u8], prints: Vec<Print>) -> Result<String, String> {
        self.open_patch();
        let version = self.game_version().map(str::to_owned);
        let user_dir = self.user_dir.clone();
        let patch = self.patch.as_mut().ok_or_else(|| t!("The merged patch couldn't be opened"))?;
        patch.save_file(&user_dir, version.as_deref(), path, contents, prints).map_err(|e| tf!("Could not save the merged patch: {error}", error = e))?;
        self.check_stale();
        self.patch_saved()
    }

    /// Makes sure the patch is in the load order, last, once it has
    /// content, and saves the load order if it had to be added.
    fn patch_saved(&mut self) -> Result<String, String> {
        let target = self.patch_target();
        let index = match self.state.mods.iter().position(|m| m.id == target.id()) {
            Some(i) => i,
            None => {
                let descriptor = std::fs::read_to_string(self.user_dir.join(target.id())).ok().and_then(|src| mods::Descriptor::parse(&src).ok()).unwrap_or_default();
                self.state.mods.push(Mod {
                    id: target.id(),
                    descriptor,
                    content: Some(target.root(&self.user_dir)),
                    source: mods::Source::Local,
                    load_position: None,
                    error: None,
                    steam: None,
                    requires: Vec::new(),
                });
                self.state.mods.len() - 1
            }
        };
        if self.state.enabled.contains(&index) {
            return Ok(tf!("Saved to {name}", name = target.name.as_str()));
        }
        // Pinned to the end, like every change.
        self.state.set_enabled(index, true);
        self.save()?;
        self.recheck();
        Ok(tf!("Saved to {name}, which was added to the end of the load order", name = target.name.as_str()))
    }

    /// Ignores conflicts by key (see `merge::scan`), or by an ignore rule
    /// line, and saves the settings.
    pub fn ignore(&mut self, key: Option<String>, rule: Option<String>) -> Result<(), String> {
        if let Some(key) = key {
            self.settings.ignored.insert(key);
        }
        if let Some(rule) = rule {
            if !self.settings.ignore_rules.is_empty() && !self.settings.ignore_rules.ends_with('\n') {
                self.settings.ignore_rules.push('\n');
            }
            self.settings.ignore_rules.push_str(&rule);
            self.settings.ignore_rules.push('\n');
        }
        self.save_settings()
    }

    /// Takes a key, or a rule line, out of what's ignored.
    pub fn unignore(&mut self, key: Option<&str>, rule: Option<&str>) -> Result<(), String> {
        if let Some(key) = key {
            self.settings.ignored.remove(key);
        }
        if let Some(rule) = rule {
            self.settings.ignore_rules = self.settings.ignore_rules.lines().filter(|l| l.trim() != rule.trim()).map(|l| format!("{l}\n")).collect();
        }
        self.save_settings()
    }

    pub fn save_settings(&self) -> Result<(), String> {
        self.settings.save(&settings::path()).map_err(|e| tf!("Could not save settings: {error}", error = e))
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
