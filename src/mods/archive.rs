//! Zipped mods (`archive=` descriptors, as older launchers and some Workshop
//! items ship them). Their files are read from an extracted copy in the
//! cache, refreshed when the zip changes, so everything that reads mod files
//! works on them the same way as on folders.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Records which zip, at which size and time, a cached copy came from.
/// Hidden, so it is never taken for game content.
const STAMP: &str = ".drukhal-source";

/// Where the files of the mod at `content` (its folder or its zip) can be
/// read. Extracts a zip on first use, or when it changed since.
pub fn files(content: &Path) -> io::Result<PathBuf> {
    files_in(content, &cache_dir())
}

fn cache_dir() -> PathBuf {
    crate::platform::cache_dir().join("archives")
}

pub(crate) fn files_in(content: &Path, cache: &Path) -> io::Result<PathBuf> {
    if !content.is_file() {
        return Ok(content.to_path_buf());
    }
    let meta = std::fs::metadata(content)?;
    let modified = meta.modified()?.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let stamp = format!("{}\n{}\n{}\n", content.display(), meta.len(), modified);

    let mut hasher = DefaultHasher::new();
    content.hash(&mut hasher);
    let stem: String = content
        .file_stem()
        .map(|s| s.to_string_lossy().chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).take(40).collect())
        .unwrap_or_default();
    let dir = cache.join(format!("{stem}-{:016x}", hasher.finish()));
    if std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s == stamp) {
        return Ok(dir);
    }

    // Extract beside the final folder, then swap it in, so an interrupted
    // extraction is never mistaken for a complete one.
    let tmp = cache.join(format!(".{}.tmp-{}", dir.file_name().unwrap().to_string_lossy(), std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    let result = extract(content, &tmp).and_then(|()| std::fs::write(tmp.join(STAMP), &stamp));
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&dir);
    crate::platform::rename(&tmp, &dir)?;
    Ok(dir)
}

fn extract(zip: &Path, to: &Path) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).map_err(io::Error::other)?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(io::Error::other)?;
        // Entries with `..` or absolute paths would land outside the folder.
        let Some(name) = entry.enclosed_name() else { continue };
        let path = to.join(name);
        if entry.is_dir() {
            std::fs::create_dir_all(&path)?;
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut std::fs::File::create(&path)?)?;
    }
    Ok(())
}

/// The zip inside a Workshop item's folder, for an `archive=` mod found by
/// its id rather than its (stale) path.
pub fn find_zip(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .find(|p| p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")))
}

#[cfg(test)]
pub(crate) fn write_zip(path: &Path, files: &[(&str, &str)]) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, body) in files {
        zip.start_file(*name, options).unwrap();
        zip.write_all(body.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_zips_once_and_again_when_they_change() {
        let root = std::env::temp_dir().join(format!("drukhal-archive-{}", std::process::id()));
        let cache = root.join("cache");
        std::fs::create_dir_all(&root).unwrap();
        let zip = root.join("old mod.zip");
        write_zip(&zip, &[("common/buildings/a.txt", "a = {}"), ("../escape.txt", "no"), ("descriptor.mod", "name=\"x\"")]);

        let dir = files_in(&zip, &cache).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("common/buildings/a.txt")).unwrap(), "a = {}");
        assert!(!root.join("escape.txt").exists() && !cache.join("escape.txt").exists());
        // Cached: a file added to the copy survives the next lookup.
        std::fs::write(dir.join("marker"), "").unwrap();
        assert_eq!(files_in(&zip, &cache).unwrap(), dir);
        assert!(dir.join("marker").exists());

        // Changed (different size): extracted afresh.
        write_zip(&zip, &[("common/buildings/a.txt", "a = { cost = 1 }")]);
        assert_eq!(files_in(&zip, &cache).unwrap(), dir);
        assert!(!dir.join("marker").exists());
        assert_eq!(std::fs::read_to_string(dir.join("common/buildings/a.txt")).unwrap(), "a = { cost = 1 }");

        // Folders are read in place; a broken zip is an error.
        assert_eq!(files_in(&root, &cache).unwrap(), root);
        std::fs::write(root.join("broken.zip"), "not a zip").unwrap();
        assert!(files_in(&root.join("broken.zip"), &cache).is_err());
        assert_eq!(std::fs::read_dir(&cache).unwrap().filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".tmp-")).count(), 0);

        std::fs::remove_dir_all(root).unwrap();
    }
}
