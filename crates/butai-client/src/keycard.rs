//! The workbench's keys, as data: one table the `?` card draws and the HELP
//! page can read, so the two cannot come to list different keys.
//!
//! Vim style. `h` `j` `k` `l` move, `gg` / `G` go to either end, `gt` / `gT`
//! walk the tabs, `i` hands the keyboard to the pane on the stage and
//! `ctrl-\ ctrl-n` (neovim's way out of a terminal) takes it back. The Alt
//! layer and the `C-b` prefix are aliases for the same verbs and still work.
//!
//! Nothing here binds a key: dispatch lives in [`crate::workbench`] and
//! [`crate::verbs`]. A row added here without a binding is a lie on screen, so
//! the test below checks the ones it can.

/// A titled group of `(keys, what they do)` rows.
pub type Group = (&'static str, &'static [(&'static str, &'static str)]);

/// The workbench's keys on one card, vim style: what `?` opens.
///
/// One screen and no more. The reference behind `?` used to be thirteen pages,
/// which is where you go to learn *why*; this is where you look up *which*.
/// Each group is a heading and its rows, `(keys, what)`. The Alt layer and the
/// prefix still work and are only mentioned at the bottom: they are aliases for
/// the same verbs, kept for the fingers that have learned them.
pub const KEY_CARD: [Group; 4] = [
    (
        "MOVE",
        &[
            ("j k", "down · up"),
            ("h l", "left rail · CHANGES"),
            ("gg G", "top · bottom"),
            ("enter", "show the row on the stage"),
            ("i", "type into the stage"),
            ("C-\\ C-n", "back out of the stage"),
        ],
    ),
    (
        "ROWS",
        &[
            ("a A", "new agent · choose which"),
            ("x", "close the row"),
            ("F", "fork the agent"),
            ("t r", "new shell · restart it"),
            ("space", "fold an agent's helpers"),
            ("m", "the row's menu"),
        ],
    ),
    (
        "WORKSPACES",
        &[
            ("gt gT", "next · previous tab"),
            ("n", "open a folder"),
            ("X", "close this workspace"),
            ("alt-1..9", "tab by number"),
            ("alt-0", "BOOTH, every agent"),
            ("q", "detach (all keeps running)"),
        ],
    ),
    (
        "TOOLS",
        &[
            ("/", "find in files"),
            ("f", "follow a link"),
            (":", "type a command"),
            ("g b", "git menu · branch"),
            ("c", "commit (on CHANGES)"),
            ("alt-o r m", "files · git · docs"),
        ],
    ),
];

/// The card's last line: where the rest is.
pub const KEY_CARD_FOOT: &str =
    "? again: full reference · esc: close · alt and C-b keys still work";

#[cfg(test)]
mod tests {
    use super::*;

    /// The card is one screen: every group fits the column it is drawn in, and
    /// no key is listed twice.
    #[test]
    fn the_card_is_one_screen_with_no_key_twice() {
        let mut seen = std::collections::HashSet::new();
        for (head, rows) in KEY_CARD {
            assert!(rows.len() <= 8, "{head} is too long for one screen");
            for (keys, what) in rows {
                assert!(keys.chars().count() < 11, "{keys:?} overruns the key column");
                assert!(what.chars().count() <= 28, "{what:?} overruns its column");
                assert!(seen.insert(*keys), "{keys:?} is on the card twice");
            }
        }
    }
}
