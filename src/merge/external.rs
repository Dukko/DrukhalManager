//! Merging in an external tool, like Irony Mod Manager's External Merge:
//! the two versions and the base game's are written to files, the tool
//! the user chose (Meld, KDiff3, VS Code…) is started on them, and what it
//! saves is read back.
//!
//! The command is split like a shell would and may use `{left}`,
//! `{right}`, `{base}` and `{output}`. In the Flatpak, the tool runs on the
//! host through `flatpak-spawn --host`, which needs a permission the app
//! doesn't ask for by default (see [`FLATPAK_OVERRIDE`]).

use std::path::{Path, PathBuf};

/// Commands for tools commonly installed, by name.
pub const PRESETS: &[(&str, &str)] = &[
    ("Meld", "meld --auto-merge {left} {base} {right} --output {output}"),
    ("KDiff3", "kdiff3 {base} {left} {right} --output {output}"),
    ("Visual Studio Code", "code --wait --merge {left} {right} {base} {output}"),
];

/// Lets the Flatpak start programs on the host.
pub const FLATPAK_OVERRIDE: &str = "flatpak override --user --talk-name=org.freedesktop.Flatpak io.github.dukko.DrukhalManager";

/// The first preset whose program is installed, outside the Flatpak (where
/// the host's programs can't be seen).
pub fn detect() -> Option<&'static str> {
    if crate::platform::is_flatpak() {
        return None;
    }
    PRESETS.iter().find(|(_, command)| command.split(' ').next().and_then(crate::platform::find_program).is_some()).map(|(_, c)| *c)
}

/// One side of the merge.
pub struct Side<'a> {
    pub label: &'a str,
    pub text: &'a str,
}

/// Runs `command` on `left` and `right` (and the base game's version, if
/// any) and waits for it to exit. Returns the merged text, or `None` if the
/// tool was closed without saving. Blocks; call off the main thread.
pub fn run(command: &str, left: Side, right: Side, base: Option<&str>) -> Result<Option<String>, String> {
    let dir = crate::platform::cache_dir().join("merge-tool").join(format!("{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())));
    let result = run_in(&dir, command, left, right, base);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_in(dir: &Path, command: &str, left: Side, right: Side, base: Option<&str>) -> Result<Option<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Could not write the versions to merge: {e}"))?;
    let write = |name: String, text: &str| -> Result<PathBuf, String> {
        let path = dir.join(name);
        std::fs::write(&path, text).map_err(|e| format!("Could not write the versions to merge: {e}"))?;
        Ok(path)
    };
    // Named after where they're from, which the tool shows.
    let left_path = write(format!("1 {}.txt", file_safe(left.label)), left.text)?;
    let right_path = write(format!("2 {}.txt", file_safe(right.label)), right.text)?;
    let base_path = write("0 Base game.txt".into(), base.unwrap_or_default())?;
    // Starts as the left version, so tools that edit it in place have it.
    let output_path = write("Merged.txt".into(), left.text)?;
    let before = std::fs::metadata(&output_path).and_then(|m| m.modified()).ok();

    let args = arguments(command, &left_path, &right_path, &base_path, &output_path)?;
    let mut process = if crate::platform::is_flatpak() {
        let mut p = std::process::Command::new("flatpak-spawn");
        p.arg("--host").args(&args);
        p
    } else {
        let mut p = std::process::Command::new(&args[0]);
        p.args(&args[1..]);
        p
    };
    let output = process.stdin(std::process::Stdio::null()).output().map_err(|e| format!("Could not start {}: {e}", args[0]))?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    if crate::platform::is_flatpak() && !output.status.success() && (stderr.contains("org.freedesktop.Flatpak") || stderr.contains("ServiceUnknown") || stderr.contains("AccessDenied")) {
        return Err(format!("The Flatpak isn't allowed to start programs outside it. To use a merge tool, run this once, then restart DrukhalManager: {FLATPAK_OVERRIDE}"));
    }
    // Exit codes say little: Meld always succeeds, KDiff3 fails when not
    // saved. Whether the output changed says what happened.
    let after = std::fs::metadata(&output_path).and_then(|m| m.modified()).ok();
    let merged = std::fs::read_to_string(&output_path).map_err(|e| format!("Could not read the merged version: {e}"))?;
    if after == before && merged == left.text {
        if !output.status.success() && !stderr.trim().is_empty() {
            return Err(format!("{} failed: {}", args[0], stderr.trim()));
        }
        return Ok(None);
    }
    Ok(Some(merged))
}

/// The command's words, with the placeholders filled in.
fn arguments(command: &str, left: &Path, right: &Path, base: &Path, output: &Path) -> Result<Vec<String>, String> {
    let words = shlex::split(command.trim()).ok_or(t!("The merge tool command can't be read: a quote isn't closed"))?;
    let args: Vec<String> = words
        .iter()
        .map(|w| {
            w.replace("{left}", &left.to_string_lossy())
                .replace("{right}", &right.to_string_lossy())
                .replace("{base}", &base.to_string_lossy())
                .replace("{output}", &output.to_string_lossy())
        })
        .collect();
    if args.is_empty() {
        return Err(t!("No merge tool is set"));
    }
    Ok(args)
}

fn file_safe(label: &str) -> String {
    let s: String = label.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).take(60).collect();
    s.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_in_placeholders() {
        let p = |s: &str| PathBuf::from(format!("/t/{s}"));
        let args = arguments(PRESETS[0].1, &p("l"), &p("r"), &p("b"), &p("o")).unwrap();
        assert_eq!(args, ["meld", "--auto-merge", "/t/l", "/t/b", "/t/r", "--output", "/t/o"]);
        let args = arguments("'my tool' --out={output} {left}", &p("l"), &p("r"), &p("b"), &p("o")).unwrap();
        assert_eq!(args, ["my tool", "--out=/t/o", "/t/l"]);
        assert!(arguments("  ", &p("l"), &p("r"), &p("b"), &p("o")).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn reads_what_the_tool_saved() {
        if crate::platform::is_flatpak() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("drukhal-external-{}", std::process::id()));
        let side = |label, text| Side { label, text };
        // A "tool" that writes both versions into the output.
        let merged = run_in(&dir.join("a"), "sh -c 'cat \"$1\" \"$2\" > \"$3\"' sh {left} {right} {output}", side("A", "a\n"), side("B/x", "b\n"), None).unwrap();
        assert_eq!(merged.as_deref(), Some("a\nb\n"));
        // Closed without saving.
        assert_eq!(run_in(&dir.join("b"), "true {output}", side("A", "a\n"), side("B", "b\n"), Some("base")).unwrap(), None);
        assert!(run_in(&dir.join("c"), "drukhal-no-such-tool {output}", side("A", "a"), side("B", "b"), None).unwrap_err().contains("Could not start"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
