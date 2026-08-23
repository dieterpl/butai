//! The client's half of `~/.butai/config.toml`.
//!
//! One file, two readers. This one takes `[general] prefix`, `[keys]`,
//! `[theme]`, `[ui]` and `[[remote]]` — everything about how a workbench looks
//! and answers; the daemon's [`ServerConfig`](butai_server::config::ServerConfig)
//! takes the shell, the scrollback and `[[agents]]`. Neither struct declares
//! the other's tables and serde ignores what it does not know, so each side
//! parses the whole file and sees only its own part.
//!
//! Splitting them is the point. A palette is not something a daemon can have an
//! opinion about — two people on one daemon can want different ones — and a
//! scrollback budget is not something a client can enforce. Keeping one struct
//! meant every field looked shared whether it was or not.
//!
//! **Writes are surgical.** `save_ui`, `save_default_agent`,
//! `save_declined_version` and `save_remote`/`forget_remote` rewrite a single
//! key or block through
//! `toml_edit` and leave every other key, comment and blank line where it was,
//! because this is a file a person edits by hand.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The palette a config with no `[theme] name` gets.
pub const DEFAULT_THEME: &str = "blueprint-dark";

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub general: General,
    pub keys: HashMap<String, String>,
    pub theme: Theme,
    pub ui: UiConfig,
    /// `[[remote]]` blocks: other daemons whose workspaces join this one's tab
    /// bar. Connected at start, so they are there without a gesture every
    /// morning.
    pub remote: Vec<RemoteDef>,
    /// `[update]`: whether to look for a newer release, and which one was
    /// turned down.
    pub update: UpdateConfig,
}

/// One `[[remote]]` block.
///
/// ```toml
/// [[remote]]
/// host = "gpu-box"            # an ssh destination: alias, or user@host
/// # name = "gpu"              # tab badge; defaults to the destination
/// # ssh_args = ["-p", "2222"] # extra ssh flags, before the destination
///
/// [[remote]]
/// socket = "/tmp/fwd.sock"    # instead of ssh: an already-forwarded socket
/// ```
///
/// `host` and `socket` are the two ways in, and are mutually exclusive: one
/// runs `ssh … butai proxy`, the other connects to a socket that is already
/// reachable (an `ssh -N -L` forward).
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct RemoteDef {
    /// Tab badge. Defaults to the ssh destination, or the socket's file stem.
    pub name: Option<String>,
    /// ssh destination.
    pub host: Option<String>,
    /// Extra ssh arguments, placed before the destination.
    pub ssh_args: Vec<String>,
    /// A socket reachable from here, instead of dialling ssh ourselves.
    pub socket: Option<String>,
    /// `BUTAI_SOCKET` for the far daemon. Normally left unset so the far `butai`
    /// resolves its own default and finds the daemon already running there,
    /// rather than starting a second one on a path nothing else uses.
    pub socket_path: Option<String>,
}

/// `[update]`.
///
/// ```toml
/// [update]
/// check = true                 # look for a newer release at all
/// declined_version = "1.1.0"   # written when you answer no; that one stops asking
/// ```
///
/// Client-side, because the client is the side that can ask a question and the
/// side that owns the binary a person actually runs. The daemon never declares
/// this table and serde ignores what it does not know, so it costs the daemon
/// nothing.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UpdateConfig {
    /// Ask GitHub whether a newer release exists. On by default: an update you
    /// are not told about is one you do not get.
    ///
    /// This is the only outbound request a butai client makes. Turning it off
    /// — here, or with `BUTAI_NO_UPDATE_CHECK` for a packaged install whose
    /// updates arrive some other way — stops it entirely.
    pub check: bool,
    /// A version that was offered and turned down.
    ///
    /// Answering no to the prompt is an answer about *that release*, not about
    /// updating: it is written here so 1.1.0 stops asking, and left behind when
    /// 1.2.0 comes out so that one asks once of its own. Cleared by hand, or by
    /// `butai update`, which ignores it — somebody who types the command has
    /// changed their mind by definition.
    pub declined_version: Option<String>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self { check: true, declined_version: None }
    }
}

impl UpdateConfig {
    /// Whether `version` is the one already turned down.
    pub fn declined(&self, version: &str) -> bool {
        self.declined_version.as_deref() == Some(version)
    }
}

/// `[general]`, client side. The daemon's struct declares a different set of
/// keys from the same table.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct General {
    pub prefix: String,
    /// Agent spawned straight away by the AGENTS `+` button, skipping the
    /// picker. `None` — the default — asks every time.
    ///
    /// Held as a name rather than an index so it survives reordering
    /// `[[agents]]`, and checked against `GET /v1/agents` before it is saved:
    /// a pin left behind by a renamed agent should cost a keystroke, not fail
    /// every spawn.
    pub default_agent: Option<String>,
    /// Whether a `butai` run over ssh inside a pane may pull its machine into
    /// this tab bar on its own.
    ///
    /// On by default: it is the whole point of typing `butai` after `ssh`, and
    /// the far side only announces itself when it has already confirmed it is
    /// inside a butai pane. Turn it off if you would rather connect hosts
    /// deliberately with `[+ host]`.
    ///
    /// Read here rather than by the daemon because the daemon does not dial —
    /// it reports the announcement and this side decides.
    pub remote_auto_attach: bool,
    /// Read macOS's Option-composed characters back as the Alt layer.
    ///
    /// On a Mac, Option is a compose key, not a modifier: Option-o types `ø`
    /// and no terminal ever reports Alt. That is a keyboard setting, not a bug
    /// — but it means the whole Alt layer is dead out of the box for most Mac
    /// users, and nothing on screen says why.
    ///
    /// So the client maps the characters back. `ø` becomes Alt-o, but only for
    /// the characters the workbench actually binds: `∫` is Option-b, which
    /// nothing binds, so it stays a character you can type.
    ///
    /// Defaults to on when this client runs on macOS. The cost is that the
    /// mapped characters cannot be typed into a pane, which matters if you
    /// write Danish or Norwegian — turn it off, and either use the `{prefix}`
    /// layer, which reaches everything the Alt layer does, or set your terminal
    /// to send Alt (Terminal.app: "Use Option as Meta Key"; iTerm2: Left Option
    /// = "Esc+"; Ghostty: `macos-option-as-alt = true`).
    pub option_as_alt: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            prefix: "C-b".into(),
            default_agent: None,
            remote_auto_attach: true,
            option_as_alt: cfg!(target_os = "macos"),
        }
    }
}

/// `[ui]` table: chrome geometry. Global — the rail geometry applies to every
/// workspace at once, so resizing one rail reflows the whole workbench.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Width of the left rail (agents/processes/system) in cells.
    pub left_rail: Option<u16>,
    /// Width of the right (changes) rail in cells.
    pub right_rail: Option<u16>,
    /// Rows given to the PROCESSES section of the left rail; AGENTS takes
    /// whatever is left over after this and `system_height`.
    pub procs_height: Option<u16>,
    /// Rows given to the SYSTEM gauges at the foot of the left rail.
    pub system_height: Option<u16>,
    /// Which network interfaces the SYSTEM rail draws.
    pub net: NetSelect,
    /// Which mounted filesystems the SYSTEM rail draws.
    pub disks: DiskSelect,
    /// Whether a URL on screen is marked up as a hyperlink for the terminal
    /// butai is drawn on, so the pointer can follow it.
    ///
    /// On by default: it is an OSC sequence a terminal that does not know it
    /// discards, which is every terminal we could find, and tmux before 3.4
    /// drops it as well. Off is for the one that turns out to *print* it —
    /// and the picker (`alt-f`) keeps working either way, because it never
    /// leaves this client.
    pub links: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            left_rail: None,
            right_rail: None,
            procs_height: None,
            system_height: None,
            net: NetSelect::default(),
            disks: DiskSelect::default(),
            links: true,
        }
    }
}

/// `[ui] net`: which interfaces get a NET gauge.
///
/// ```toml
/// [ui]
/// net = "all"                      # every real link, capped (the default)
/// # net = "auto"                   # one: the default route, else the busiest
/// # net = ["enp1s0", "vpn-tunnel"] # exactly these, in this order
/// ```
///
/// The daemon publishes every interface it can see and says what each one *is*;
/// this is the client's side of that bargain. A list is honoured literally —
/// name a bridge and you get the bridge — because an explicit request is not
/// something to second-guess.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum NetSelect {
    Mode(NetMode),
    Named(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetMode {
    /// One gauge: whichever interface carries the default route, else the
    /// busiest real link. What the rail did before it could hold more than one.
    Auto,
    /// Every link that is up and not double-counted, capped so a docker host
    /// with three dozen interfaces does not eat the rail.
    All,
}

impl Default for NetSelect {
    fn default() -> Self {
        Self::Mode(NetMode::All)
    }
}

/// `[ui] disks`: which mounts get a DSK gauge.
///
/// ```toml
/// [ui]
/// disks = "all"                  # every real disk, capped (the default)
/// # disks = "auto"               # one: the filesystem holding /
/// # disks = ["/", "/media/fast"] # exactly these, in this order
/// # disks = []                   # none, and the rail keeps the rows
/// ```
///
/// The same bargain [`NetSelect`] states, for the same reason: the daemon
/// publishes every mount and says what each one *is*, and this is the client's
/// side of it. A list is honoured literally — name `/dev/shm` and you get the
/// tmpfs — because an explicit request is not something to second-guess.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum DiskSelect {
    Mode(DiskMode),
    Named(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskMode {
    /// One gauge: the filesystem holding `/`, else the largest real disk.
    Auto,
    /// Every real disk, capped so a docker host — where each image layer is a
    /// mount — does not eat the rail.
    All,
}

impl Default for DiskSelect {
    fn default() -> Self {
        Self::Mode(DiskMode::All)
    }
}

/// The workbench's rail geometry: the two rail widths plus the section heights
/// inside them.
///
/// A `None` height means the section sizes itself to the terminal, which is how
/// every one of them starts out. Layout mode's ↑/↓ keys pin it to a row count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RailGeom {
    pub left_w: u16,
    pub right_w: u16,
    /// Rows for PROCESSES; AGENTS gets the remainder of the rail.
    pub procs_h: Option<u16>,
    /// Rows for the SYSTEM gauges.
    pub system_h: Option<u16>,
}

/// `[theme]`: a palette name plus per-role overrides.
///
/// ```toml
/// [theme]
/// name = "blueprint-dark"   # built-in, or ~/.butai/themes/<name>.toml
/// accent = "#ff8800"        # override one role without writing a theme file
/// ```
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// A built-in palette, or a file in the themes directory.
    pub name: String,
    /// Accepted and ignored: it named a syntect theme back when the daemon ran
    /// files through syntect for its own editor pane. Source is coloured from
    /// the same roles as the rest of the chrome now. Kept so an existing config
    /// neither breaks nor has the key mistaken for a role override.
    pub syntax_theme: String,
    /// Every other key in the table: role name -> color. Kept loose so an
    /// unknown role warns during theme resolution instead of failing the whole
    /// config parse, matching this module's version-skew policy.
    #[serde(flatten)]
    pub colors: HashMap<String, String>,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            name: DEFAULT_THEME.into(),
            syntax_theme: "base16-ocean.dark".into(),
            colors: HashMap::new(),
        }
    }
}

impl Theme {
    /// Role overrides to apply over the named palette, sorted so warnings come
    /// out in a stable order. `syntax_theme` is not a role and is excluded.
    pub fn role_overrides(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> =
            self.colors.iter().map(|(k, v)| (legacy_role_alias(k), v.as_str())).collect();
        out.sort_by_key(|(k, _)| *k);
        out
    }
}

/// Pre-role key names, kept working: `border` and `border_focused` were the
/// whole theme surface before roles existed.
fn legacy_role_alias(key: &str) -> &str {
    match key {
        "border" => "rule",
        "border_focused" => "rule_focus",
        other => other,
    }
}

impl Config {
    /// `~/.butai/config.toml`. The same file the daemon reads its own half of.
    pub fn path() -> PathBuf {
        butai_protocol::paths::config_path()
    }

    /// Load from the default path; missing file yields defaults. Returns
    /// human-readable warnings (parse fallback, and so on).
    pub fn load() -> (Self, Vec<String>) {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &Path) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let cfg = match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    warnings.push(format!("{}: {e}; using defaults", path.display()));
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        };
        (cfg, warnings)
    }

    /// Pin (or clear) the agent the AGENTS `+` button spawns without asking.
    pub fn save_default_agent(name: Option<&str>) -> std::io::Result<()> {
        Self::save_default_agent_at(&Self::path(), name)
    }

    /// [`save_default_agent`](Self::save_default_agent) against an explicit
    /// path (tests).
    pub fn save_default_agent_at(path: &Path, name: Option<&str>) -> std::io::Result<()> {
        edit_config(path, |doc| {
            let general = table(doc, "general");
            match name {
                Some(name) => general["default_agent"] = toml_edit::value(name),
                // Unpinning drops the key rather than writing an empty string,
                // so the file goes back to looking like one that never set it.
                None => {
                    if let Some(t) = general.as_table_like_mut() {
                        t.remove("default_agent");
                    }
                }
            }
        })
    }

    /// Write the rail geometry back into `[ui]`, preserving every other key and
    /// comment in the file. The counterpart read is
    /// [`crate::chrome::geom_from_config`].
    pub fn save_ui(geom: RailGeom) -> std::io::Result<()> {
        Self::save_ui_at(&Self::path(), geom)
    }

    /// [`save_ui`](Self::save_ui) against an explicit path (tests).
    pub fn save_ui_at(path: &Path, geom: RailGeom) -> std::io::Result<()> {
        edit_config(path, |doc| {
            let ui = table(doc, "ui");
            ui["left_rail"] = toml_edit::value(geom.left_w as i64);
            ui["right_rail"] = toml_edit::value(geom.right_w as i64);
            // A section with no pinned height sizes itself to the terminal, and
            // that is a real state rather than a missing one — so clearing it
            // removes the key instead of writing a number back.
            for (key, value) in [("procs_height", geom.procs_h), ("system_height", geom.system_h)] {
                match value {
                    Some(h) => ui[key] = toml_edit::value(h as i64),
                    None => {
                        if let Some(t) = ui.as_table_like_mut() {
                            t.remove(key);
                        }
                    }
                }
            }
        })
    }

    /// Select a palette: `[theme] name`.
    ///
    /// Only the name. The role overrides beside it in that table are hand
    /// written and stay that way — the SETTINGS page picks a theme, and a page
    /// that also rewrote `accent = "#ff8800"` would be silently discarding
    /// something the file's owner typed on purpose.
    pub fn save_theme_name(name: &str) -> std::io::Result<()> {
        Self::save_theme_name_at(&Self::path(), name)
    }

    /// [`save_theme_name`](Self::save_theme_name) against an explicit path.
    pub fn save_theme_name_at(path: &Path, name: &str) -> std::io::Result<()> {
        edit_config(path, |doc| {
            table(doc, "theme")["name"] = toml_edit::value(name);
        })
    }

    /// Whether a `butai` run over ssh inside a pane may pull its machine in.
    pub fn save_remote_auto_attach(on: bool) -> std::io::Result<()> {
        Self::save_remote_auto_attach_at(&Self::path(), on)
    }

    /// [`save_remote_auto_attach`](Self::save_remote_auto_attach) against an
    /// explicit path.
    pub fn save_remote_auto_attach_at(path: &Path, on: bool) -> std::io::Result<()> {
        edit_config(path, |doc| {
            table(doc, "general")["remote_auto_attach"] = toml_edit::value(on);
        })
    }

    /// Whether URLs are marked up as hyperlinks for the terminal butai draws on.
    pub fn save_links(on: bool) -> std::io::Result<()> {
        Self::save_links_at(&Self::path(), on)
    }

    /// [`save_links`](Self::save_links) against an explicit path (tests).
    pub fn save_links_at(path: &Path, on: bool) -> std::io::Result<()> {
        edit_config(path, |doc| {
            table(doc, "ui")["links"] = toml_edit::value(on);
        })
    }

    /// Remember that a release was offered and turned down.
    ///
    /// The prompt has two answers and this is what the second one means: not
    /// "later" but "not this one". `esc` dismisses the box without coming here,
    /// which is the way to be asked again next launch.
    pub fn save_declined_version(version: &str) -> std::io::Result<()> {
        Self::save_declined_version_at(&Self::path(), version)
    }

    /// [`save_declined_version`](Self::save_declined_version) against an
    /// explicit path (tests).
    pub fn save_declined_version_at(path: &Path, version: &str) -> std::io::Result<()> {
        edit_config(path, |doc| {
            table(doc, "update")["declined_version"] = toml_edit::value(version);
        })
    }

    /// Whether to look for a newer release at all: `[update] check`.
    pub fn save_update_check(on: bool) -> std::io::Result<()> {
        Self::save_update_check_at(&Self::path(), on)
    }

    /// [`save_update_check`](Self::save_update_check) against an explicit path.
    pub fn save_update_check_at(path: &Path, on: bool) -> std::io::Result<()> {
        edit_config(path, |doc| {
            table(doc, "update")["check"] = toml_edit::value(on);
        })
    }

    /// Remember a machine connected from `[+ host]`, as a `[[remote]]` block.
    ///
    /// **Only a deliberate connection is written.** A machine that announced
    /// itself from inside a pane is adopted for the session and left out of the
    /// file: a week of `ssh`-ing around would otherwise turn every morning into
    /// a startup that dials nine machines and waits on the seven that are
    /// asleep. Typing `[+ host]` is the act that says "this one is mine".
    ///
    /// Idempotent by `host`, because reconnecting the same machine on Tuesday
    /// must not leave two blocks behind — and re-picking a host you already have
    /// is the ordinary way to recover from a dropped tunnel.
    pub fn save_remote(name: Option<&str>, host: &str, ssh_args: &[String]) -> std::io::Result<()> {
        Self::save_remote_at(&Self::path(), name, host, ssh_args)
    }

    /// [`save_remote`](Self::save_remote) against an explicit path (tests).
    pub fn save_remote_at(
        path: &Path,
        name: Option<&str>,
        host: &str,
        ssh_args: &[String],
    ) -> std::io::Result<()> {
        edit_config(path, |doc| {
            let remotes = array_of_tables(doc, "remote");
            if remotes.iter().any(|t| t.get("host").and_then(|v| v.as_str()) == Some(host)) {
                return;
            }
            let mut t = toml_edit::Table::new();
            t["host"] = toml_edit::value(host);
            if let Some(name) = name.filter(|n| *n != host) {
                t["name"] = toml_edit::value(name);
            }
            if !ssh_args.is_empty() {
                let mut arr = toml_edit::Array::new();
                for a in ssh_args {
                    arr.push(a.as_str());
                }
                t["ssh_args"] = toml_edit::value(arr);
            }
            remotes.push(t);
        })
    }

    /// Drop a remembered machine, named by the badge its tabs carry.
    ///
    /// **The other half of [`save_remote`](Self::save_remote), and disconnecting
    /// is what calls it.** Without this, `[+ host]` wrote a block and nothing
    /// ever took one out: a machine you disconnected was gone until you
    /// detached, and back in the tab bar the moment you attached again, because
    /// the block that dialled it every morning was still in the file.
    ///
    /// The badge is what a disconnect has to hand — the tab bar is about
    /// machines, not about `[[remote]]` blocks — so it is matched the way it was
    /// derived: a block's badge is its `name`, or its destination when it has
    /// none. See [`names_machine`].
    ///
    /// Returns whether a block actually went, so the caller can say so. Silent
    /// when it was never remembered — forgetting a host adopted for the session
    /// is a no-op, not an error, because the caller cannot tell the two apart
    /// and should not have to.
    pub fn forget_remote(host: &str) -> std::io::Result<bool> {
        Self::forget_remote_at(&Self::path(), host)
    }

    /// [`forget_remote`](Self::forget_remote) against an explicit path (tests).
    pub fn forget_remote_at(path: &Path, host: &str) -> std::io::Result<bool> {
        // Nothing to forget: leave the file alone rather than rewriting it. Most
        // disconnects are of a machine that announced itself and was never
        // written down, and those must not touch — or, on a fresh install,
        // create — a config file.
        let remembered = Self::load_from(path)
            .0
            .remote
            .iter()
            .any(|r| names_machine(r.name.as_deref(), r.host.as_deref(), host));
        if !remembered {
            return Ok(false);
        }
        edit_config(path, |doc| {
            array_of_tables(doc, "remote")
                .retain(|t| !names_machine(str_at(t, "name"), str_at(t, "host"), host));
        })?;
        Ok(true)
    }
}

/// Apply `edit` to the TOML document at `path` and write it back, preserving
/// every key and comment `edit` did not touch. The write is atomic (temp file +
/// rename in the same directory), so a crash mid-save cannot truncate a config.
fn edit_config(path: &Path, edit: impl FnOnce(&mut toml_edit::DocumentMut)) -> std::io::Result<()> {
    // The config file is often absent on a fresh install, and so is its parent.
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{e}")))?;
    edit(&mut doc);

    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, doc.to_string())?;
    std::fs::rename(&tmp, path)
}

/// Whether a `[[remote]]` block is the machine `badge` names.
///
/// The exact inverse of how the badge was derived — [`crate::remotes`] labels a
/// block with its `name`, falling back to its destination — so a machine
/// disconnected by the name on its tabs finds the block that dialled it. The
/// destination matches too, which costs nothing and covers the blocks `[+ host]`
/// writes, since those carry no `name` at all.
///
/// A block with no `host` is a `[[remote]] socket`: somebody else's forward,
/// which the client cannot drop and so must never forget.
fn names_machine(name: Option<&str>, host: Option<&str>, badge: &str) -> bool {
    let Some(host) = host else { return false };
    host == badge || name == Some(badge)
}

/// A string-valued key of a TOML table, if it is one.
fn str_at<'t>(table: &'t toml_edit::Table, key: &str) -> Option<&'t str> {
    table.get(key).and_then(|v| v.as_str())
}

/// The named array-of-tables (`[[remote]]`), created if absent.
///
/// Same defensive shape as [`table`]: a hand-edited `remote = 5` is replaced
/// rather than indexed into, which would panic on a file the user is allowed to
/// get wrong.
fn array_of_tables<'d>(
    doc: &'d mut toml_edit::DocumentMut,
    key: &str,
) -> &'d mut toml_edit::ArrayOfTables {
    if !doc.get(key).is_some_and(|it| it.is_array_of_tables()) {
        doc[key] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
    }
    doc[key].as_array_of_tables_mut().expect("just created")
}

/// The named table, created (as a real `[table]`, not inline) if absent. A key
/// that is not a table at all (a hand-edited `ui = 5`) is replaced rather than
/// indexed into, which would panic.
fn table<'d>(doc: &'d mut toml_edit::DocumentMut, key: &str) -> &'d mut toml_edit::Item {
    if !doc.get(key).is_some_and(|it| it.is_table_like()) {
        doc[key] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    &mut doc[key]
}

/// Parse `#rrggbb` into an RGB triple; `None` on malformed input.
pub fn parse_hex_color(s: &str) -> Option<(u8, u8, u8)> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 {
        return None;
    }
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The split's load-bearing invariant: one file, two structs, and each
    /// silently ignores the other's tables. If either side ever grew
    /// `deny_unknown_fields`, every real config would fail to load on that side
    /// — so this feeds a config with the *daemon's* half in it and checks that
    /// the client's own keys still arrive.
    #[test]
    fn the_daemons_half_of_the_file_parses_away() {
        let text = r##"
            [general]
            prefix = "C-a"
            default_shell = "fish"
            scrollback = 9000
            restore_bytes = 1024

            [api]
            websocket_port = 8080

            [[agents]]
            name = "claude"
            command = "claude"

            [theme]
            name = "tokyonight"

            [ui]
            left_rail = 30
        "##;
        let cfg: Config =
            toml::from_str(text).expect("the daemon's tables must not fail the parse");
        assert_eq!(cfg.general.prefix, "C-a");
        assert_eq!(cfg.theme.name, "tokyonight");
        assert_eq!(cfg.ui.left_rail, Some(30));
        // ...and the daemon's keys did not leak in as theme role overrides,
        // which is what `#[serde(flatten)]` on `Theme::colors` would do if the
        // tables were not properly separate.
        assert!(cfg.theme.role_overrides().is_empty(), "{:?}", cfg.theme.role_overrides());
    }

    #[test]
    fn theme_defaults_to_the_default_palette() {
        let cfg = Config::default();
        assert_eq!(cfg.theme.name, DEFAULT_THEME);
        assert!(cfg.theme.colors.is_empty(), "no overrides unless asked for");
        assert_eq!(cfg.theme.syntax_theme, "base16-ocean.dark");
    }

    #[test]
    fn theme_name_selects_and_extra_keys_become_overrides() {
        let text = "[theme]\nname = \"terminal\"\naccent = \"#ff8800\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.theme.name, "terminal");
        assert_eq!(cfg.theme.role_overrides(), vec![("accent", "#ff8800")]);
    }

    /// A config written against the pre-theme four-color table still resolves,
    /// with `border`/`border_focused` mapped onto the roles that replaced them.
    #[test]
    fn legacy_theme_keys_map_to_roles() {
        let text = concat!(
            "[theme]\n",
            "border = \"#101010\"\n",
            "border_focused = \"#ff0000\"\n",
            "status_bg = \"#202020\"\n",
            "status_fg = \"#f0f0f0\"\n",
        );
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.theme.name, DEFAULT_THEME, "unnamed = default");
        assert_eq!(
            cfg.theme.role_overrides(),
            vec![
                ("rule", "#101010"),
                ("rule_focus", "#ff0000"),
                ("status_bg", "#202020"),
                ("status_fg", "#f0f0f0"),
            ]
        );
    }

    /// `syntax_theme` is a syntect theme name, not a role — it must not leak
    /// into the overrides and warn as an unknown role.
    #[test]
    fn syntax_theme_is_not_a_role_override() {
        let text = "[theme]\nsyntax_theme = \"InspiredGitHub\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert_eq!(cfg.theme.syntax_theme, "InspiredGitHub");
        assert!(cfg.theme.role_overrides().is_empty());
    }

    #[test]
    fn hex_colors() {
        assert_eq!(parse_hex_color("#7aa2f7"), Some((0x7a, 0xa2, 0xf7)));
        assert_eq!(parse_hex_color("7aa2f7"), None);
        assert_eq!(parse_hex_color("#zzz"), None);
    }

    #[test]
    fn the_update_table_defaults_to_checking_and_nothing_declined() {
        let cfg = Config::default();
        assert!(cfg.update.check, "an update you are not told about is one you do not get");
        assert_eq!(cfg.update.declined_version, None);
        assert!(!cfg.update.declined("1.1.0"));
    }

    #[test]
    fn a_declined_version_only_silences_that_version() {
        let text = "[update]\ncheck = true\ndeclined_version = \"1.1.0\"\n";
        let cfg: Config = toml::from_str(text).unwrap();
        assert!(cfg.update.declined("1.1.0"));
        // The whole point of storing the version rather than a bool: the next
        // release has to ask once of its own.
        assert!(!cfg.update.declined("1.2.0"));
        assert!(!cfg.update.declined("1.0.9"));
    }

    #[test]
    fn save_declined_version_preserves_other_content() {
        let dir = std::env::temp_dir().join(format!("butai-save-decline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# my setup\n[general]\nprefix = \"C-a\"\n\n[ui]\nleft_rail = 30\n")
            .unwrap();

        Config::save_declined_version_at(&path, "1.1.0").unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my setup"), "{text}");
        let (cfg, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty());
        assert_eq!(cfg.general.prefix, "C-a");
        assert_eq!(cfg.ui.left_rail, Some(30));
        assert!(cfg.update.declined("1.1.0"));
        // Still checking — turning one release down is not turning the feature
        // off, and the two keys have to stay independent.
        assert!(cfg.update.check);

        // Declining a later release replaces the key rather than adding a second.
        Config::save_declined_version_at(&path, "1.2.0").unwrap();
        let (cfg2, _) = Config::load_from(&path);
        assert!(cfg2.update.declined("1.2.0"));
        assert!(!cfg2.update.declined("1.1.0"));
        let text2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text2.matches("declined_version").count(), 1);

        // And the two writers do not tread on each other.
        Config::save_update_check_at(&path, false).unwrap();
        let (cfg3, _) = Config::load_from(&path);
        assert!(!cfg3.update.check);
        assert!(cfg3.update.declined("1.2.0"));

        // A hand-edited `update` that is not a table is replaced, not indexed
        // into — the same defence every other writer has.
        std::fs::write(&path, "update = 5\n").unwrap();
        Config::save_declined_version_at(&path, "1.3.0").unwrap();
        let (cfg4, _) = Config::load_from(&path);
        assert!(cfg4.update.declined("1.3.0"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_ui_preserves_other_content() {
        let dir = std::env::temp_dir().join(format!("butai-save-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my setup\n[general]\nprefix = \"C-a\"\n\n[theme]\nborder = \"#101010\"\n",
        )
        .unwrap();

        let geom = RailGeom { left_w: 30, right_w: 44, procs_h: Some(10), system_h: Some(6) };
        Config::save_ui_at(&path, geom).unwrap();

        // Existing keys/comments survive, and the geometry round-trips on reload.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my setup"));
        assert!(text.contains("#101010"));
        let (cfg, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty());
        assert_eq!(cfg.general.prefix, "C-a");
        assert_eq!(cfg.theme.colors.get("border").map(String::as_str), Some("#101010"));
        assert_eq!(cfg.ui.left_rail, Some(30));
        assert_eq!(cfg.ui.right_rail, Some(44));
        assert_eq!(cfg.ui.procs_height, Some(10));
        assert_eq!(cfg.ui.system_height, Some(6));

        // A second save overwrites the same keys rather than duplicating them.
        Config::save_ui_at(&path, RailGeom { left_w: 20, right_w: 40, procs_h: Some(14), ..geom })
            .unwrap();
        let (cfg2, _) = Config::load_from(&path);
        assert_eq!(cfg2.ui.left_rail, Some(20));
        assert_eq!(cfg2.ui.right_rail, Some(40));
        assert_eq!(cfg2.ui.procs_height, Some(14));
        let text2 = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text2.matches("procs_height").count(), 1);

        // A hand-edited `ui` that is not a table is replaced, not indexed into.
        std::fs::write(&path, "ui = 5\n").unwrap();
        Config::save_ui_at(&path, RailGeom { left_w: 24, right_w: 36, ..geom }).unwrap();
        let (cfg3, _) = Config::load_from(&path);
        assert_eq!(cfg3.ui.left_rail, Some(24));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `[ui] net` takes a word or a list, and the untagged enum has to tell them
    /// apart without help. The default matters as much as the parse: an
    /// unconfigured rail shows every real link, which is what makes the setting
    /// something you reach for rather than something you need.
    #[test]
    fn the_net_key_takes_a_mode_or_a_list() {
        let parse = |s: &str| toml::from_str::<UiConfig>(s).map(|u| u.net);
        assert_eq!(parse("").unwrap(), NetSelect::Mode(NetMode::All), "the default");
        assert_eq!(parse("net = \"all\"").unwrap(), NetSelect::Mode(NetMode::All));
        assert_eq!(parse("net = \"auto\"").unwrap(), NetSelect::Mode(NetMode::Auto));
        assert_eq!(
            parse("net = [\"enp1s0\", \"vpn-tunnel\"]").unwrap(),
            NetSelect::Named(vec!["enp1s0".into(), "vpn-tunnel".into()])
        );
        // An empty list is "draw none", which is a thing someone may want, and
        // is not the same as leaving the key out.
        assert_eq!(parse("net = []").unwrap(), NetSelect::Named(vec![]));
        // A word that is neither mode is an error rather than a silent "all":
        // it is a typo, and the load path turns it into a visible warning.
        assert!(parse("net = \"eth0\"").is_err(), "a bare unknown word should not parse");
    }

    /// `[ui] disks` is the same shape as `[ui] net` and has to stay that way:
    /// two keys that mean "which of these does the rail draw" and disagree about
    /// how to say it is a manual with two answers to one question.
    #[test]
    fn the_disks_key_takes_a_mode_or_a_list() {
        let parse = |s: &str| toml::from_str::<UiConfig>(s).map(|u| u.disks);
        assert_eq!(parse("").unwrap(), DiskSelect::Mode(DiskMode::All), "the default");
        assert_eq!(parse("disks = \"all\"").unwrap(), DiskSelect::Mode(DiskMode::All));
        assert_eq!(parse("disks = \"auto\"").unwrap(), DiskSelect::Mode(DiskMode::Auto));
        assert_eq!(
            parse("disks = [\"/\", \"/media/fast\"]").unwrap(),
            DiskSelect::Named(vec!["/".into(), "/media/fast".into()])
        );
        // "Draw none", for the rail that would rather spend the rows on agents.
        assert_eq!(parse("disks = []").unwrap(), DiskSelect::Named(vec![]));
        assert!(parse("disks = \"root\"").is_err(), "a bare unknown word should not parse");
    }

    /// A machine connected from `[+ host]` comes back on the next start.
    ///
    /// The whole point of the feature: before this, a dialled host lived in the
    /// session's `hosts` vector and nowhere else, so quitting forgot it and only
    /// hand-written `[[remote]]` blocks survived.
    #[test]
    fn a_remembered_host_comes_back_as_a_remote_block() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "a_remembered_host_come",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# my setup\n[general]\nprefix = \"C-a\"\n").unwrap();

        Config::save_remote_at(&path, None, "gpu-box", &[]).unwrap();
        let cfg = Config::load_from(&path).0;
        assert_eq!(cfg.remote.len(), 1);
        assert_eq!(cfg.remote[0].host.as_deref(), Some("gpu-box"));

        // And it did not tread on what was already there.
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my setup"), "{text}");
        assert!(text.contains("C-a"), "{text}");
    }

    /// Reconnecting a machine you already have must not leave two blocks
    /// behind — and re-picking a host is the ordinary way to recover from a
    /// tunnel that dropped, so this is the common path, not an edge case.
    #[test]
    fn remembering_the_same_host_twice_writes_one_block() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "remembering_the_same_h",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        Config::save_remote_at(&path, None, "gpu-box", &[]).unwrap();
        Config::save_remote_at(&path, None, "gpu-box", &[]).unwrap();
        Config::save_remote_at(&path, None, "pi-farm", &[]).unwrap();

        let cfg = Config::load_from(&path).0;
        let hosts: Vec<&str> = cfg.remote.iter().filter_map(|r| r.host.as_deref()).collect();
        assert_eq!(hosts, vec!["gpu-box", "pi-farm"]);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("gpu-box").count(), 1, "duplicated: {text}");
    }

    /// A name and ssh flags survive, because they are what make the machine
    /// reachable and what the tab badge reads.
    #[test]
    fn a_remembered_host_keeps_its_name_and_ssh_args() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "a_remembered_host_keep",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        Config::save_remote_at(&path, Some("gpu"), "gpu-box", &["-p".into(), "2222".into()])
            .unwrap();

        let cfg = Config::load_from(&path).0;
        assert_eq!(cfg.remote[0].name.as_deref(), Some("gpu"));
        assert_eq!(cfg.remote[0].ssh_args, vec!["-p".to_string(), "2222".to_string()]);

        // A name identical to the destination is noise, so it is not written.
        let plain = dir.join("plain.toml");
        Config::save_remote_at(&plain, Some("gpu-box"), "gpu-box", &[]).unwrap();
        assert!(Config::load_from(&plain).0.remote[0].name.is_none());
    }

    /// Forgetting drops that block and leaves the others alone.
    #[test]
    fn forgetting_a_host_removes_only_its_block() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "forgetting_a_host_remo",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        Config::save_remote_at(&path, None, "gpu-box", &[]).unwrap();
        Config::save_remote_at(&path, None, "pi-farm", &[]).unwrap();

        assert!(Config::forget_remote_at(&path, "gpu-box").unwrap(), "a block went");
        let hosts: Vec<String> =
            Config::load_from(&path).0.remote.iter().filter_map(|r| r.host.clone()).collect();
        assert_eq!(hosts, vec!["pi-farm".to_string()]);

        // Forgetting one that was never remembered is a no-op, not an error:
        // the caller cannot tell a session-adopted host from a saved one.
        assert!(!Config::forget_remote_at(&path, "never-seen").unwrap(), "nothing to forget");
        assert_eq!(Config::load_from(&path).0.remote.len(), 1);
    }

    /// The bug this pair exists to close: a disconnect that does not outlive a
    /// detach.
    ///
    /// `[+ host]` writes a block, the block is dialled on every attach, and the
    /// disconnect that was supposed to end that is handed a *badge* — the name
    /// on the tabs — not a destination. A hand-written block renames itself with
    /// `name`, so matching only `host` left it in the file and the machine was
    /// back in the tab bar on the next attach, which is what "I disconnected it
    /// and it came back" was.
    #[test]
    fn a_disconnected_machine_is_forgotten_by_the_badge_its_tabs_carry() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "a_disconnected_machine",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            concat!(
                "[[remote]]\n",
                "host = \"gpu-box\"\n",
                "name = \"gpu\"\n",
                "\n",
                "[[remote]]\n",
                "host = \"pi-farm\"\n",
            ),
        )
        .unwrap();

        // The badge, which is all a disconnect knows.
        assert!(Config::forget_remote_at(&path, "gpu").unwrap());
        let hosts: Vec<String> =
            Config::load_from(&path).0.remote.iter().filter_map(|r| r.host.clone()).collect();
        assert_eq!(hosts, vec!["pi-farm".to_string()], "the renamed block stayed behind");

        // And the unnamed one still answers to its destination, which is the
        // badge it carries and the only name `[+ host]` ever writes.
        assert!(Config::forget_remote_at(&path, "pi-farm").unwrap());
        assert!(Config::load_from(&path).0.remote.is_empty());
    }

    /// A `[[remote]] socket` block is somebody else's forward. The client
    /// refuses to disconnect one — it has no ssh under it to kill — so nothing
    /// may forget it either, however its badge happens to read.
    #[test]
    fn a_socket_block_is_never_forgotten() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "a_socket_block_is_neve",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "[[remote]]\nsocket = \"/tmp/gpu-box.sock\"\nname = \"gpu-box\"\n")
            .unwrap();

        assert!(!Config::forget_remote_at(&path, "gpu-box").unwrap());
        assert_eq!(Config::load_from(&path).0.remote.len(), 1);
    }

    /// Disconnecting a machine that announced itself must not write a config
    /// file to a machine that has never had one — it was never remembered, so
    /// there is nothing to forget and nothing to write.
    #[test]
    fn forgetting_a_machine_that_was_never_remembered_touches_nothing() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "forgetting_a_machine_t",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");

        assert!(!Config::forget_remote_at(&path, "build-box").unwrap());
        assert!(!path.exists(), "a no-op must not create a config file");
    }

    /// A hand-edited `remote = 5` must not panic the writer. The config is the
    /// user's file and they are allowed to get it wrong.
    #[test]
    fn a_nonsense_remote_key_is_replaced_rather_than_indexed_into() {
        let dir = std::env::temp_dir().join(format!(
            "butai-{}-{}",
            "a_nonsense_remote_key_",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "remote = 5\n").unwrap();
        Config::save_remote_at(&path, None, "gpu-box", &[]).unwrap();
        assert_eq!(Config::load_from(&path).0.remote[0].host.as_deref(), Some("gpu-box"));
    }

    /// Pinning a default agent writes one key into `[general]` and unpinning
    /// takes it back out, both without disturbing the settings around it.
    #[test]
    fn save_default_agent_round_trips() {
        let dir = std::env::temp_dir().join(format!("butai-save-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# my setup\n[general]\nprefix = \"C-a\"\nscrollback = 9000\n\n[ui]\nleft_rail = 30\n",
        )
        .unwrap();

        Config::save_default_agent_at(&path, Some("codex")).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my setup"));
        let (cfg, warnings) = Config::load_from(&path);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cfg.general.default_agent.as_deref(), Some("codex"));
        assert_eq!(cfg.general.prefix, "C-a", "neighbouring keys untouched");
        assert_eq!(cfg.ui.left_rail, Some(30), "unrelated tables untouched");
        // `scrollback` is the *daemon's* key in the same table, and this side
        // cannot even see it — which is exactly why the write has to be
        // surgical. Asserted against the file text, since the struct has no
        // field for it.
        assert!(text.contains("scrollback = 9000"), "the daemon's key was lost:\n{text}");

        // Repinning replaces the key rather than appending a second one.
        Config::save_default_agent_at(&path, Some("gemini")).unwrap();
        let (cfg2, _) = Config::load_from(&path);
        assert_eq!(cfg2.general.default_agent.as_deref(), Some("gemini"));

        // Unpinning removes it outright, leaving a file that looks like one
        // that never set it — not `default_agent = ""`.
        Config::save_default_agent_at(&path, None).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("default_agent"), "{text}");
        let (cfg3, _) = Config::load_from(&path);
        assert_eq!(cfg3.general.default_agent, None);
        assert_eq!(cfg3.general.prefix, "C-a");

        // A fresh install has no config file at all: the first pin writes one.
        let fresh = dir.join("nested/config.toml");
        Config::save_default_agent_at(&fresh, Some("claude")).unwrap();
        let (cfg4, _) = Config::load_from(&fresh);
        assert_eq!(cfg4.general.default_agent.as_deref(), Some("claude"));

        std::fs::remove_dir_all(&dir).ok();
    }
}
