//! Turns two versions of a file into the lines worth showing.

use std::ops::Range;

use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

/// Byte ranges of changed words in a line.
pub type Words = Vec<Range<usize>>;

/// Byte ranges of the words that differ between two versions of a line,
/// in the old and in the new line. `None` when the lines differ in more
/// than [`MAX_WORD_CHANGE`] of their text: then they are different lines,
/// not an edit, and marking words would only be noise. This is Claude
/// Code's rule.
pub fn changed_words(old: &str, new: &str) -> Option<(Words, Words)> {
    let (old_words, new_words) = (words(old), words(new));
    let diff = TextDiff::from_slices(&old_words, &new_words);
    let (mut gone, mut came) = (Vec::new(), Vec::new());
    // Where each word starts, in bytes, plus the end.
    let starts = |words: &[&str]| -> Vec<usize> {
        std::iter::once(0)
            .chain(words.iter().scan(0, |at, w| {
                *at += w.len();
                Some(*at)
            }))
            .collect()
    };
    let (old_at, new_at) = (starts(&old_words), starts(&new_words));
    for op in diff.ops() {
        if op.tag() == similar::DiffTag::Equal {
            continue;
        }
        let (o, n) = (op.old_range(), op.new_range());
        if !o.is_empty() {
            gone.push(old_at[o.start]..old_at[o.end]);
        }
        if !n.is_empty() {
            came.push(new_at[n.start]..new_at[n.end]);
        }
    }
    let changed: usize = gone.iter().chain(&came).map(|r| r.len()).sum();
    let total = old.len() + new.len();
    let share = changed as f64 / total.max(1) as f64;
    (changed > 0 && share <= MAX_WORD_CHANGE).then_some((gone, came))
}

/// Beyond this share of changed text, a line pair isn't marked word by word.
const MAX_WORD_CHANGE: f64 = 0.4;

/// Words (letters, digits, `_`), runs of whitespace, and single other
/// characters, so `foo(bar)` changes one word when `bar` does.
fn words(line: &str) -> Vec<&str> {
    let kind = |c: char| match c {
        c if c.is_alphanumeric() || c == '_' => 0,
        c if c.is_whitespace() => 1,
        _ => 2,
    };
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = line.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let k = kind(c);
        let joins = |next: char| k != 2 && kind(next) == k;
        if chars.peek().is_some_and(|&(_, next)| joins(next)) {
            continue;
        }
        out.push(&line[start..i + c.len_utf8()]);
        start = i + c.len_utf8();
    }
    out
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
    fn changed_words_mark_only_what_changed() {
        let (gone, came) = changed_words("let x = foo(bar);", "let x = foo(baz);").unwrap();
        assert_eq!((gone.len(), came.len()), (1, 1));
        assert_eq!((gone[0].clone(), came[0].clone()), (12..15, 12..15));
        // Most of the line changed: no word marks.
        assert_eq!(changed_words("let x = 1;", "fn main() {}"), None);
        assert_eq!(changed_words("same", "same"), None);
        assert_eq!(words("a_b  c(d)"), ["a_b", "  ", "c", "(", "d", ")"]);
    }

    #[test]
    fn identical_and_created() {
        assert!(hunks("same\n", "same\n", 3).is_empty());
        let h = hunks("", "x\ny\n", 3);
        assert_eq!(kinds(&h[0]), [(Added, 0), (Added, 1)]);
    }
}
