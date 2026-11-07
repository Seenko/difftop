//! Turns two versions of a file into the lines worth showing.

use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Context,
    Added,
    Removed,
}

/// One line of a hunk. `index` is a 0-based line number into the *old* text
/// for [`Kind::Removed`] and into the *new* text otherwise, so the caller can
/// fetch the syntax-highlighted version of that line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: Kind,
    pub index: usize,
}

/// Groups of nearby changes, each padded with up to `context` unchanged lines.
pub fn hunks(old: &str, new: &str, context: usize) -> Vec<Vec<DiffLine>> {
    // Lines end at `\n` only, exactly as the highlighter splits them, so a
    // line's index finds the same line there. `from_lines` also ends lines
    // at a lone `\r` (progress bars in logs), which made the indexes run
    // past the highlighted lines and crashed the renderer.
    let (old, new): (Vec<&str>, Vec<&str>) = (
        old.split_inclusive('\n').collect(),
        new.split_inclusive('\n').collect(),
    );
    let diff = TextDiff::from_slices(&old, &new);
    diff.grouped_ops(context)
        .iter()
        .map(|group| {
            group
                .iter()
                .flat_map(|op| diff.iter_changes(op))
                .map(|change| match change.tag() {
                    ChangeTag::Equal => DiffLine {
                        kind: Kind::Context,
                        index: change.new_index().unwrap_or_default(),
                    },
                    ChangeTag::Insert => DiffLine {
                        kind: Kind::Added,
                        index: change.new_index().unwrap_or_default(),
                    },
                    ChangeTag::Delete => DiffLine {
                        kind: Kind::Removed,
                        index: change.old_index().unwrap_or_default(),
                    },
                })
                .collect()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use Kind::*;

    fn kinds(hunk: &[DiffLine]) -> Vec<(Kind, usize)> {
        hunk.iter().map(|l| (l.kind, l.index)).collect()
    }

    #[test]
    fn single_edit_with_context() {
        let old = "a\nb\nc\nd\ne\n";
        let new = "a\nb\nC\nd\ne\n";
        let h = hunks(old, new, 1);
        assert_eq!(h.len(), 1);
        assert_eq!(
            kinds(&h[0]),
            [(Context, 1), (Removed, 2), (Added, 2), (Context, 3)]
        );
    }

    #[test]
    fn distant_edits_become_separate_hunks() {
        let old: String = (0..20).map(|i| format!("{i}\n")).collect();
        let new = old.replace("2\n", "two\n").replace("17\n", "seventeen\n");
        assert_eq!(hunks(&old, &new, 2).len(), 2);
    }

    #[test]
    fn lone_carriage_returns_stay_inside_their_line() {
        // One line each side, the same count the highlighter sees.
        let h = hunks("a\rb\n", "a\rb\rc\n", 0);
        assert_eq!(kinds(&h[0]), [(Removed, 0), (Added, 0)]);
    }

    #[test]
    fn identical_and_created() {
        assert!(hunks("same\n", "same\n", 3).is_empty());
        let h = hunks("", "x\ny\n", 3);
        assert_eq!(kinds(&h[0]), [(Added, 0), (Added, 1)]);
    }
}
