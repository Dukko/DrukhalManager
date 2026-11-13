//! Reading and writing the launcher's `dlc_load.json`, which is the load
//! order the game itself reads at startup.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Map, Value};

const FILE_NAME: &str = "dlc_load.json";
const BACKUPS_KEPT: usize = 20;

#[derive(Debug, Default, Deserialize)]
pub struct DlcLoad {
    #[serde(default)]
    pub enabled_mods: Vec<String>,
    /// Descriptor paths of turned-off DLCs, e.g. `dlc/dlc002_arachnoid/dlc002.dlc`.
    #[serde(default)]
    pub disabled_dlcs: Vec<String>,
}

impl DlcLoad {
    pub fn read(user_dir: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(user_dir.join(FILE_NAME)) {
            Ok(src) => serde_json::from_str(&src).map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }
}

/// Replaces `enabled_mods` in `dlc_load.json`, keeping every other field
/// (e.g. `disabled_dlcs`) untouched. The previous file is backed up first and
/// the new one is written atomically. Returns the backup's path, if any.
pub fn save(user_dir: &Path, backup_dir: &Path, enabled_mods: &[String]) -> io::Result<Option<PathBuf>> {
    write(user_dir, backup_dir, "enabled_mods", enabled_mods)
}

/// Replaces `disabled_dlcs`, keeping the load order, like `save`.
pub fn save_disabled_dlcs(user_dir: &Path, backup_dir: &Path, disabled: &[String]) -> io::Result<Option<PathBuf>> {
    write(user_dir, backup_dir, "disabled_dlcs", disabled)
}

fn write(user_dir: &Path, backup_dir: &Path, key: &str, list: &[String]) -> io::Result<Option<PathBuf>> {
    let path = user_dir.join(FILE_NAME);
    let existing = match std::fs::read_to_string(&path) {
        Ok(src) => Some(src),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };

    let mut doc = match &existing {
        Some(src) => match serde_json::from_str::<Value>(src).map_err(io::Error::other)? {
            Value::Object(map) => map,
            _ => return Err(io::Error::other(format!("{} is not a JSON object", path.display()))),
        },
        None => Map::new(),
    };
    doc.insert(key.into(), list.iter().cloned().map(Value::String).collect());
    doc.entry("enabled_mods").or_insert_with(|| Value::Array(Vec::new()));
    doc.entry("disabled_dlcs").or_insert_with(|| Value::Array(Vec::new()));

    let backup = match &existing {
        Some(src) => Some(backup(backup_dir, src)?),
        None => None,
    };

    let tmp = path.with_extension("json.drukhal-tmp");
    {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(&doc).map_err(io::Error::other)?)?;
        file.sync_all()?;
    }
    crate::platform::rename(&tmp, &path)?;
    Ok(backup)
}

fn backup(dir: &Path, contents: &str) -> io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let path = dir.join(format!("dlc_load-{stamp}.json"));
    std::fs::write(&path, contents)?;

    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("dlc_load-")))
        .collect();
    // Millisecond stamps of equal length sort lexically.
    backups.sort();
    for old in backups.iter().rev().skip(BACKUPS_KEPT) {
        std::fs::remove_file(old)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_preserves_other_fields_and_backs_up() {
        let dir = std::env::temp_dir().join(format!("drukhal-load-order-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let original = r#"{"disabled_dlcs":["dlc/x"],"enabled_mods":["mod/a.mod"],"future_field":1}"#;
        std::fs::write(dir.join(FILE_NAME), original).unwrap();

        let backup = save(&dir, &dir.join("backups"), &["mod/b.mod".into(), "mod/a.mod".into()]).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(backup).unwrap(), original);

        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(saved["enabled_mods"], serde_json::json!(["mod/b.mod", "mod/a.mod"]));
        assert_eq!(saved["disabled_dlcs"], serde_json::json!(["dlc/x"]));
        assert_eq!(saved["future_field"], 1);
        assert_eq!(DlcLoad::read(&dir).unwrap().enabled_mods, ["mod/b.mod", "mod/a.mod"]);

        save_disabled_dlcs(&dir, &dir.join("backups"), &["dlc/y".into()]).unwrap();
        let read = DlcLoad::read(&dir).unwrap();
        assert_eq!((read.enabled_mods.as_slice(), read.disabled_dlcs.as_slice()), (["mod/b.mod".to_owned(), "mod/a.mod".to_owned()].as_slice(), ["dlc/y".to_owned()].as_slice()), "keeps the load order");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_creates_missing_file() {
        let dir = std::env::temp_dir().join(format!("drukhal-load-order-new-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(save(&dir, &dir.join("backups"), &["mod/a.mod".into()]).unwrap().is_none());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(dir.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(saved, serde_json::json!({"enabled_mods": ["mod/a.mod"], "disabled_dlcs": []}));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
