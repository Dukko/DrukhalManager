//! The shipped notes on what recent Stellaris versions renamed or removed,
//! written for the AI prompts and read here too, so the error report can tell
//! a harmless leftover from a real problem.

/// The notes file, as written.
pub const NOTES: &str = include_str!("../merge/ai_notes.md");

/// One entry of the shipped notes on game changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub old: String,
    /// The names that replace `old`; empty when it was removed.
    pub new: Vec<String>,
    /// What the note says about the change, after the colon.
    pub why: String,
}

/// The shipped notes, parsed.
pub fn notes() -> Vec<Note> {
    let ticked = |s: &str| -> Vec<String> { s.split('`').skip(1).step_by(2).map(str::to_owned).collect() };
    NOTES
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- "))
        .filter_map(|line| {
            let (old, rest) = line.split_once('→')?;
            let old = ticked(old).into_iter().next()?;
            let (names, why) = rest.split_once(':').unwrap_or((rest, ""));
            Some(Note { old, new: ticked(names), why: why.trim().to_owned() })
        })
        .collect()
}

/// What the notes say about `keyword`, if the game removed it with nothing
/// to replace it.
pub fn removed(keyword: &str) -> Option<String> {
    notes().into_iter().find(|n| n.old == keyword && n.new.is_empty()).map(|n| n.why).filter(|w| !w.is_empty())
}
