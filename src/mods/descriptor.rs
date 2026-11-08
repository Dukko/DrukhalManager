//! `.mod` descriptor files.
//!
//! Field handling (`path` falling back to `archive`, `supported_version` to
//! `version`) mirrors Irony Mod Manager's `ModParser`
//! (MIT, © 2020 Mario, https://github.com/bcssov/IronyModManager).

use crate::paradox::script::{self, Entries, ParseError};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Descriptor {
    pub name: String,
    /// Mod folder, or zip file for `archive` mods.
    pub path: Option<String>,
    pub is_archive: bool,
    pub version: String,
    pub supported_version: String,
    pub tags: Vec<String>,
    pub picture: Option<String>,
    pub remote_file_id: Option<u64>,
    pub dependencies: Vec<String>,
    pub replace_path: Vec<String>,
    pub user_dir: Vec<String>,
}

impl Descriptor {
    pub fn parse(src: &str) -> Result<Self, ParseError> {
        let entries = script::parse(src)?;
        let entries = entries.as_slice();
        let owned = |key: &str| entries.scalar(key).map(str::to_owned);
        let list = |key: &str| {
            // `replace_path` and `user_dir` may be repeated rather than listed.
            let mut values: Vec<String> = entries.values(key).filter_map(|v| v.as_scalar()).map(str::to_owned).collect();
            values.extend(entries.scalar_items(key));
            values
        };

        let archive = owned("archive");
        Ok(Self {
            name: owned("name").unwrap_or_default(),
            is_archive: entries.scalar("path").is_none() && archive.is_some(),
            path: owned("path").or(archive),
            version: owned("version").unwrap_or_default(),
            supported_version: owned("supported_version").unwrap_or_default(),
            tags: entries.scalar_items("tags"),
            picture: owned("picture"),
            remote_file_id: entries.scalar("remote_file_id").and_then(|s| s.trim().parse().ok()),
            dependencies: entries.scalar_items("dependencies"),
            replace_path: list("replace_path"),
            user_dir: list("user_dir"),
        })
    }

    /// The game version this mod targets, as shown to the user.
    pub fn game_version(&self) -> &str {
        if self.supported_version.is_empty() { &self.version } else { &self.supported_version }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_workshop_descriptor() {
        let d = Descriptor::parse(
            "version=\"1.0\"\ntags={\n\t\"Graphics\"\n}\nname=\"! IBS Animated Map Icons\"\npicture=\"thumbnail.png\"\nsupported_version=\"*\"\npath=\"/steam/workshop/content/281990/2633444661\"\nremote_file_id=\"2633444661\"",
        )
        .unwrap();
        assert_eq!(d.name, "! IBS Animated Map Icons");
        assert_eq!(d.game_version(), "*");
        assert_eq!(d.tags, ["Graphics"]);
        assert_eq!(d.remote_file_id, Some(2633444661));
        assert!(!d.is_archive);
    }

    #[test]
    fn handles_archive_and_replace_paths() {
        let d = Descriptor::parse("name=\"Old\" archive=\"mod/old.zip\" version=\"2.8\" replace_path=\"common/ethics\" replace_path=\"events\" dependencies={ \"Base\" }").unwrap();
        assert!(d.is_archive);
        assert_eq!(d.path.as_deref(), Some("mod/old.zip"));
        assert_eq!(d.game_version(), "2.8");
        assert_eq!(d.replace_path, ["common/ethics", "events"]);
        assert_eq!(d.dependencies, ["Base"]);
    }
}
