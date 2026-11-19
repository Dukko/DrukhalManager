//! Zipped mods (`archive=` descriptors, as older launchers and some Workshop
//! items ship them). Their files are read from an extracted copy in the
//! cache, refreshed when the zip changes, so everything that reads mod files
//! works on them the same way as on folders.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Records which zip, at which size and time, a cached copy came from.
/// Hidden, so it is never taken for game content.
const STAMP: &str = ".drukhal-source";

/// Caps on a zip's contents, so a damaged or hostile archive can't fill the
/// disk or the file table.
pub(crate) const MAX_ENTRIES: usize = 100_000;
pub(crate) const MAX_UNPACKED: u64 = 16 << 30;

/// Refuses an archive with too many entries to unpack.
pub(crate) fn check_entries(count: usize) -> io::Result<()> {
    if count > MAX_ENTRIES {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the archive has {count} entries, more than the {MAX_ENTRIES} allowed")));
    }
    Ok(())
}

/// Copies `entry` into `out`, keeping the whole archive under
/// [`MAX_UNPACKED`] bytes.
pub(crate) fn copy_bounded(entry: &mut impl Read, out: &mut impl Write, total: &mut u64) -> io::Result<()> {
    let written = io::copy(&mut entry.take(MAX_UNPACKED.saturating_sub(*total) + 1), out)?;
    *total += written;
    if *total > MAX_UNPACKED {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("the archive holds more than {} bytes of files", MAX_UNPACKED)));
    }
    Ok(())
}

/// Where the files of the mod at `content` (its folder or its zip) can be
/// read. Extracts a zip on first use, or when it changed since.
pub fn files(content: &Path) -> io::Result<PathBuf> {
    files_in(content, &cache_dir())
}

fn cache_dir() -> PathBuf {
    crate::platform::cache_dir().join("archives")
}

/// The folder a zip's copy is unpacked to, and the stamp that shows the
/// copy matches the zip as it is now.
fn location(content: &Path, cache: &Path) -> io::Result<(PathBuf, String)> {
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
    Ok((dir, stamp))
}

/// Like [`files`], but never unpacks: `None` for a zip with no current
/// unpacked copy yet. Cheap enough for the UI thread.
pub fn cached(content: &Path) -> Option<PathBuf> {
    cached_in(content, &cache_dir())
}

pub(crate) fn cached_in(content: &Path, cache: &Path) -> Option<PathBuf> {
    if !content.is_file() {
        return Some(content.to_path_buf());
    }
    let (dir, stamp) = location(content, cache).ok()?;
    std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s == stamp).then_some(dir)
}

pub(crate) fn files_in(content: &Path, cache: &Path) -> io::Result<PathBuf> {
    if !content.is_file() {
        return Ok(content.to_path_buf());
    }
    let (dir, stamp) = location(content, cache)?;
    if std::fs::read_to_string(dir.join(STAMP)).is_ok_and(|s| s == stamp) {
        return Ok(dir);
    }

    // Extract beside the final folder, then swap it in, so an interrupted
    // extraction is never mistaken for a complete one. The unique suffix
    // keeps two jobs unpacking the same changed zip apart.
    let tmp = cache.join(format!(".{}.tmp-{}-{}", dir.file_name().unwrap().to_string_lossy(), std::process::id(), uuid::Uuid::new_v4()));
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
    check_entries(archive.len())?;
    let mut total = 0;
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
        copy_bounded(&mut entry, &mut std::fs::File::create(&path)?, &mut total)?;
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

    #[test]
    fn caps_what_an_archive_can_unpack() {
        assert!(check_entries(MAX_ENTRIES).is_ok());
        assert_eq!(check_entries(MAX_ENTRIES + 1).unwrap_err().kind(), io::ErrorKind::InvalidData);

        let mut out = Vec::new();
        let mut total = MAX_UNPACKED - 1;
        let error = copy_bounded(&mut std::io::Cursor::new(b"ab"), &mut out, &mut total).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(out, b"ab", "writes only the bytes that show the cap is passed");
    }

    #[test]
    fn cached_never_unpacks() {
        let root = std::env::temp_dir().join(format!("drukhal-archive-cached-{}", std::process::id()));
        let cache = root.join("cache");
        std::fs::create_dir_all(&root).unwrap();
        let zip = root.join("old mod.zip");
        write_zip(&zip, &[("common/buildings/a.txt", "a = {}")]);

        // Never unpacked: no copy, and the lookup unpacks nothing.
        assert_eq!(cached_in(&zip, &cache), None);
        assert!(!cache.exists() || std::fs::read_dir(&cache).unwrap().next().is_none());

        // Unpacked: the current copy is found.
        let dir = files_in(&zip, &cache).unwrap();
        assert_eq!(cached_in(&zip, &cache), Some(dir));

        // Changed (different size): the copy is stale.
        write_zip(&zip, &[("common/buildings/a.txt", "a = { cost = 1 }")]);
        assert_eq!(cached_in(&zip, &cache), None);

        // Folders are read in place.
        assert_eq!(cached_in(&root, &cache), Some(root.clone()));

        std::fs::remove_dir_all(root).unwrap();
    }
}
