//! Repairing mod descriptors: rewriting `path` entries that point at folders
//! that no longer exist (e.g. after moving a Steam library), and clearing out
//! descriptors whose mod is gone. Irony Mod Manager does the same kind of
//! cleanup (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).
//! Every changed or removed file is copied to a backup folder first.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::Mod;
use crate::paradox::script::{self, Entry};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePath {
    pub id: String,
    pub old: String,
    pub new: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Orphan {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Findings {
    pub stale: Vec<StalePath>,
    pub orphans: Vec<Orphan>,
}

impl Findings {
    pub fn is_empty(&self) -> bool {
        self.stale.is_empty() && self.orphans.is_empty()
    }
}

/// Descriptors in `<user_dir>/mod` whose `path` is wrong but whose files were
/// found elsewhere, and those whose files can't be found at all.
pub fn find(user_dir: &Path, mods: &[Mod]) -> Findings {
    let mut findings = Findings::default();
    for m in mods {
        if !user_dir.join(&m.id).is_file() {
            continue; // an enabled entry whose descriptor is already gone
        }
        let Some(old) = m.descriptor.path.as_deref() else { continue };
        let resolved = if Path::new(old).is_absolute() { PathBuf::from(old) } else { user_dir.join(old) };
        match &m.content {
            Some(found) if !resolved.exists() && *found != resolved => findings.stale.push(StalePath { id: m.id.clone(), old: old.to_owned(), new: found.clone() }),
            None => findings.orphans.push(Orphan { id: m.id.clone(), name: if m.descriptor.name.is_empty() { m.id.clone() } else { m.descriptor.name.clone() } }),
            _ => {}
        }
    }
    findings
}

/// `src` with its top-level `path` (or `archive`) value replaced.
fn rewrite_path(src: &str, new: &Path) -> Option<String> {
    let entries = script::parse_with_spans(src).ok()?;
    let (key, range) = entries.iter().find_map(|(e, r)| match e {
        Entry::Assign { key, .. } if key == "path" => Some(("path", r.clone())),
        _ => None,
    }).or_else(|| entries.iter().find_map(|(e, r)| match e {
        Entry::Assign { key, .. } if key == "archive" => Some(("archive", r.clone())),
        _ => None,
    }))?;
    let value = new.to_string_lossy().replace('\\', "/");
    Some(format!("{}{key}=\"{value}\"{}", &src[..range.start], &src[range.end..]))
}

/// Applies the repairs and removals, backing every touched descriptor up to
/// a new folder in `backup_dir`, which is returned.
pub fn apply(user_dir: &Path, backup_dir: &Path, stale: &[StalePath], remove: &[Orphan]) -> io::Result<PathBuf> {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let backup = backup_dir.join(format!("descriptors-{stamp}"));
    std::fs::create_dir_all(&backup)?;
    let backup_of = |id: &str| backup.join(Path::new(id).file_name().unwrap());

    for s in stale {
        let file = user_dir.join(&s.id);
        let src = std::fs::read_to_string(&file)?;
        let Some(fixed) = rewrite_path(&src, &s.new) else { continue };
        std::fs::write(backup_of(&s.id), &src)?;
        let tmp = file.with_extension("mod.drukhal-tmp");
        std::fs::write(&tmp, fixed)?;
        std::fs::rename(&tmp, &file)?;
    }
    for o in remove {
        let file = user_dir.join(&o.id);
        // Moved, not deleted: the backup folder can restore it.
        if std::fs::rename(&file, backup_of(&o.id)).is_err() {
            std::fs::copy(&file, backup_of(&o.id))?;
            std::fs::remove_file(&file)?;
        }
    }
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_and_repairs_descriptors() {
        let root = std::env::temp_dir().join(format!("drukhal-repair-{}", std::process::id()));
        let (user, workshop) = (root.join("user"), root.join("workshop"));
        std::fs::create_dir_all(user.join("mod/local")).unwrap();
        std::fs::create_dir_all(workshop.join("42")).unwrap();
        let moved = "version=\"1\"\nname=\"Moved\"\npath=\"/old/library/42\"\nremote_file_id=\"42\"\n";
        std::fs::write(user.join("mod/ugc_42.mod"), moved).unwrap();
        std::fs::write(user.join("mod/local.mod"), "name=\"Fine\" path=\"mod/local\"").unwrap();
        std::fs::write(user.join("mod/gone.mod"), "name=\"Gone\" path=\"/nowhere\"").unwrap();

        let mods = crate::mods::discover(&user, std::slice::from_ref(&workshop)).unwrap();
        let findings = find(&user, &mods);
        assert_eq!(findings.stale, [StalePath { id: "mod/ugc_42.mod".into(), old: "/old/library/42".into(), new: workshop.join("42") }]);
        assert_eq!(findings.orphans, [Orphan { id: "mod/gone.mod".into(), name: "Gone".into() }]);

        let backup = apply(&user, &root.join("backups"), &findings.stale, &findings.orphans).unwrap();
        let fixed = std::fs::read_to_string(user.join("mod/ugc_42.mod")).unwrap();
        assert_eq!(fixed, format!("version=\"1\"\nname=\"Moved\"\npath=\"{}\"\nremote_file_id=\"42\"\n", workshop.join("42").display()));
        assert!(!user.join("mod/gone.mod").exists());
        assert_eq!(std::fs::read_to_string(backup.join("ugc_42.mod")).unwrap(), moved);
        assert!(backup.join("gone.mod").is_file());
        assert!(find(&user, &crate::mods::discover(&user, &[workshop]).unwrap()).is_empty());

        std::fs::remove_dir_all(root).unwrap();
    }
}
