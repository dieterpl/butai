//! A unified diff, as data — parse it, take a piece of it, turn it round.
//!
//! This is what partial staging is actually made of. `git add -p` looks like a
//! UI feature and is really a text transformation: to stage half a hunk you
//! build a *new, valid* patch containing only the lines you chose, and apply
//! that. Get the `@@` arithmetic wrong and the apply is rejected — or worse,
//! accepted against the wrong lines.
//!
//! It lives in the protocol crate because the patch text *is* the wire format
//! for staging: a client `GET`s a diff, takes a piece of it, and `POST`s the
//! piece back to `git/apply` (see [`api::ApplyTarget`]). Both ends therefore
//! need the same reading of what a hunk is, and a copy on each side is two
//! things that drift — the `@@` arithmetic being exactly the part that fails
//! silently when they disagree.
//!
//! There is no I/O here and no `git2` type in any signature: the whole surface
//! is `&str` in, `String` out, so the risky part is unit-tested without a
//! repository and the crate stays dependency-free.
//!
//! Two transformations carry everything:
//!
//! - [`Patch::subset`] — the selected hunks, or selected *lines* within a hunk,
//!   as a standalone patch. An unselected `+` line is dropped; an unselected
//!   `-` line becomes context, because the file it is being applied to still
//!   contains it. That asymmetry is the whole trick, and it is why the counts
//!   have to be recomputed rather than copied.
//! - [`Patch::reversed`] — the same patch backwards, which is how unstaging and
//!   discarding work. libgit2's apply has no reverse flag, and doing it on the
//!   model rather than on the text means the `@@` header can never disagree
//!   with the body.

/// What a line does to the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Context,
    Added,
    Removed,
    /// `\ No newline at end of file`. Not a line of the file — an annotation on
    /// the one before it, and it has to travel with it or the patch is invalid.
    NoNewline,
}

impl Origin {
    fn marker(self) -> char {
        match self {
            Origin::Context => ' ',
            Origin::Added => '+',
            Origin::Removed => '-',
            Origin::NoNewline => '\\',
        }
    }

    /// Whether this line exists in the file the patch applies *to*.
    fn in_old(self) -> bool {
        matches!(self, Origin::Context | Origin::Removed)
    }

    /// Whether this line exists in the file the patch produces.
    fn in_new(self) -> bool {
        matches!(self, Origin::Context | Origin::Added)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub origin: Origin,
    /// The text after the marker, with no trailing newline.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub new_start: usize,
    /// Whatever followed the closing `@@` — usually the enclosing function.
    /// Carried through because it is the only orientation a long diff gives.
    pub section: String,
    pub lines: Vec<Line>,
}

impl Hunk {
    fn old_len(&self) -> usize {
        self.lines.iter().filter(|l| l.origin.in_old()).count()
    }

    fn new_len(&self) -> usize {
        self.lines.iter().filter(|l| l.origin.in_new()).count()
    }

    /// Indices into `lines` that a selection can name — the `+` and `-` lines.
    /// Context is never selectable: it is not a change, it is the thing the
    /// change is anchored to.
    pub fn changed_line_indices(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, l)| matches!(l.origin, Origin::Added | Origin::Removed))
            .map(|(i, _)| i)
            .collect()
    }

    /// Render one `@@` header. The lengths are always recomputed from the body
    /// rather than remembered, so a subset cannot disagree with its own header.
    fn header(&self, new_start: usize) -> String {
        let (ol, nl) = (self.old_len(), self.new_len());
        // git omits the count when it is 1, and points at the line *before* an
        // insertion when the old side is empty. Both are load-bearing: `git
        // apply` accepts the long form, but the round-trip test compares text.
        let old = if ol == 1 {
            format!("{}", self.old_start)
        } else {
            format!("{},{ol}", self.old_start)
        };
        let new = if nl == 1 { format!("{new_start}") } else { format!("{new_start},{nl}") };
        let section =
            if self.section.is_empty() { String::new() } else { format!(" {}", self.section) };
        format!("@@ -{old} +{new} @@{section}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePatch {
    /// Everything before the first `@@`: `diff --git`, `index`, mode lines,
    /// `---`/`+++`. Kept verbatim — rewriting it is how a rename or a mode
    /// change gets silently dropped.
    pub header: Vec<String>,
    /// `a/…` and `b/…` paths with their prefix stripped, for display and for
    /// naming what a selection is about.
    pub old_path: String,
    pub new_path: String,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Patch {
    pub files: Vec<FilePatch>,
}

/// Which hunk, and optionally which lines inside it, an operation is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub file: usize,
    pub hunk: usize,
    /// Indices into the hunk's `lines`. `None` means the whole hunk — not the
    /// same as an empty list, which selects nothing and yields an empty patch.
    pub lines: Option<Vec<usize>>,
}

impl Patch {
    /// Parse unified diff text. Anything before the first `diff --git` is
    /// ignored, and a malformed body yields the files parsed so far rather than
    /// an error: this also parses patches typed by an API caller, and half a
    /// diff should fail at `apply` with git's own message, not here with ours.
    pub fn parse(text: &str) -> Patch {
        let mut files: Vec<FilePatch> = Vec::new();
        for raw in text.lines() {
            if raw.starts_with("diff --git ") {
                files.push(FilePatch {
                    header: vec![raw.to_string()],
                    old_path: String::new(),
                    new_path: String::new(),
                    hunks: Vec::new(),
                });
                continue;
            }
            let Some(file) = files.last_mut() else { continue };
            if let Some(rest) = raw.strip_prefix("@@ ") {
                if let Some(hunk) = parse_hunk_header(rest) {
                    file.hunks.push(hunk);
                }
                continue;
            }
            if file.hunks.is_empty() {
                if let Some(p) = raw.strip_prefix("--- ") {
                    file.old_path = strip_prefix_dir(p);
                } else if let Some(p) = raw.strip_prefix("+++ ") {
                    file.new_path = strip_prefix_dir(p);
                }
                file.header.push(raw.to_string());
                continue;
            }
            let hunk = file.hunks.last_mut().expect("checked non-empty");
            let (origin, text) = match raw.chars().next() {
                Some(' ') => (Origin::Context, &raw[1..]),
                Some('+') => (Origin::Added, &raw[1..]),
                Some('-') => (Origin::Removed, &raw[1..]),
                Some('\\') => (Origin::NoNewline, &raw[1..]),
                // An empty line inside a hunk is a context line whose trailing
                // space some tool ate. Treating it as the end of the hunk
                // truncates the patch; treating it as context is what `git
                // apply` itself does.
                None => (Origin::Context, ""),
                _ => continue,
            };
            hunk.lines.push(Line { origin, text: text.to_string() });
        }
        Patch { files }
    }

    /// The whole patch as text, with every `@@` recomputed.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            write_file(&mut out, file, |_, _| true);
        }
        out
    }

    /// A standalone patch containing only what `sel` names.
    ///
    /// The rules that make a partial hunk valid, in one place:
    ///
    /// - a **selected** `+` or `-` stays as it is;
    /// - an **unselected `+`** is dropped — it is not going into the index, and
    ///   it is not in the file being applied to either;
    /// - an **unselected `-`** becomes context — it *is* in the file being
    ///   applied to, so it has to be matched, just not removed;
    /// - context is always kept.
    ///
    /// Returns `None` when the selection names nothing that exists, or selects
    /// no actual change (an all-context patch would apply cleanly and do
    /// nothing, which reads to the caller as a silent success).
    pub fn subset(&self, sel: &Selection) -> Option<String> {
        let file = self.files.get(sel.file)?;
        let hunk = file.hunks.get(sel.hunk)?;
        let picked = sel.lines.clone();
        let keep = |i: usize| picked.as_ref().is_none_or(|p| p.contains(&i));

        let mut lines: Vec<Line> = Vec::new();
        for (i, line) in hunk.lines.iter().enumerate() {
            match line.origin {
                Origin::Context => lines.push(line.clone()),
                Origin::Added if keep(i) => lines.push(line.clone()),
                Origin::Added => {}
                Origin::Removed if keep(i) => lines.push(line.clone()),
                Origin::Removed => {
                    lines.push(Line { origin: Origin::Context, text: line.text.clone() })
                }
                // Follows the line it annotates, so it survives exactly when
                // that line did. Dropping an added line takes its marker with
                // it; a `-` demoted to context keeps one, because the file
                // being applied to still ends without a newline.
                Origin::NoNewline => {
                    let kept_previous = lines.len() == i
                        || lines.last().is_some_and(|l| {
                            hunk.lines.get(i.saturating_sub(1)).is_some_and(|p| p.text == l.text)
                        });
                    if kept_previous {
                        lines.push(line.clone());
                    }
                }
            }
        }
        if !lines.iter().any(|l| matches!(l.origin, Origin::Added | Origin::Removed)) {
            return None;
        }

        let subset = Hunk {
            old_start: hunk.old_start,
            new_start: hunk.new_start,
            section: hunk.section.clone(),
            lines,
        };
        let one = FilePatch {
            header: file.header.clone(),
            old_path: file.old_path.clone(),
            new_path: file.new_path.clone(),
            hunks: vec![subset],
        };
        let mut out = String::new();
        write_file(&mut out, &one, |_, _| true);
        Some(out)
    }

    /// The patch backwards: applying this undoes applying the original.
    ///
    /// Adds and removes swap, and so do the two sides of every `@@` and of the
    /// `---`/`+++` pair. Used for unstaging (reverse-apply to the index) and
    /// discarding (reverse-apply to the worktree).
    pub fn reversed(&self) -> Patch {
        Patch {
            files: self
                .files
                .iter()
                .map(|f| FilePatch {
                    header: reverse_header(&f.header, &f.new_path, &f.old_path),
                    old_path: f.new_path.clone(),
                    new_path: f.old_path.clone(),
                    hunks: f
                        .hunks
                        .iter()
                        .map(|h| Hunk {
                            old_start: h.new_start,
                            new_start: h.old_start,
                            section: h.section.clone(),
                            lines: h
                                .lines
                                .iter()
                                .map(|l| Line {
                                    origin: match l.origin {
                                        Origin::Added => Origin::Removed,
                                        Origin::Removed => Origin::Added,
                                        other => other,
                                    },
                                    text: l.text.clone(),
                                })
                                .collect(),
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    /// Total hunks across every file — what a "hunk 2/6" counter reports.
    pub fn hunk_count(&self) -> usize {
        self.files.iter().map(|f| f.hunks.len()).sum()
    }
}

/// Reverse a file header, given the paths the reversed patch will have.
///
/// The subtlety that cost a debugging round: the `a/` and `b/` prefixes are
/// **positional**, not part of the path. `---` is always the `a/` side. So
/// reversing swaps the *paths* between the two lines and leaves the prefixes
/// alone — for a file whose name did not change, the two lines come out
/// identical. Swapping the whole strings instead produces `--- b/x` / `+++ a/x`,
/// which libgit2 rejects with "mismatched old path names" because they no
/// longer agree with the `diff --git` line.
fn reverse_header(header: &[String], old_path: &str, new_path: &str) -> Vec<String> {
    const NULL: &str = "/dev/null";
    let side = |prefix: &str, path: &str| {
        if path == NULL || path.is_empty() {
            NULL.to_string()
        } else {
            format!("{prefix}/{path}")
        }
    };
    // git names the real file on both sides of `diff --git`, even when one side
    // is /dev/null, and names both ends of a rename.
    fn real<'a>(a: &'a str, b: &'a str) -> &'a str {
        if a == NULL || a.is_empty() {
            b
        } else {
            a
        }
    }
    let git_old = real(old_path, new_path);
    let git_new = real(new_path, old_path);

    header
        .iter()
        .map(|line| {
            if line.starts_with("--- ") {
                format!("--- {}", side("a", old_path))
            } else if line.starts_with("+++ ") {
                format!("+++ {}", side("b", new_path))
            } else if line.starts_with("diff --git ") {
                format!("diff --git a/{git_old} b/{git_new}")
            } else if let Some(rest) = line.strip_prefix("index ") {
                match rest.split_once("..") {
                    Some((a, b)) => {
                        // The mode, when present, trails the second hash.
                        let (b, mode) = b
                            .split_once(' ')
                            .map(|(b, m)| (b, format!(" {m}")))
                            .unwrap_or((b, String::new()));
                        format!("index {b}..{a}{mode}")
                    }
                    None => line.clone(),
                }
            } else if let Some(mode) = line.strip_prefix("new file mode ") {
                // Undoing a creation is a deletion.
                format!("deleted file mode {mode}")
            } else if let Some(mode) = line.strip_prefix("deleted file mode ") {
                format!("new file mode {mode}")
            } else if let Some(p) = line.strip_prefix("rename from ") {
                format!("rename to {p}")
            } else if let Some(p) = line.strip_prefix("rename to ") {
                format!("rename from {p}")
            } else {
                line.clone()
            }
        })
        .collect()
}

/// Write one file's patch, keeping the `@@` headers consistent.
///
/// `new_start` is recomputed as it goes: dropping or shrinking a hunk shifts
/// every later hunk in the same file, and a patch whose second hunk still
/// claims the original offset applies at the wrong place.
fn write_file(out: &mut String, file: &FilePatch, keep: impl Fn(usize, &Hunk) -> bool) {
    let kept: Vec<(usize, &Hunk)> =
        file.hunks.iter().enumerate().filter(|(i, h)| keep(*i, h)).collect();
    if kept.is_empty() && !file.hunks.is_empty() {
        return;
    }
    for line in &file.header {
        out.push_str(line);
        out.push('\n');
    }
    let mut delta: isize = 0;
    for (_, hunk) in kept {
        // A hunk with nothing on one side points that side at the line *before*
        // it — `@@ -0,0 +1,2 @@` for a new file, `@@ -2,2 +1,0 @@` for a pure
        // deletion. So the two sides are not simply offset by `delta`, and
        // treating them as though they were writes `+0,2` for a file that
        // starts at line 1.
        let (ol, nl) = (hunk.old_len(), hunk.new_len());
        let base = hunk.old_start as isize + delta;
        let new_start = if nl == 0 {
            base - 1
        } else if ol == 0 {
            base + 1
        } else {
            base
        };
        out.push_str(&hunk.header(new_start.max(0) as usize));
        out.push('\n');
        for line in &hunk.lines {
            out.push(line.origin.marker());
            out.push_str(&line.text);
            out.push('\n');
        }
        delta += hunk.new_len() as isize - hunk.old_len() as isize;
    }
}

/// `@@ -1,3 +1,4 @@ fn main()` → the hunk it describes, with no lines yet.
fn parse_hunk_header(rest: &str) -> Option<Hunk> {
    let (ranges, section) = match rest.split_once(" @@") {
        Some((r, s)) => (r, s.strip_prefix(' ').unwrap_or(s).to_string()),
        None => (rest.trim_end_matches(" @@"), String::new()),
    };
    let mut parts = ranges.split_whitespace();
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let start = |s: &str| s.split_once(',').map_or(s, |(a, _)| a).parse::<usize>().ok();
    Some(Hunk { old_start: start(old)?, new_start: start(new)?, section, lines: Vec::new() })
}

/// `a/src/main.rs` → `src/main.rs`; `/dev/null` is left alone.
fn strip_prefix_dir(path: &str) -> String {
    let path = path.split('\t').next().unwrap_or(path);
    match path.split_once('/') {
        Some((a, rest)) if a == "a" || a == "b" => rest.to_string(),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_HUNKS: &str = "\
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,4 +1,5 @@
 one
-two
+TWO
+two-and-a-half
 three
 four
@@ -10,3 +11,3 @@ fn tail()
 ten
-eleven
+ELEVEN
 twelve
";

    fn hunk(p: &Patch, i: usize) -> &Hunk {
        &p.files[0].hunks[i]
    }

    #[test]
    fn parses_files_hunks_and_line_origins() {
        let p = Patch::parse(TWO_HUNKS);
        assert_eq!(p.files.len(), 1);
        assert_eq!(p.files[0].old_path, "a.txt");
        assert_eq!(p.files[0].new_path, "a.txt");
        assert_eq!(p.hunk_count(), 2);

        let h = hunk(&p, 0);
        assert_eq!(h.old_start, 1);
        assert_eq!(h.new_start, 1);
        assert_eq!(h.old_len(), 4);
        assert_eq!(h.new_len(), 5);
        assert_eq!(h.lines[1], Line { origin: Origin::Removed, text: "two".into() });
        assert_eq!(h.lines[2], Line { origin: Origin::Added, text: "TWO".into() });

        // The section heading survives, because on a long diff it is the only
        // orientation there is.
        assert_eq!(hunk(&p, 1).section, "fn tail()");
    }

    /// The round trip has to be exact: `subset` and `reversed` are only
    /// trustworthy if writing back what was read changes nothing.
    #[test]
    fn parsing_and_writing_round_trips() {
        assert_eq!(Patch::parse(TWO_HUNKS).to_text(), TWO_HUNKS);
    }

    #[test]
    fn one_hunk_comes_out_as_a_standalone_patch() {
        let p = Patch::parse(TWO_HUNKS);
        let text = p.subset(&Selection { file: 0, hunk: 1, lines: None }).unwrap();
        assert_eq!(
            text,
            "\
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -10,3 +10,3 @@ fn tail()
 ten
-eleven
+ELEVEN
 twelve
"
        );
        // The second hunk stands alone, so it is no longer displaced by the
        // first: its new-side start is its own old start, not the original 11.
        assert!(text.contains("@@ -10,3 +10,3 @@"), "{text}");
    }

    /// The rule that makes partial staging work, and the easiest one to get
    /// wrong: a `-` you did not pick is still in the file being patched, so it
    /// has to be matched as context rather than dropped.
    #[test]
    fn an_unpicked_removal_becomes_context_and_an_unpicked_addition_vanishes() {
        let p = Patch::parse(TWO_HUNKS);
        // Take only `+TWO` (index 2), leaving `-two` (1) and `+two-and-a-half` (3).
        let text = p.subset(&Selection { file: 0, hunk: 0, lines: Some(vec![2]) }).unwrap();
        assert_eq!(
            text,
            "\
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,4 +1,5 @@
 one
 two
+TWO
 three
 four
"
        );

        let sub = Patch::parse(&text);
        let h = hunk(&sub, 0);
        assert_eq!(h.old_len(), 4, "the pre-image still has every line it had");
        assert_eq!(h.new_len(), 5, "one line added");
    }

    #[test]
    fn taking_only_a_removal_leaves_the_addition_behind() {
        let p = Patch::parse(TWO_HUNKS);
        let text = p.subset(&Selection { file: 0, hunk: 0, lines: Some(vec![1]) }).unwrap();
        assert!(text.contains("-two\n"), "{text}");
        assert!(!text.contains("+TWO"), "{text}");
        assert!(text.contains("@@ -1,4 +1,3 @@"), "counts not recomputed: {text}");
    }

    /// Selecting nothing has to be distinguishable from selecting everything.
    /// An all-context patch applies cleanly and does nothing, which would read
    /// to the caller as a silent success.
    #[test]
    fn a_selection_with_no_changes_in_it_is_refused() {
        let p = Patch::parse(TWO_HUNKS);
        assert!(p.subset(&Selection { file: 0, hunk: 0, lines: Some(vec![]) }).is_none());
        assert!(p.subset(&Selection { file: 9, hunk: 0, lines: None }).is_none());
        assert!(p.subset(&Selection { file: 0, hunk: 9, lines: None }).is_none());
    }

    #[test]
    fn reversing_swaps_the_sides_and_is_its_own_inverse() {
        let p = Patch::parse(TWO_HUNKS);
        let r = p.reversed();
        let h = &r.files[0].hunks[0];
        assert_eq!(h.lines[1], Line { origin: Origin::Added, text: "two".into() });
        assert_eq!(h.lines[2], Line { origin: Origin::Removed, text: "TWO".into() });
        assert_eq!(h.old_start, 1);
        assert_eq!(r.files[0].header[1], "index 2222222..1111111 100644");
        assert_eq!(r.to_text(), Patch::parse(&r.to_text()).to_text());
        assert_eq!(r.reversed().to_text(), TWO_HUNKS, "reverse is not an involution");
    }

    /// `\ No newline at end of file` annotates the line above it. Carrying it
    /// when its line was dropped produces a patch git refuses.
    #[test]
    fn the_no_newline_marker_travels_with_the_line_it_annotates() {
        let src = "\
diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 one
-two
\\ No newline at end of file
+two
";
        let p = Patch::parse(src);
        assert_eq!(p.files[0].hunks[0].lines[2].origin, Origin::NoNewline);
        assert_eq!(p.to_text(), src);

        // Taking only the addition demotes `-two` to context; the marker stays,
        // because the file being applied to still ends without a newline.
        let text = p.subset(&Selection { file: 0, hunk: 0, lines: Some(vec![3]) }).unwrap();
        assert!(text.contains("\\ No newline at end of file"), "{text}");
    }

    /// A file added or deleted whole has no `index` line pair to swap and one
    /// side is `/dev/null`. It must survive both transformations intact.
    #[test]
    fn a_new_file_round_trips_and_reverses() {
        let src = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+alpha
+beta
";
        let p = Patch::parse(src);
        assert_eq!(p.files[0].old_path, "/dev/null");
        assert_eq!(p.files[0].new_path, "new.txt");
        assert_eq!(p.to_text(), src);

        let r = p.reversed();
        let text = r.to_text();
        assert_eq!(r.files[0].hunks[0].lines[0].origin, Origin::Removed);
        // The `a/` and `b/` prefixes are positional: `---` is always the `a/`
        // side. Only the paths move, so the reversed patch reads as an ordinary
        // deletion — which is what libgit2 will accept.
        assert!(text.contains("--- a/new.txt"), "{text}");
        assert!(text.contains("+++ /dev/null"), "{text}");
        assert!(text.contains("diff --git a/new.txt b/new.txt"), "{text}");
        // Undoing a creation is a deletion, and the header has to say so.
        assert!(text.contains("deleted file mode 100644"), "{text}");
        assert_eq!(r.reversed().to_text(), src, "reverse is not an involution");
    }

    /// Multi-file patches keep their files separate, and a subset names one.
    #[test]
    fn a_subset_of_a_multi_file_patch_names_only_its_own_file() {
        let src = format!(
            "{TWO_HUNKS}diff --git a/b.txt b/b.txt\nindex 4444444..5555555 100644\n--- a/b.txt\n+++ b/b.txt\n@@ -1,1 +1,1 @@\n-old\n+new\n"
        );
        let p = Patch::parse(&src);
        assert_eq!(p.files.len(), 2);
        assert_eq!(p.hunk_count(), 3);
        let text = p.subset(&Selection { file: 1, hunk: 0, lines: None }).unwrap();
        assert!(text.contains("b/b.txt"), "{text}");
        assert!(!text.contains("a.txt"), "leaked the other file: {text}");
        assert!(text.contains("@@ -1 +1 @@"), "a one-line range drops its count: {text}");
    }

    /// Only real changes are selectable; context is what the change is anchored
    /// to, and offering it would let a user "stage" a line that never moved.
    #[test]
    fn only_added_and_removed_lines_can_be_selected() {
        let p = Patch::parse(TWO_HUNKS);
        assert_eq!(hunk(&p, 0).changed_line_indices(), vec![1, 2, 3]);
        assert_eq!(hunk(&p, 1).changed_line_indices(), vec![1, 2]);
    }
}
