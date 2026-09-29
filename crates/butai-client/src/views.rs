//! Which space each workspace was last looking at.
//!
//! The page used to be a property of this *client*: one `view.page`, carried
//! across every tab, so switching from a project you were reading the history
//! of to one you were reading the files of put you on the history of the
//! second. That is the wrong owner. A page is a way of looking at *one
//! workspace* — [`Page::ORDER`] says so, and it is why BOOTH is not in that
//! list — and which way you were looking at a project is a fact about the
//! project, not about the window it was in.
//!
//! So it is remembered per workspace, in `[views]` in the client's own config,
//! and this module is the whole of that: the key a workspace is filed under,
//! the in-memory copy the workbench reads on arrival, and the debounce that
//! keeps a page change from being a file rewrite per keystroke.
//!
//! Client-side for the reason the palette and the keymap are. The daemon
//! renders no chrome and has no page — two people attached to one workspace can
//! be on different ones, and neither of them is the workspace's opinion.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::chrome::Page;
use crate::config::Config;

/// How long a page change waits before it is written down.
///
/// The problem it solves is that a page change is a *keystroke*: `alt-o`, the
/// cycle keys, a click on a space button, and whatever else the user has bound.
/// Writing `~/.butai/config.toml` on each one is a read, a parse, a temp file
/// and a rename per press, on a file the user is allowed to be editing at the
/// same time.
///
/// Two seconds is longer than a walk through the cycle keys and far shorter
/// than a visit to a page, so the file sees the page you *stopped* on and not
/// the four you passed through. Leaving a workspace flushes early — see
/// [`Views::note`] — because that is the boundary the whole feature is about,
/// and so does the end of the workbench loop.
///
/// **What this cannot cover is a signal.** [`crate::term`]'s handlers put the
/// terminal back with two async-signal-safe syscalls and re-raise; writing a
/// config file from one is not on the table, and `SIGKILL` gets nothing at all.
/// So a `SIGTERM` in the two seconds after a page change loses that change and
/// nothing else — the window is what this constant bounds, and it is why the
/// answer is a short debounce rather than a write on exit.
const DEBOUNCE: Duration = Duration::from_secs(2);

/// How long a *failed* write waits before it is tried again, and the ceiling
/// that wait doubles up to.
///
/// **The failure this exists for is a temporary one.** A config file is
/// unwritable for a few seconds at a time and for ordinary reasons — a disk
/// that filled up, an editor holding the file, a directory whose permissions
/// were being changed underneath it — and the change that could not be written
/// is still true when it comes back. Dropping it on the first error meant a
/// briefly unwritable file cost that project its remembered page for good, with
/// nothing on screen to say so.
///
/// So the line stays owed, and the wait doubles: five seconds, then ten, then
/// twenty, up to five minutes. The doubling is what keeps this from becoming a
/// retry loop against a file that will *never* be writable — a read-only home
/// directory is a state a butai can sit in all day, and two failed writes an
/// hour is the right amount of effort to spend on it. There is nothing to undo
/// when it starts working again: the first write that lands resets the count,
/// so a file that comes back is back to the ordinary debounce.
const RETRY: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(300);

/// The most lines that may be owed at once.
///
/// A bound rather than a number that matters: on a working file this queue is
/// never longer than one, because every change is written within a couple of
/// seconds of being made. It grows only while writes are failing, at one entry
/// per *workspace* visited since the file broke, and a butai left open on a
/// read-only home for a week must not turn that into a list of every project
/// touched in it. Sixteen is more projects than a session moves between while a
/// disk stays full, and the oldest is what falls off — the same rule
/// [`Config::save_view_at`] applies to the table itself.
const OWED_CAP: usize = 16;

/// The badge for the daemon on this machine, which has none of its own.
///
/// The same word BOOTH's compute column puts on that row, for the same reason:
/// there is nothing to qualify the local daemon against, but a key is a list of
/// machines and an unnamed entry in one reads as a bug.
const LOCAL: &str = "local";

/// The `[views]` key a workspace is filed under: the machine it is on, then the
/// directory it is open in.
///
/// **The user chose machine plus path, and the alternative is worse than it
/// looks.** A [`SessionId`](butai_protocol::SessionId) is per daemon and is
/// handed out fresh on every restart, so a view saved against `1` would come
/// back attached to whatever project happened to be numbered `1` the next
/// morning — a remembered page that silently moves to another project is worse
/// than no memory at all. A path is what a workspace *is*.
///
/// The badge is what stops two machines colliding: `/srv/diffusion` open on
/// this laptop and on `gpu-box` are two workspaces and get two keys. The one
/// collision left is a `[[remote]]` whose `name` is literally `local`, which
/// would share a namespace with this machine's own daemon — the same ambiguity
/// that badge already creates in the tab bar, and not one worth a second
/// spelling here.
///
/// **Normalisation is deliberately only lexical.** Duplicate and trailing
/// slashes go, because those are spellings of one path that this side can be
/// certain about. `~` is left alone and symlinks are left alone, and both for
/// the same reason: the path may be on another machine, where this client's
/// `$HOME` is the wrong home and the link target is not ours to resolve. In
/// practice neither reaches here — the daemon canonicalises a workspace's
/// directory when it opens it, so `cwd` arrives absolute and link-free — and
/// the worst a spelling that slipped through can do is remember one project
/// under two keys.
pub fn key(host: Option<&str>, cwd: &str) -> String {
    format!("{}:{}", host.unwrap_or(LOCAL), normalise(cwd))
}

/// `//a//b/` → `/a/b`. See [`key`] for what is deliberately not done here.
fn normalise(cwd: &str) -> String {
    let mut out = String::with_capacity(cwd.len());
    for c in cwd.chars() {
        if c == '/' && out.ends_with('/') {
            continue;
        }
        out.push(c);
    }
    while out.len() > 1 && out.ends_with('/') {
        out.pop();
    }
    out
}

/// The remembered views, and the changes that have not reached the file yet.
///
/// Seeded from the config at launch and updated as this client works. It is
/// deliberately *not* re-read: another workbench open on the same project has
/// its own cursor and its own page, and "what was this project last looking at"
/// has no better answer than the one the client being asked has been watching.
/// Each writes the line it is about and leaves the others alone, so neither
/// erases the other's — see [`Config::save_view_at`].
#[derive(Debug)]
pub struct Views {
    /// The config file to write back to. Held rather than resolved on each
    /// write so a test can point a whole `Views` at a temp path, which is the
    /// `save_*_at` bargain one level up.
    path: PathBuf,
    seen: HashMap<String, Page>,
    /// The lines that are owed to the file, oldest first.
    ///
    /// A queue rather than the single slot this was, and the retry is why.
    /// Leaving a workspace writes what that workspace owed, so on a file that
    /// accepts writes there is only ever one entry — the change on the
    /// workspace you are still in. A write that *fails* hands its line back,
    /// and that line outlives the workspace it was made on, so the change you
    /// make on the project you moved to must not be able to discard it. One
    /// entry per workspace, [`OWED_CAP`] of them, oldest evicted.
    owed: VecDeque<(String, Page)>,
    /// When the front of [`owed`](Self::owed) is due. Fixed at the moment the
    /// first change arrived rather than pushed back by each later one.
    due: Option<Instant>,
    /// Consecutive failed writes, which is the only thing [`wait`](Self::wait)
    /// is computed from. Reset by the first write that lands.
    fails: u32,
}

impl Views {
    /// Seeded from a loaded config, writing back to the file it came from.
    pub fn new(cfg: &Config) -> Self {
        Self::at(Config::path(), cfg)
    }

    /// [`new`](Self::new) against an explicit path (tests).
    pub fn at(path: PathBuf, cfg: &Config) -> Self {
        let seen = cfg
            .views
            .iter()
            .filter_map(|(k, v)| Page::space_named(v).map(|p| (k.clone(), p)))
            .collect();
        Self { path, seen, owed: VecDeque::new(), due: None, fails: 0 }
    }

    /// The space `key` was last looking at, if this build knows the word the
    /// file wrote.
    pub fn page_for(&self, key: &str) -> Option<Page> {
        self.seen.get(key).copied()
    }

    /// Take note of where `key` is being looked at now, and write down anything
    /// that has come due.
    ///
    /// Called once a frame with whatever is on screen, rather than from the
    /// dozen places a page can change. The workbench already learned that
    /// lesson twice — `open_page` is the single funnel for arriving somewhere,
    /// and the loop notices a tab change in one place because seven gestures
    /// move it — and this is the same shape: a page is a state, so the thing to
    /// watch is the state.
    ///
    /// **Only a view *of a workspace* is recorded.** BOOTH spans machines,
    /// SETTINGS is about this client and HELP is about the program, so being on
    /// one of the three says nothing about where this project was left, and
    /// [`Page::is_space`] is the existing test for exactly that. Storing one
    /// would mean walking into a project and landing in the settings page
    /// because that is where you happened to be when you last walked out.
    /// DIFF is out on the same terms: it is what is *on* the stage rather than
    /// somewhere you navigated to.
    pub fn note(&mut self, key: Option<&str>, page: Page, now: Instant) {
        // A pending line belongs to the workspace it was made on, and leaving
        // that workspace is the boundary the debounce was waiting for anyway,
        // so it goes now rather than when the clock catches up.
        //
        // Not while a write is failing, though: the backoff owns the timing
        // then. A line handed back by a failed write belongs to a workspace you
        // may have long since left, so this test stays true frame after frame —
        // and every one of them would be another attempt on a file that is
        // still broken, which is exactly the loop [`RETRY`] exists to prevent.
        if self.fails == 0 && self.owed.iter().any(|(k, _)| Some(k.as_str()) != key) {
            self.flush();
        }
        if let Some(key) = key {
            if page.is_space() && self.seen.get(key) != Some(&page) {
                self.seen.insert(key.to_string(), page);
                self.owe(key, page);
            }
        }
        if self.due.is_some_and(|due| now >= due) {
            self.flush();
        }
        // Fixed rather than sliding. A sliding deadline is one a held `alt-.`
        // pushes out of reach for as long as it is held, which turns a debounce
        // into a write that never happens.
        //
        // Armed from whatever is owed rather than from the change that just
        // arrived, which is what makes a failed write get another go: `flush`
        // hands its line back and clears the deadline, and the next frame sets
        // a new one — a longer one, by [`Self::wait`].
        if !self.owed.is_empty() && self.due.is_none() {
            self.due = Some(now + self.wait());
        }
    }

    /// Add a line to what is owed, replacing whatever was owed for that same
    /// workspace.
    ///
    /// One entry per workspace, so four `alt-o`s on one project are still one
    /// line in the file rather than four rewrites of it — the property the
    /// debounce is there for, kept now that there is a queue to keep it in.
    fn owe(&mut self, key: &str, page: Page) {
        if let Some(slot) = self.owed.iter_mut().find(|(k, _)| k.as_str() == key) {
            slot.1 = page;
            return;
        }
        self.owed.push_back((key.to_string(), page));
        while self.owed.len() > OWED_CAP {
            self.owed.pop_front();
        }
    }

    /// How long the next attempt waits: the debounce after a write that landed,
    /// and a doubling backoff after one that did not.
    fn wait(&self) -> Duration {
        match self.fails {
            0 => DEBOUNCE,
            // Shifted by one less than the count, so the first retry is `RETRY`
            // itself; clamped twice, once so the shift cannot overflow and once
            // so the wait cannot outgrow the ceiling.
            n => RETRY.saturating_mul(1u32 << (n - 1).min(6)).min(RETRY_MAX),
        }
    }

    /// Write what is owed. The end of the workbench loop calls this, so a
    /// detach never leaves the last page unwritten.
    ///
    /// **A line that cannot be written stays owed.** It used to be dropped, and
    /// dropped silently — the in-memory copy had already recorded the page, so
    /// nothing ever asked for that write again and a config file that was
    /// unwritable for ten seconds cost that project its remembered page until
    /// the next time you happened to change it. Putting the line back is the
    /// whole fix; [`RETRY`] is what stops it from becoming a loop.
    ///
    /// The first failure stops the run rather than skipping past it. Every line
    /// here goes to the same file, so the second write is going to fail for the
    /// reason the first one did, and the order they were made in is the order
    /// [`Config::save_view_at`] wants them — its table evicts the least
    /// recently written.
    pub fn flush(&mut self) {
        self.due = None;
        while let Some((key, page)) = self.owed.pop_front() {
            if let Err(e) = Config::save_view_at(&self.path, &key, page) {
                // Not a flash, and that part was always right. Nobody asked for
                // this write — it is the side effect of having pressed `alt-o`
                // two seconds ago — and a config file that cannot be written
                // would otherwise put the same sentence over the footer every
                // time the user changed page.
                tracing::debug!("views: {key} not saved: {e}");
                self.owed.push_front((key, page));
                self.fails = self.fails.saturating_add(1);
                return;
            }
        }
        self.fails = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("butai-views-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("config.toml")
    }

    fn config_at(path: &std::path::Path) -> Config {
        let (cfg, warnings) = Config::load_from(path);
        assert!(warnings.is_empty(), "{warnings:?}");
        cfg
    }

    /// The key is a machine and a path, and the machine half is what keeps two
    /// of them apart: a client with a laptop and a GPU box in its tab bar
    /// routinely has the same project checked out on both.
    #[test]
    fn the_key_is_a_machine_and_a_path() {
        assert_eq!(key(None, "/media/nvme/Projects/butai"), "local:/media/nvme/Projects/butai");
        assert_eq!(key(Some("gpu-box"), "/srv/diffusion"), "gpu-box:/srv/diffusion");
        assert_ne!(key(None, "/srv/diffusion"), key(Some("gpu-box"), "/srv/diffusion"));
    }

    /// Two spellings of one directory are one workspace, for the spellings this
    /// side can be sure about. `~` and symlinks are not among them — see
    /// [`key`] — and the daemon has already resolved both by the time a `cwd`
    /// gets here.
    #[test]
    fn the_obvious_spellings_of_a_path_are_one_key() {
        let want = key(None, "/srv/x");
        assert_eq!(key(None, "/srv/x/"), want);
        assert_eq!(key(None, "/srv/x///"), want);
        assert_eq!(key(None, "//srv//x"), want);
        // Root survives being trimmed to nothing.
        assert_eq!(key(None, "/"), "local:/");
    }

    /// Two workspaces' views round-trip through the file, and a word this build
    /// does not know is one line ignored rather than a config that fails to
    /// load.
    ///
    /// The unknown value is the whole point of holding the table as strings: a
    /// newer butai with a page this one has never heard of must not be able to
    /// stop an older one from starting.
    #[test]
    fn views_round_trip_and_an_unknown_page_is_ignored() {
        let path = tmp("roundtrip");
        std::fs::write(&path, "# my setup\n[theme]\nname = \"terminal\"\n").unwrap();

        Config::save_view_at(&path, &key(None, "/p/butai"), Page::Git).unwrap();
        Config::save_view_at(&path, &key(Some("gpu-box"), "/srv/diffusion"), Page::Files).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my setup"), "{text}");
        assert!(text.contains(r#""local:/p/butai" = "git""#), "{text}");
        assert!(text.contains(r#""gpu-box:/srv/diffusion" = "files""#), "{text}");
        assert!(text.contains("[theme]"), "the other tables are still there: {text}");

        let cfg = config_at(&path);
        let views = Views::at(path.clone(), &cfg);
        assert_eq!(views.page_for(&key(None, "/p/butai")), Some(Page::Git));
        assert_eq!(views.page_for(&key(Some("gpu-box"), "/srv/diffusion")), Some(Page::Files));
        assert_eq!(views.page_for(&key(None, "/p/never-seen")), None);

        // A page name from a build that has one more page than this one.
        std::fs::write(
            &path,
            "[views]\n\"local:/p/butai\" = \"holodeck\"\n\"local:/p/caliper\" = \"docs\"\n",
        )
        .unwrap();
        let cfg = config_at(&path);
        let views = Views::at(path.clone(), &cfg);
        assert_eq!(views.page_for(&key(None, "/p/butai")), None, "the unknown word is skipped");
        assert_eq!(
            views.page_for(&key(None, "/p/caliper")),
            Some(Page::Docs),
            "and the line beside it still loads"
        );

        // A hand-edited `views` that is not a table is replaced rather than
        // indexed into — the same defence every other writer in `config` has.
        std::fs::write(&path, "views = 5\n").unwrap();
        Config::save_view_at(&path, &key(None, "/p/butai"), Page::Docker).unwrap();
        assert_eq!(
            config_at(&path).views.get(&key(None, "/p/butai")).map(String::as_str),
            Some("docker")
        );

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// BOOTH, SETTINGS and HELP are not views of a workspace, so a workspace
    /// never remembers one.
    ///
    /// The constraint the whole feature rests on. All three are peers of the
    /// workspace chips rather than entries in the list of views — BOOTH spans
    /// machines, SETTINGS is about this client, HELP is about the program — and
    /// remembering one would mean walking into a project and being thrown into
    /// the settings page because that is where you were when you last left it.
    /// Both halves are checked: nothing is written, and a line that says one
    /// anyway (a hand edit) is not read back.
    #[test]
    fn booth_settings_and_help_are_not_views_of_a_workspace() {
        let path = tmp("not-a-view");
        let cfg = config_at(&path);
        let mut views = Views::at(path.clone(), &cfg);
        let now = Instant::now();
        let k = key(None, "/p/butai");

        for page in [Page::Booth, Page::Settings, Page::Help, Page::Diff] {
            views.note(Some(&k), page, now);
            views.flush();
            assert_eq!(views.page_for(&k), None, "{page:?} was remembered as a view");
            assert!(!path.exists(), "{page:?} wrote a config file to say so");
        }

        // And a space in between is remembered, so the test above is testing
        // the filter rather than a `note` that does nothing at all.
        views.note(Some(&k), Page::Git, now);
        views.flush();
        assert_eq!(views.page_for(&k), Some(Page::Git));

        // The reading half: a hand-written `settings` is a word `space_named`
        // does not answer to, so it is skipped like any other.
        std::fs::write(&path, "[views]\n\"local:/p/butai\" = \"settings\"\n").unwrap();
        let cfg = config_at(&path);
        assert_eq!(Views::at(path.clone(), &cfg).page_for(&k), None);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// The table stops growing at the cap, and what falls off is what has been
    /// left alone the longest.
    #[test]
    fn the_table_is_bounded_and_evicts_the_oldest() {
        let path = tmp("evict");
        let cap = crate::config::VIEWS_CAP;

        for i in 0..cap {
            Config::save_view_at(&path, &key(None, &format!("/p/{i}")), Page::Files).unwrap();
        }
        assert_eq!(config_at(&path).views.len(), cap);

        // Touching the oldest again moves it to the back of the queue, so the
        // *next* one along is what the overflow evicts.
        Config::save_view_at(&path, &key(None, "/p/0"), Page::Git).unwrap();
        Config::save_view_at(&path, &key(None, "/p/new"), Page::Docs).unwrap();

        let cfg = config_at(&path);
        assert_eq!(cfg.views.len(), cap, "the cap held: {}", cfg.views.len());
        assert!(cfg.views.contains_key(&key(None, "/p/new")), "the newest entry is in");
        assert_eq!(
            cfg.views.get(&key(None, "/p/0")).map(String::as_str),
            Some("git"),
            "re-visiting a project keeps it, and updates it"
        );
        assert!(!cfg.views.contains_key(&key(None, "/p/1")), "the least recent one went");

        // Far past the cap, from a table that starts over it: the bound is on
        // the file, not on how it got there.
        for i in 0..cap * 2 {
            Config::save_view_at(&path, &key(None, &format!("/q/{i}")), Page::Docker).unwrap();
        }
        assert_eq!(config_at(&path).views.len(), cap);

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A page change waits, and leaving the workspace does not.
    ///
    /// The two halves of the write policy: `alt-o` four times in a row is one
    /// line in the file and not four rewrites of it, and the change you made on
    /// the project you just left is on disk before the project you arrived at
    /// has a chance to replace it.
    #[test]
    fn a_page_change_is_debounced_and_a_workspace_change_is_not() {
        let path = tmp("debounce");
        let cfg = config_at(&path);
        let mut views = Views::at(path.clone(), &cfg);
        let start = Instant::now();
        let butai = key(None, "/p/butai");
        let caliper = key(None, "/p/caliper");

        views.note(Some(&butai), Page::Files, start);
        views.note(Some(&butai), Page::Docker, start);
        views.note(Some(&butai), Page::Git, start);
        assert!(!path.exists(), "three presses in a row should not have written anything yet");

        // The clock catches up, and only the page it was left on lands.
        views.note(Some(&butai), Page::Git, start + DEBOUNCE);
        assert_eq!(
            config_at(&path).views.get(&butai).map(String::as_str),
            Some("git"),
            "the page it stopped on"
        );

        // A change on the way out is written by the change of workspace, with
        // no clock involved at all.
        views.note(Some(&butai), Page::Docs, start + DEBOUNCE);
        views.note(Some(&caliper), Page::Agents, start + DEBOUNCE);
        assert_eq!(config_at(&path).views.get(&butai).map(String::as_str), Some("docs"));

        // And the loop's own exit is the backstop for the last one.
        views.flush();
        assert_eq!(config_at(&path).views.get(&caliper).map(String::as_str), Some("agents"));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// A write that could not happen is owed until it can, and one that
    /// happened is not repeated.
    ///
    /// The bug this is about was quiet in both directions. `note` recorded the
    /// page in memory *before* `flush` tried the file, so a failed write left
    /// the client believing the line was on disk: nothing ever asked for it
    /// again, and a `config.toml` that was briefly unwritable — a full disk, an
    /// editor holding it, a permission that was being changed — cost that
    /// project its remembered page for good, with only a `debug!` to say so.
    ///
    /// The write is made to fail the way it fails in the field, by making the
    /// path unwritable rather than by faking the error: the config sits under a
    /// directory that is really a file, so `create_dir_all` refuses. Removing
    /// it is the disk coming back.
    #[test]
    fn a_failed_write_is_owed_until_it_lands_and_is_not_repeated() {
        let dir = tmp("retry").parent().unwrap().to_path_buf();
        // A regular file standing where the config's directory should be.
        let blocker = dir.join("blocked");
        std::fs::write(&blocker, "").unwrap();
        let path = blocker.join("config.toml");

        let mut views = Views::at(path.clone(), &config_at(&path));
        let start = Instant::now();
        let butai = key(None, "/p/butai");
        let caliper = key(None, "/p/caliper");

        views.note(Some(&butai), Page::Git, start);
        views.note(Some(&butai), Page::Git, start + DEBOUNCE);
        assert!(!path.exists(), "the write cannot have happened");
        assert_eq!(views.fails, 1, "and the failure was counted");
        assert_eq!(views.owed.len(), 1, "the line is still owed rather than dropped");

        // A change on the project you moved to does not throw the owed one
        // away: it is a queue precisely so a failed line can outlive the
        // workspace it was made on.
        views.note(Some(&caliper), Page::Files, start + DEBOUNCE);
        assert_eq!(views.owed.len(), 2);

        // The disk comes back — and the retry still waits, because a file that
        // has been failing is not one to ask again on the very next frame.
        std::fs::remove_file(&blocker).unwrap();
        views.note(Some(&caliper), Page::Files, start + DEBOUNCE + Duration::from_millis(1));
        assert!(!path.exists(), "the backoff held rather than retrying at once");

        // Once it is up, everything owed lands, oldest first.
        views.note(Some(&caliper), Page::Files, start + DEBOUNCE + RETRY);
        let cfg = config_at(&path);
        assert_eq!(cfg.views.get(&butai).map(String::as_str), Some("git"));
        assert_eq!(cfg.views.get(&caliper).map(String::as_str), Some("files"));
        assert!(views.owed.is_empty(), "nothing is owed once it is written");
        assert_eq!(views.fails, 0, "and the backoff is back to the ordinary debounce");

        // A line that landed is not written again. The file is edited by hand
        // to something else; the frames that follow leave it exactly there.
        std::fs::write(&path, "[views]\n\"local:/p/butai\" = \"docs\"\n").unwrap();
        for i in 0..3 {
            views.note(Some(&butai), Page::Git, start + Duration::from_secs(600 + i));
        }
        views.flush();
        assert_eq!(
            config_at(&path).views.get(&butai).map(String::as_str),
            Some("docs"),
            "nothing was owed, so nothing was written"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Two clients do not erase each other.
    ///
    /// Each writes the line it is about, so the second one to save leaves the
    /// first one's projects where they were — the property every other writer
    /// in [`crate::config`] has, and the reason none of them serialises the
    /// whole struct back out.
    #[test]
    fn a_second_client_writing_leaves_the_first_ones_entries_alone() {
        let path = tmp("two-clients");
        let butai = key(None, "/p/butai");
        let caliper = key(Some("gpu-box"), "/p/caliper");

        let mut one = Views::at(path.clone(), &config_at(&path));
        let mut two = Views::at(path.clone(), &config_at(&path));
        let now = Instant::now();

        one.note(Some(&butai), Page::Git, now);
        one.flush();
        two.note(Some(&caliper), Page::Files, now);
        two.flush();

        let cfg = config_at(&path);
        assert_eq!(cfg.views.get(&butai).map(String::as_str), Some("git"));
        assert_eq!(cfg.views.get(&caliper).map(String::as_str), Some("files"));

        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
