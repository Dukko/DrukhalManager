//! Line and word diffs between two versions of a text, for the side-by-side
//! conflict view.
//!
//! Lines are matched with Myers' algorithm. Runs of removed and added lines
//! between matches are paired up as changed lines, so the two sides can be
//! shown aligned row by row, with filler rows where one side has no line.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Same,
    /// Both sides have a line here, and they differ.
    Changed,
    /// Only the left side has a line here.
    Removed,
    /// Only the right side has a line here.
    Added,
}

/// One aligned row: a line index on each side, or `None` for a filler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub left: Option<usize>,
    pub right: Option<usize>,
    pub kind: Kind,
}

/// A run of differing rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub rows: Range<usize>,
    /// The lines it covers on each side (empty where a side has none).
    pub left: Range<usize>,
    pub right: Range<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub rows: Vec<Row>,
    pub hunks: Vec<Hunk>,
}

/// Past this many differences the line diff gives up matching and marks
/// the rest as changed, to bound time and memory on unrelated texts. The
/// search keeps about `MAX_EDITS²` numbers (32 MB here) and runs on the
/// main thread.
const MAX_EDITS: usize = 2000;

/// Diffs two texts line by line. With `ignore_whitespace`, lines that only
/// differ in spacing or indentation count as the same.
pub fn lines(left: &str, right: &str, ignore_whitespace: bool) -> Diff {
    let a: Vec<&str> = left.lines().collect();
    let b: Vec<&str> = right.lines().collect();
    let norm = |l: &&str| if ignore_whitespace { l.split_whitespace().collect::<Vec<_>>().join(" ") } else { (*l).to_owned() };
    let (na, nb): (Vec<String>, Vec<String>) = (a.iter().map(norm).collect(), b.iter().map(norm).collect());
    let ops = edits(&na, &nb, MAX_EDITS);
    align(&ops, a.len(), b.len())
}

/// One step of an edit script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Keep,
    Delete,
    Insert,
}

/// The shortest edit script turning `a` into `b` (Myers, 1986), or a
/// delete-everything-then-insert script past `max` edits.
fn edits<T: PartialEq>(a: &[T], b: &[T], max: usize) -> Vec<Op> {
    // Common prefix and suffix are cheap to strip and common in practice.
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..].iter().rev().zip(b[prefix..].iter().rev()).take_while(|(x, y)| x == y).count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
    let mut ops = vec![Op::Keep; prefix];
    ops.extend(middle(a_mid, b_mid, max));
    ops.extend(std::iter::repeat_n(Op::Keep, suffix));
    ops
}

fn middle<T: PartialEq>(a: &[T], b: &[T], max: usize) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    if n == 0 || m == 0 {
        let mut ops = vec![Op::Delete; a.len()];
        ops.extend(std::iter::repeat_n(Op::Insert, b.len()));
        return ops;
    }
    let limit = ((n + m) as usize).min(max) as isize;
    let offset = limit + 1;
    let mut v = vec![0isize; 2 * offset as usize + 1];
    // The diagonals -d..=d of v as each round starts, to walk back the
    // path; the rest of v is unused then, and copying it all made the
    // memory grow with the texts' length times the rounds.
    let mut trace: Vec<Vec<isize>> = Vec::new();
    let mut found = None;
    'search: for d in 0..=limit {
        trace.push(v[(offset - d) as usize..=(offset + d) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let i = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[i - 1] < v[i + 1]) { v[i + 1] } else { v[i - 1] + 1 };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[i] = x;
            if x >= n && y >= m {
                found = Some(d);
                break 'search;
            }
            k += 2;
        }
    }
    let Some(d_end) = found else {
        let mut ops = vec![Op::Delete; a.len()];
        ops.extend(std::iter::repeat_n(Op::Insert, b.len()));
        return ops;
    };
    // Backtrack from (n, m).
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (1..=d_end).rev() {
        // Diagonal k of round d is at k + d in its window.
        let window = &trace[d as usize];
        let at = |k: isize| window[(k + d) as usize];
        let k = x - y;
        let prev_k = if k == -d || (k != d && at(k - 1) < at(k + 1)) { k + 1 } else { k - 1 };
        let prev_x = at(prev_k);
        let prev_y = prev_x - prev_k;
        while x > prev_x && y > prev_y {
            ops.push(Op::Keep);
            x -= 1;
            y -= 1;
        }
        ops.push(if x == prev_x { Op::Insert } else { Op::Delete });
        (x, y) = (prev_x, prev_y);
    }
    while x > 0 && y > 0 {
        ops.push(Op::Keep);
        x -= 1;
        y -= 1;
    }
    ops.reverse();
    ops
}

/// Turns an edit script into aligned rows and hunks.
fn align(ops: &[Op], left_len: usize, right_len: usize) -> Diff {
    let mut diff = Diff::default();
    let (mut i, mut j, mut at) = (0, 0, 0);
    while at < ops.len() {
        if ops[at] == Op::Keep {
            diff.rows.push(Row { left: Some(i), right: Some(j), kind: Kind::Same });
            (i, j, at) = (i + 1, j + 1, at + 1);
            continue;
        }
        // A run of deletes and inserts, in any order, until the next keep.
        let end = ops[at..].iter().position(|&o| o == Op::Keep).map_or(ops.len(), |p| at + p);
        let deletes = ops[at..end].iter().filter(|&&o| o == Op::Delete).count();
        let inserts = end - at - deletes;
        let first_row = diff.rows.len();
        for r in 0..deletes.max(inserts) {
            let (l, rt) = ((r < deletes).then_some(i + r), (r < inserts).then_some(j + r));
            let kind = match (l, rt) {
                (Some(_), Some(_)) => Kind::Changed,
                (Some(_), None) => Kind::Removed,
                _ => Kind::Added,
            };
            diff.rows.push(Row { left: l, right: rt, kind });
        }
        diff.hunks.push(Hunk { rows: first_row..diff.rows.len(), left: i..i + deletes, right: j..j + inserts });
        (i, j, at) = (i + deletes, j + inserts, end);
    }
    debug_assert_eq!((i, j), (left_len, right_len));
    diff
}

/// The byte ranges of `left` and `right` that differ, word by word. Words
/// are runs of letters, digits and `_`, runs of spaces, or single symbols.
pub fn words(left: &str, right: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let (a, b) = (tokens(left), tokens(right));
    let ta: Vec<&str> = a.iter().map(|r| &left[r.clone()]).collect();
    let tb: Vec<&str> = b.iter().map(|r| &right[r.clone()]).collect();
    let ops = edits(&ta, &tb, 500);
    let (mut out_a, mut out_b) = (Vec::new(), Vec::new());
    let (mut i, mut j) = (0, 0);
    for op in ops {
        match op {
            Op::Keep => (i, j) = (i + 1, j + 1),
            Op::Delete => {
                push_range(&mut out_a, a[i].clone());
                i += 1;
            }
            Op::Insert => {
                push_range(&mut out_b, b[j].clone());
                j += 1;
            }
        }
    }
    (out_a, out_b)
}

/// Appends `r`, joining it to the last range when they touch.
fn push_range(out: &mut Vec<Range<usize>>, r: Range<usize>) {
    match out.last_mut() {
        Some(last) if last.end == r.start => last.end = r.end,
        _ => out.push(r),
    }
}

fn tokens(s: &str) -> Vec<Range<usize>> {
    let class = |c: char| if c.is_alphanumeric() || c == '_' { 0 } else if c.is_whitespace() { 1 } else { 2 };
    let mut out: Vec<Range<usize>> = Vec::new();
    let mut last_class = None;
    for (at, c) in s.char_indices() {
        let k = class(c);
        match out.last_mut() {
            Some(r) if last_class == Some(k) && k != 2 => r.end = at + c.len_utf8(),
            _ => out.push(at..at + c.len_utf8()),
        }
        last_class = Some(k);
    }
    out
}

/// `left` with the lines of `hunk` replaced by the right side's lines.
pub fn take(left: &str, right: &str, hunk: &Hunk) -> String {
    let a: Vec<&str> = left.lines().collect();
    let b: Vec<&str> = right.lines().collect();
    let mut out: Vec<&str> = a[..hunk.left.start].to_vec();
    out.extend_from_slice(&b[hunk.right.clone()]);
    out.extend_from_slice(&a[hunk.left.end..]);
    let mut text = out.join("\n");
    if left.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sides(d: &Diff) -> Vec<(Option<usize>, Option<usize>, Kind)> {
        d.rows.iter().map(|r| (r.left, r.right, r.kind)).collect()
    }

    #[test]
    fn identical_texts_have_no_hunks() {
        let d = lines("a\nb\nc", "a\nb\nc", false);
        assert!(d.hunks.is_empty());
        assert_eq!(d.rows.len(), 3);
    }

    #[test]
    fn aligns_changes_additions_and_removals() {
        let left = "a = {\n\tcost = 100\n\tminerals = 0\n}";
        let right = "a = {\n\tsize > 5\n\tcost = 150\n}";
        let d = lines(left, right, false);
        // Row by row, every line of each side appears once, in order.
        let l: Vec<usize> = d.rows.iter().filter_map(|r| r.left).collect();
        let r: Vec<usize> = d.rows.iter().filter_map(|r| r.right).collect();
        assert_eq!(l, [0, 1, 2, 3]);
        assert_eq!(r, [0, 1, 2, 3]);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].left, 1..3);
        assert_eq!(d.hunks[0].right, 1..3);
        assert!(d.rows[1..3].iter().all(|r| r.kind == Kind::Changed));
    }

    #[test]
    fn pure_insert_gets_a_filler_on_the_left() {
        let d = lines("a\nc", "a\nb\nc", false);
        assert_eq!(sides(&d), [(Some(0), Some(0), Kind::Same), (None, Some(1), Kind::Added), (Some(1), Some(2), Kind::Same)]);
        assert_eq!(d.hunks[0].left, 1..1);
        assert_eq!(d.hunks[0].right, 1..2);
    }

    #[test]
    fn whitespace_can_be_ignored() {
        assert_eq!(lines("a = 1\n  b", "a  =  1\n\tb", true).hunks.len(), 0);
        assert_eq!(lines("a = 1", "a  =  1", false).hunks.len(), 1);
    }

    #[test]
    fn handles_empty_sides() {
        assert_eq!(sides(&lines("", "x", false)), [(None, Some(0), Kind::Added)]);
        assert_eq!(sides(&lines("x\ny", "", false)), [(Some(0), None, Kind::Removed), (Some(1), None, Kind::Removed)]);
    }

    #[test]
    fn finds_a_minimal_diff() {
        // The classic example from Myers' paper: 5 edits.
        let a: Vec<char> = "ABCABBA".chars().collect();
        let b: Vec<char> = "CBABAC".chars().collect();
        let ops = edits(&a, &b, 100);
        assert_eq!(ops.iter().filter(|&&o| o != Op::Keep).count(), 5);
    }

    #[test]
    fn stays_minimal_on_longer_texts() {
        // Every 7th line changed in a long file: the diff finds exactly those.
        let a: Vec<String> = (0..3000).map(|i| format!("line {i}")).collect();
        let b: Vec<String> = (0..3000).map(|i| if i % 7 == 0 { format!("changed {i}") } else { format!("line {i}") }).collect();
        let ops = edits(&a, &b, MAX_EDITS);
        let changed = (0..3000).filter(|i| i % 7 == 0).count();
        assert_eq!(ops.iter().filter(|&&o| o == Op::Delete).count(), changed);
        assert_eq!(ops.iter().filter(|&&o| o == Op::Insert).count(), changed);
    }

    #[test]
    fn unrelated_big_texts_finish_quickly() {
        let a: Vec<String> = (0..20_000).map(|i| format!("a {i}")).collect();
        let b: Vec<String> = (0..20_000).map(|i| format!("b {i}")).collect();
        let start = std::time::Instant::now();
        let ops = edits(&a, &b, MAX_EDITS);
        assert_eq!(ops.len(), 40_000);
        assert!(start.elapsed() < std::time::Duration::from_secs(10), "took {:?}", start.elapsed());
    }

    #[test]
    fn gives_up_past_the_edit_limit() {
        let a: Vec<u32> = (0..50).collect();
        let b: Vec<u32> = (100..150).collect();
        let ops = edits(&a, &b, 10);
        assert_eq!(ops.iter().filter(|&&o| o == Op::Delete).count(), 50);
        assert_eq!(ops.iter().filter(|&&o| o == Op::Insert).count(), 50);
    }

    #[test]
    fn word_diff_marks_only_the_changed_words() {
        let (a, b) = words("\tenergy = 100", "\tenergy = 150");
        assert_eq!(a, vec![10..13]);
        assert_eq!(b, vec![10..13]);
        let (a, b) = words("x = { a }", "x = { a b }");
        assert!(a.is_empty());
        assert_eq!(b.len(), 1);
        assert_eq!("x = { a b }"[b[0].clone()].trim(), "b");
    }

    #[test]
    fn takes_a_hunk_from_the_right() {
        let left = "a\nb\nc\n";
        let right = "a\nB\nB2\nc\n";
        let d = lines(left, right, false);
        assert_eq!(take(left, right, &d.hunks[0]), "a\nB\nB2\nc\n");
        // Taking an insertion adds lines without removing any.
        let d = lines("a\nc", "a\nb\nc", false);
        assert_eq!(take("a\nc", "a\nb\nc", &d.hunks[0]), "a\nb\nc");
    }
}
