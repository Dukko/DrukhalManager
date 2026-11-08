//! Mirroring the load order into the Paradox Launcher's database
//! (`launcher-v2.sqlite`), so the launcher shows the same mod list.
//!
//! The launcher reads its playsets from this database, not `dlc_load.json`.
//! Like Irony Mod Manager's `SQLiteExporter`
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager), this keeps
//! one playset of our own, lists its mods by the launcher's internal mod ids,
//! and registers local mods the launcher hasn't seen yet.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

pub const FILE_NAME: &str = "launcher-v2.sqlite";
const BACKUPS_KEPT: usize = 10;

/// One enabled mod, in load order.
#[derive(Debug, Clone)]
pub struct PlaysetMod {
    /// e.g. `mod/ugc_123.mod`; the launcher calls this `gameRegistryId`.
    pub registry_id: String,
    pub steam_id: Option<u64>,
    pub name: String,
    /// The mod's folder; needed to register a local mod.
    pub dir: Option<PathBuf>,
    pub tags: Vec<String>,
    pub supported_version: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Synced {
    /// Mods the launcher doesn't know and that couldn't be registered
    /// (workshop mods it hasn't scanned yet). They're left out of the playset.
    pub missing: Vec<String>,
    /// Local mods newly registered with the launcher.
    pub registered: Vec<String>,
}

/// Writes `mods` as the playset `playset` and makes it the active one.
/// Returns `Ok(None)` if the launcher has never been run (no database).
pub fn sync(user_dir: &Path, backup_dir: &Path, playset: &str, mods: &[PlaysetMod]) -> Result<Option<Synced>, String> {
    let path = user_dir.join(FILE_NAME);
    if !path.is_file() {
        return Ok(None);
    }
    let mut db = Connection::open(&path).map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(3)).map_err(|e| e.to_string())?;
    backup(&db, backup_dir).map_err(|e| format!("could not back up the launcher database: {e}"))?;
    write(&mut db, playset, mods).map(Some).map_err(busy_message)
}

fn busy_message(e: rusqlite::Error) -> String {
    match e {
        rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::DatabaseBusy => "the Paradox Launcher is using its database; close it and save again".to_owned(),
        e => e.to_string(),
    }
}

/// The launcher's playsets with their enabled mods' descriptor ids, in
/// order, for importing. Empty if the launcher has never been run.
pub fn playsets(user_dir: &Path) -> Result<Vec<(String, Vec<String>)>, String> {
    let path = user_dir.join(FILE_NAME);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let db = Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let read = || -> rusqlite::Result<Vec<(String, Vec<String>)>> {
        let mut playsets = Vec::new();
        let mut names = db.prepare("SELECT id, name FROM playsets WHERE isRemoved = 0 ORDER BY name")?;
        let mut mods = db.prepare("SELECT m.gameRegistryId FROM playsets_mods pm JOIN mods m ON m.id = pm.modId WHERE pm.playsetId = ?1 AND pm.enabled = 1 AND m.gameRegistryId IS NOT NULL ORDER BY pm.position")?;
        for row in names.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, name) = row?;
            let ids = mods.query_map([&id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?;
            playsets.push((name, ids));
        }
        Ok(playsets)
    };
    read().map_err(busy_message)
}

/// Renames a playset, if it exists.
pub fn rename(user_dir: &Path, from: &str, to: &str) -> Result<(), String> {
    let path = user_dir.join(FILE_NAME);
    if !path.is_file() {
        return Ok(());
    }
    let db = Connection::open(&path).map_err(|e| e.to_string())?;
    db.busy_timeout(std::time::Duration::from_secs(3)).map_err(|e| e.to_string())?;
    db.execute("UPDATE playsets SET name = ?2, updatedOn = ?3 WHERE name = ?1 AND isRemoved = 0", params![from, to, now_ms()]).map(|_| ()).map_err(busy_message)
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64
}

/// A consistent copy, including anything still in the write-ahead log.
fn backup(db: &Connection, dir: &Path) -> rusqlite::Result<()> {
    std::fs::create_dir_all(dir).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
    let target = dir.join(format!("launcher-v2-{}.sqlite", now_ms()));
    db.execute("VACUUM INTO ?1", [target.to_string_lossy()])?;

    let mut backups: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("launcher-v2-"))).collect())
        .unwrap_or_default();
    backups.sort();
    for old in backups.iter().rev().skip(BACKUPS_KEPT) {
        let _ = std::fs::remove_file(old);
    }
    Ok(())
}

fn write(db: &mut Connection, playset_name: &str, mods: &[PlaysetMod]) -> rusqlite::Result<Synced> {
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut synced = Synced::default();

    let mut ids = Vec::new();
    for m in mods {
        let mut id: Option<String> = tx.query_row("SELECT id FROM mods WHERE gameRegistryId = ?1", [&m.registry_id], |r| r.get(0)).optional()?;
        if id.is_none()
            && let Some(steam_id) = m.steam_id
        {
            id = tx.query_row("SELECT id FROM mods WHERE steamId = ?1 AND source = 'steam'", [steam_id.to_string()], |r| r.get(0)).optional()?;
        }
        if id.is_none()
            && m.steam_id.is_none()
            && let Some(dir) = &m.dir
        {
            let new_id = gtk::glib::uuid_string_random().to_string();
            tx.execute(
                "INSERT INTO mods (id, gameRegistryId, displayName, tags, requiredVersion, dirPath, status, source, isNew, createdDate, isMetadataApplied, metadataStatus, keepLatest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready_to_play', 'local', 0, ?7, 0, 'not_applied', 1)",
                params![new_id, m.registry_id, m.name, serde_json::to_string(&m.tags).unwrap(), m.supported_version, dir.to_string_lossy(), now_ms()],
            )?;
            synced.registered.push(m.name.clone());
            id = Some(new_id);
        }
        match id {
            Some(id) => ids.push(id),
            None => synced.missing.push(m.name.clone()),
        }
    }

    let playset: Option<String> = tx.query_row("SELECT id FROM playsets WHERE name = ?1 AND isRemoved = 0", [playset_name], |r| r.get(0)).optional()?;
    let playset = match playset {
        Some(id) => {
            tx.execute("UPDATE playsets SET updatedOn = ?2 WHERE id = ?1", params![id, now_ms()])?;
            id
        }
        None => {
            let id = gtk::glib::uuid_string_random().to_string();
            tx.execute("INSERT INTO playsets (id, name, isActive, loadOrder, createdOn) VALUES (?1, ?2, 0, 'custom', ?3)", params![id, playset_name, now_ms()])?;
            id
        }
    };
    tx.execute("UPDATE playsets SET isActive = (id = ?1)", [&playset])?;
    tx.execute("DELETE FROM playsets_mods WHERE playsetId = ?1", [&playset])?;
    for (position, id) in ids.iter().enumerate() {
        tx.execute("INSERT INTO playsets_mods (playsetId, modId, enabled, position) VALUES (?1, ?2, 1, ?3)", params![playset, id, position as i64])?;
    }
    tx.commit()?;
    Ok(synced)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parts of the launcher's schema this module touches.
    const SCHEMA: &str = "
        CREATE TABLE mods (id char(36) primary key, steamId varchar(255), gameRegistryId text, displayName varchar(255), tags json default '[]',
            requiredVersion varchar(255), dirPath text, status text not null, source text not null, isNew boolean, createdDate datetime,
            isMetadataApplied boolean not null default '0', metadataStatus text not null default 'not_applied', keepLatest boolean not null default '1');
        CREATE TABLE playsets (id char(36) primary key, name varchar(255) not null, isActive boolean, loadOrder varchar(255), createdOn datetime not null,
            updatedOn datetime, isRemoved boolean not null default false);
        CREATE TABLE playsets_mods (playsetId char(36) not null, modId char(36) not null, enabled boolean default '1', position integer);
        INSERT INTO mods (id, steamId, gameRegistryId, status, source) VALUES ('m1', '111', 'mod/ugc_111.mod', 'ready_to_play', 'steam');
        INSERT INTO mods (id, steamId, gameRegistryId, status, source) VALUES ('m2', '222', 'mod/old_name.mod', 'ready_to_play', 'steam');
        INSERT INTO playsets (id, name, isActive, createdOn) VALUES ('irony', 'IronyModManager', 1, 0);";

    fn playset_mod(registry_id: &str, steam_id: Option<u64>, dir: Option<&str>) -> PlaysetMod {
        PlaysetMod { registry_id: registry_id.into(), steam_id, name: registry_id.into(), dir: dir.map(PathBuf::from), tags: vec!["Fixes".into()], supported_version: "v4.5.*".into() }
    }

    #[test]
    fn writes_active_playset_in_order() {
        let dir = std::env::temp_dir().join(format!("drukhal-launcher-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Connection::open(dir.join(FILE_NAME)).unwrap().execute_batch(SCHEMA).unwrap();

        let mods = [
            playset_mod("mod/patch.mod", None, Some("/mods/patch")),
            playset_mod("mod/ugc_222.mod", Some(222), None),
            playset_mod("mod/ugc_111.mod", Some(111), None),
            playset_mod("mod/ugc_999.mod", Some(999), None),
        ];
        let synced = sync(&dir, &dir.join("backups"), "DrukhalManager", &mods).unwrap().unwrap();
        assert_eq!(synced, Synced { missing: vec!["mod/ugc_999.mod".into()], registered: vec!["mod/patch.mod".into()] });
        // Saving again reuses the playset and the registered mod.
        let synced = sync(&dir, &dir.join("backups"), "DrukhalManager", &mods[1..3]).unwrap().unwrap();
        assert_eq!(synced, Synced::default());

        let db = Connection::open(dir.join(FILE_NAME)).unwrap();
        let active: Vec<(String, bool)> = db.prepare("SELECT name, isActive FROM playsets ORDER BY name").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        assert_eq!(active, [("DrukhalManager".to_owned(), true), ("IronyModManager".to_owned(), false)]);
        let order: Vec<String> = db
            .prepare("SELECT pm.modId FROM playsets_mods pm JOIN playsets p ON p.id = pm.playsetId WHERE p.name = 'DrukhalManager' ORDER BY position")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(order, ["m2", "m1"]);
        let local: i64 = db.query_row("SELECT count(*) FROM mods WHERE source = 'local' AND gameRegistryId = 'mod/patch.mod'", [], |r| r.get(0)).unwrap();
        assert_eq!(local, 1);
        assert_eq!(std::fs::read_dir(dir.join("backups")).unwrap().count(), 2);
        drop(db);

        let listed = playsets(&dir).unwrap();
        assert_eq!(listed, [("DrukhalManager".to_owned(), vec!["mod/old_name.mod".to_owned(), "mod/ugc_111.mod".to_owned()]), ("IronyModManager".to_owned(), vec![])]);
        rename(&dir, "DrukhalManager", "DrukhalManager – Renamed").unwrap();
        assert_eq!(playsets(&dir).unwrap()[0].0, "DrukhalManager – Renamed");

        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn skips_when_launcher_never_ran() {
        let dir = std::env::temp_dir().join(format!("drukhal-launcher-none-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(sync(&dir, &dir.join("backups"), "DrukhalManager", &[]).unwrap(), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
