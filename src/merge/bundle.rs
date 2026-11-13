//! A collection shared together with the merged patch, as one zip, like
//! Irony Mod Manager's Export Whole Collection: `collection.json` holds the
//! load order and `patch/` the patch folder, merge records included.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use zip::write::SimpleFileOptions;

use super::patch::Target;
use super::snapshot;
use crate::mods::collections::Shared;

const COLLECTION: &str = "collection.json";
const PATCH: &str = "patch/";

/// Writes `shared` and, if there is one, its patch `target` in `user_dir`
/// to a zip at `path`. Returns how many patch files went in.
pub fn export(path: &Path, shared: &Shared, user_dir: &Path, target: &Target) -> io::Result<usize> {
    let tmp = path.with_extension("zip.drukhal-tmp");
    let result = (|| {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&tmp)?);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file(COLLECTION, options).map_err(io::Error::other)?;
        zip.write_all(&serde_json::to_vec_pretty(shared).map_err(io::Error::other)?)?;
        let root = target.root(user_dir);
        let mut files = Vec::new();
        if root.is_dir() {
            list(&root, &root, &mut files)?;
        }
        for relative in &files {
            zip.start_file(format!("{PATCH}{relative}"), options).map_err(io::Error::other)?;
            io::copy(&mut std::fs::File::open(root.join(relative))?, &mut zip)?;
        }
        zip.finish().map_err(io::Error::other)?.sync_all()?;
        std::fs::rename(&tmp, path)?;
        Ok(files.len())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Every file under `dir`, relative to `root` with `/` separators.
fn list(root: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            list(root, &path, out)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push(crate::platform::game_path(relative));
        }
    }
    out.sort();
    Ok(())
}

/// A shared bundle, as read.
pub struct Bundle {
    pub path: PathBuf,
    pub collection: Shared,
    /// How many files its patch has; 0 if it has none.
    pub patch_files: usize,
}

/// Reads the load order from the zip at `path`, if it is a bundle.
pub fn read(path: &Path) -> io::Result<Bundle> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path)?).map_err(io::Error::other)?;
    let mut json = String::new();
    zip.by_name(COLLECTION)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not a collection shared with its merged patch"))?
        .read_to_string(&mut json)?;
    let collection = serde_json::from_str(&json).map_err(io::Error::other)?;
    let patch_files = zip.file_names().filter_map(Result::ok).filter(|n| n.starts_with(PATCH) && !n.ends_with('/')).count();
    Ok(Bundle { path: path.to_owned(), collection, patch_files })
}

/// Makes the bundle's patch the patch `target` in `user_dir`, after backing
/// up what `target` held to `backup_dir`.
pub fn install_patch(bundle: &Bundle, user_dir: &Path, target: &Target, backup_dir: &Path) -> io::Result<()> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(&bundle.path)?).map_err(io::Error::other)?;
    let staging = backup_dir.join(format!("imported-patch-{}.drukhal-tmp", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| {
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i).map_err(io::Error::other)?;
            // Entries with `..` or absolute paths would land outside it.
            let Some(name) = entry.enclosed_name() else { continue };
            let Ok(relative) = name.strip_prefix(PATCH.trim_end_matches('/')) else { continue };
            if entry.is_dir() || relative.as_os_str().is_empty() {
                continue;
            }
            let target = staging.join(relative);
            std::fs::create_dir_all(target.parent().unwrap())?;
            io::copy(&mut entry, &mut std::fs::File::create(&target)?)?;
        }
        if !staging.join("descriptor.mod").is_file() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "the shared patch has no descriptor"));
        }
        snapshot::take(user_dir, target, backup_dir, snapshot::BEFORE_IMPORTING)?;
        snapshot::replace_patch(user_dir, target, &staging)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merge::patch;
    use crate::mods::collections::SharedMod;

    #[test]
    fn round_trips_with_the_patch() {
        let root = std::env::temp_dir().join(format!("drukhal-bundle-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (mine, theirs) = (root.join("mine"), root.join("theirs"));
        let patch = patch::test_target().root(&mine);
        std::fs::create_dir_all(patch.join("common/buildings")).unwrap();
        std::fs::write(patch.join("descriptor.mod"), "name=\"DrukhalManager Merged Patch\"\n").unwrap();
        std::fs::write(patch.join("common/buildings/zz_merged.txt"), "a = {}").unwrap();
        let shared = Shared { format: 1, name: "Main".into(), mods: vec![SharedMod { name: "A".into(), steam_id: Some(1), id: "mod/ugc_1.mod".into(), note: None }] };
        let path = root.join("Main.zip");
        assert_eq!(export(&path, &shared, &mine, &patch::test_target()).unwrap(), 2);

        let bundle = read(&path).unwrap();
        assert_eq!((bundle.collection.name.as_str(), bundle.patch_files), ("Main", 2));
        // Their own patch is backed up and replaced.
        let theirs_patch = patch::test_target().root(&theirs);
        std::fs::create_dir_all(&theirs_patch).unwrap();
        std::fs::write(theirs_patch.join("descriptor.mod"), "name=\"old\"\n").unwrap();
        install_patch(&bundle, &theirs, &patch::test_target(), &root.join("backups")).unwrap();
        assert_eq!(std::fs::read_to_string(theirs_patch.join("common/buildings/zz_merged.txt")).unwrap(), "a = {}");
        assert!(std::fs::read_to_string(theirs.join(patch::test_target().id())).unwrap().contains("path="));
        let backups = snapshot::list(&root.join("backups"), &patch::test_target());
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0].reason, snapshot::BEFORE_IMPORTING);

        std::fs::write(root.join("plain.zip"), b"PK").unwrap();
        assert!(read(&root.join("plain.zip")).is_err());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
