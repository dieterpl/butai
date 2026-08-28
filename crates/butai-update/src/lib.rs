//! Updating butai in place, from the GitHub release it was installed from.
//!
//! The workbench asks once at launch — *"butai 1.1.0 is available, update?"* —
//! and this crate is everything behind that question: what the latest release
//! is, whether it is newer than this build, which of the seven published
//! artifacts belongs to this machine, and how to get from a downloaded tarball
//! to a running butai on the new build without losing the session.
//!
//! A crate rather than a module in `butai-client`, because both ends of the
//! socket need it. A daemon answering `POST /v1/update` has to reach the same
//! four steps, and taking them from the client would drag ratatui, crossterm,
//! arboard and png into the daemon to get them. It depends on nothing else in
//! the workspace, and holds the only outbound network in the tree.
//!
//! ## Why the target triple is compiled in
//!
//! A release publishes `butai-<version>-<triple>.tar.gz`, one per target.
//! [`scripts/install.sh`] has to guess which one a machine wants — `uname` for
//! the arch, `ldd --version | grep musl` to tell musl from glibc — because it
//! runs before any butai exists. This does not: `build.rs` reads the triple
//! cargo is compiling for and bakes it in as [`TARGET`]. A musl build asks for
//! the musl tarball because it *is* the musl build. There is no fallback to a
//! near match: a release with nothing for this triple is reported, never
//! approximated, because the failure mode of guessing wrong is a binary that
//! does not exec.
//!
//! ## The order the swap happens in
//!
//! The daemon is stopped *before* [`swap`] replaces the binary, and that is not
//! arbitrary. A daemon is spawned by way of `std::env::current_exe()`, and on
//! Linux, once a running executable has been renamed over, `/proc/self/exe`
//! reads `".../butai (deleted)"` — a path that does not exist. Anything that
//! spawned a daemon between the swap and the exec would fail on it. So: stop,
//! wait for it to actually be gone, [`swap`], [`restart`] an explicitly
//! resolved path.
//!
//! Stopping it is the one step this crate does *not* do, because there are two
//! answers and they do not share code. A client stops a daemon by asking it to
//! (`butai_client::update::apply`); a daemon updating itself cannot ask itself,
//! and stops by falling out of its own event loop. Both end here, at [`swap`]
//! and [`restart`].
//!
//! Nothing here is destructive at any step. `kill-server` snapshots the open
//! workspaces and every pane's output before it tears anything down, so a
//! failure after that point costs a restart and no work: the next plain `butai`
//! brings the old build back up with the session restored.
//!
//! [`scripts/install.sh`]: https://github.com/dieterpl/butai/blob/main/scripts/install.sh

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};

/// The repository releases are published from.
const REPO: &str = "dieterpl/butai";

/// The binary's name, inside the tarball and on disk.
const BIN: &str = "butai";

/// The target triple this build is for, from `build.rs`.
///
/// One of the seven in `.github/workflows/release.yml`, for a build that came
/// from the release matrix — and something else entirely for a build from
/// source on a platform butai does not publish, which is why [`check`] reports
/// a missing artifact rather than treating it as an error.
pub const TARGET: &str = env!("BUTAI_TARGET");

/// The version this build is.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Opt out of the check entirely, for a packaged butai whose updates arrive
/// through a package manager. Same shape as `BUTAI_NO_HANDOFF`: any non-empty
/// value other than `0`.
const NO_CHECK_ENV: &str = "BUTAI_NO_UPDATE_CHECK";

/// Nothing published is anywhere near this. It exists so a wrong URL cannot
/// stream into memory until the machine gives up.
const MAX_DOWNLOAD: u64 = 128 * 1024 * 1024;

/// A newer release, and everything needed to fetch the part of it this machine
/// wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The release version, without the tag's leading `v`.
    pub version: String,
    /// The artifact for [`TARGET`], which is also its key in `SHA256SUMS`.
    pub asset: String,
    /// Where to fetch that artifact.
    pub url: String,
    /// Where to fetch the checksums, when the release publishes them. Releases
    /// cut before `release.yml` existed do not, which `install.sh` also
    /// tolerates.
    pub sums_url: Option<String>,
    /// The binary this would replace, already resolved and known writable.
    pub install: PathBuf,
}

/// A verified new binary, sitting beside the one it is about to replace.
///
/// Downloaded, checksummed and unpacked; nothing has been swapped yet. It is
/// written into the install directory rather than a temp dir because the swap
/// is a `rename`, and `rename` does not cross filesystems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    pub version: String,
    tmp: PathBuf,
    install: PathBuf,
}

impl Staged {
    pub fn install_path(&self) -> &Path {
        &self.install
    }
}

impl Drop for Staged {
    /// A staged binary that was never applied is just a file in somebody's
    /// `~/.local/bin`. Dropping it cleans up; [`swap`] renames it away first,
    /// so the successful path finds nothing to remove.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.tmp);
    }
}

// ── deciding whether to look ─────────────────────────────────────────────

/// Which releases an install follows: `[update] channel`.
///
/// The two tracks are already separate on the publishing side — a tag with a
/// prerelease identifier is cut from `develop` and published with
/// `--prerelease`, a bare one is cut from `main` — and GitHub keeps a
/// prerelease out of `releases/latest`. That is what makes [`Stable`] a single
/// request with no filter of its own, and it is also why a stable install can
/// only reach a dev build by being told its tag.
///
/// Both halves read it, because both check: the client for the binary a person
/// runs, the daemon for `POST /v1/update` on a machine nobody is sitting at.
///
/// [`Stable`]: Channel::Stable
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// Stable releases only — every tag cut on `main`, and the default.
    #[default]
    Stable,
    /// Prereleases as well: the `-dev.N` tags cut on `develop`, and the stable
    /// releases they lead to, whichever of the two is highest.
    Dev,
}

impl Channel {
    /// The word this channel is written and read as.
    ///
    /// One function for both directions with [`from_name`], because the config
    /// is written by SETTINGS and read by serde: a name written here that serde
    /// would not take is a setting that saves and then cannot be loaded, and
    /// the test at the bottom of this file is what keeps the two agreeing.
    ///
    /// [`from_name`]: Channel::from_name
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
        }
    }

    /// The channel a word means, or `None` for one that is neither.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "stable" => Some(Self::Stable),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }

    /// Both channels, in the order SETTINGS offers them.
    pub const ALL: [Channel; 2] = [Channel::Stable, Channel::Dev];
}

/// Whether the launch check should run at all.
///
/// The declined version is checked later, against what the release actually
/// turns out to be — answering "no" to 1.1.0 should not also silence 1.2.0.
pub fn enabled(check: bool) -> bool {
    if !check {
        return false;
    }
    !std::env::var_os(NO_CHECK_ENV).is_some_and(|v| !v.is_empty() && v != "0")
}

/// Is `latest` a higher version than `mine`?
///
/// Semver's ordering over the two parts a butai tag has: three integers, then
/// the prerelease identifier the dev track carries. Numerically on the
/// integers, because `1.0.10` is newer than `1.0.9` and a string comparison
/// says the opposite. Anything that does not parse as `X.Y.Z` answers `false`:
/// an unrecognised tag is a reason to say nothing, not a reason to guess.
///
/// **The prerelease half is what makes [`Channel::Dev`] possible.** This used
/// to cut the suffix and compare, so `1.3.0-dev.2` and `1.3.0-dev.1` were the
/// same version and a build on the dev track could never be offered the next
/// one — only the stable release it was already ahead of.
pub fn newer(latest: &str, mine: &str) -> bool {
    match (parse(latest), parse(mine)) {
        (Some(l), Some(m)) => l > m,
        _ => false,
    }
}

/// A version, in the parts that order it.
///
/// `Ord` is derived, so the field order *is* the comparison order: the triple
/// decides, and the prerelease only breaks a tie between two tags of the same
/// release.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Version {
    core: (u64, u64, u64),
    pre: Pre,
}

/// The prerelease identifier of a version, ordered as semver orders it.
///
/// The variant order is the whole point and it is the reverse of the reading
/// order: `1.3.0-dev.4` is *below* the `1.3.0` it leads to, so the prerelease
/// variant is declared first and the derived `Ord` follows.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pre {
    /// `-dev.4`, split on its dots.
    Identifiers(Vec<Id>),
    /// No suffix at all — the release itself.
    Release,
}

/// One dot-separated piece of a prerelease identifier.
///
/// Numeric pieces compare as numbers and rank below alphanumeric ones, which
/// is again the declaration order. `dev.10` is ahead of `dev.9`; a string
/// comparison of the whole suffix says the opposite, and that is the bug this
/// type exists to not have.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Id {
    Numeric(u64),
    Alphanumeric(String),
}

/// `1.2.3-dev.4` -> the [`Version`] that orders it, or `None` for anything that
/// is not `X.Y.Z` with an optional suffix.
///
/// Build metadata (`+…`) is dropped rather than parsed: semver excludes it from
/// precedence, and butai publishes none.
fn parse(v: &str) -> Option<Version> {
    let v = v.trim().trim_start_matches('v').split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((core, pre)) => (core, Pre::Identifiers(pre.split('.').map(identifier).collect())),
        None => (v, Pre::Release),
    };
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    it.next().is_none().then_some(Version { core: (major, minor, patch), pre })
}

/// One piece of a prerelease identifier, numeric where it can be.
fn identifier(s: &str) -> Id {
    match s.parse() {
        Ok(n) => Id::Numeric(n),
        Err(_) => Id::Alphanumeric(s.to_string()),
    }
}

/// The artifact a release publishes for this build, e.g.
/// `butai-1.1.0-x86_64-unknown-linux-musl.tar.gz`.
///
/// The name is a contract with `.github/workflows/release.yml` and
/// `scripts/release.sh`, which both stage `butai-$version-$target` and tar it.
pub fn asset_name(version: &str) -> String {
    format!("{BIN}-{version}-{TARGET}.tar.gz")
}

// ── where the binary lives, and whether we may touch it ──────────────────

/// The binary this process is running, with symlinks resolved.
///
/// Canonicalised because a `~/.local/bin/butai` that is a symlink into a
/// versioned directory should be followed to the real file — renaming over the
/// symlink would silently replace the link instead of the program.
pub fn install_path() -> Result<PathBuf> {
    let exe = std::env::current_exe().context("locate the running butai")?;
    std::fs::canonicalize(&exe).with_context(|| format!("resolve {}", exe.display()))
}

/// The install path, if updating it in place is something this process can
/// actually do.
///
/// Checked *before* the question is asked. Finding out that `/usr/local/bin`
/// belongs to root after the tarball is downloaded and the daemon is already
/// stopped is the worst possible moment, and an update offer that cannot be
/// carried out is worse than no offer at all.
pub fn writable_install_path() -> Result<PathBuf> {
    let path = install_path()?;
    let dir = path.parent().unwrap_or(Path::new("/"));

    if in_cargo_target(&path) {
        bail!(
            "{} is a build in a cargo target directory — cargo owns it, so it is not \
             replaced from here",
            path.display()
        );
    }
    // The directory, not the file: the swap is a `rename` into it, which needs
    // write on the directory and says nothing about the mode of the old file.
    if rustix::fs::access(dir, rustix::fs::Access::WRITE_OK).is_err() {
        bail!(
            "butai is installed at {}, which this user cannot write — re-run \
             scripts/install.sh, or install somewhere writable with BUTAI_INSTALL_DIR",
            path.display()
        );
    }
    Ok(path)
}

/// Is this binary a cargo build rather than an installed one?
///
/// `cargo run` and `cargo build` own those files and rewrite them; replacing
/// one with a release download would be undone by the next build and confusing
/// until then.
///
/// Two ways of telling, because the obvious one is not enough. The layout
/// (`target/<profile>/butai`) covers a default build — but `CARGO_TARGET_DIR`
/// moves the whole thing somewhere that is not called `target` at all, which is
/// how this tree is usually built. What survives the move is the `CACHEDIR.TAG`
/// cargo writes at the root of a target directory: it is there to keep backup
/// tools out, and it is the only thing that identifies one wherever it was put.
fn in_cargo_target(path: &Path) -> bool {
    let mut parts = path.components().rev().skip(1).map(|c| c.as_os_str());
    let profile = parts.next();
    // `target/<profile>/butai`, and `target/<triple>/<profile>/butai` for a
    // cross build, which `scripts/release.sh` produces.
    let is_profile = matches!(profile.and_then(|p| p.to_str()), Some("debug" | "release"));
    if is_profile && parts.any(|c| c == "target") {
        return true;
    }
    // Bounded: a target root is one or two directories above the binary, and
    // walking to `/` would ask about every parent on the way.
    path.ancestors().skip(1).take(4).any(is_cargo_cache_dir)
}

/// Does this directory carry the tag cargo puts at the root of a target dir?
///
/// The Cache Directory Tagging Specification's magic string, which cargo writes
/// verbatim.
fn is_cargo_cache_dir(dir: &Path) -> bool {
    const SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";
    std::fs::read(dir.join("CACHEDIR.TAG")).is_ok_and(|b| b.starts_with(SIGNATURE))
}

// ── asking GitHub ────────────────────────────────────────────────────────

#[derive(serde::Deserialize)]
struct Release {
    tag_name: String,
    /// Unpublished, and therefore unreachable: a draft's assets 404 for
    /// anyone but its author. Only [`pick`] reads it, since `releases/latest`
    /// has already excluded drafts by the time the stable channel parses one.
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(serde::Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// How many releases [`Channel::Dev`] reads. GitHub answers newest-first, and
/// a page of thirty is more tags than either track has cut in a year.
const DEV_PAGE: u32 = 30;

/// The newest release on `channel`, if it is newer than this build.
///
/// Blocking — `ureq` is — so every caller runs it off the event loop. `Ok(None)`
/// means there is nothing to offer, which covers both "already current" and
/// "the check is switched off"; an `Err` is a real failure worth reporting to
/// somebody who asked for the check by hand, and worth swallowing on launch.
///
/// The two channels ask GitHub different questions. `releases/latest` is one
/// answer GitHub has already filtered — no drafts, no prereleases — which is
/// exactly the stable track and no filter to write here. The dev track has no
/// such endpoint, so it reads the list and chooses; see [`pick`].
pub fn check(channel: Channel) -> Result<Option<Offer>> {
    let install = writable_install_path()?;

    let release = match channel {
        Channel::Stable => {
            let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
            let body = get_string(&url).context("ask GitHub for the latest release")?;
            serde_json::from_str(&body).context("parse GitHub's answer as a release")?
        }
        Channel::Dev => {
            let url = format!("https://api.github.com/repos/{REPO}/releases?per_page={DEV_PAGE}");
            let body = get_string(&url).context("ask GitHub for the published releases")?;
            let releases: Vec<Release> =
                serde_json::from_str(&body).context("parse GitHub's answer as a release list")?;
            // A repository that has published nothing this build can compare
            // itself against. Nothing to offer, and nothing wrong either.
            let Some(release) = pick(releases) else { return Ok(None) };
            release
        }
    };

    offer(release, install)
}

/// The highest release in a list — the answer GitHub gives directly for the
/// stable track and not at all for the dev one.
///
/// **By version, never by position.** The list comes back in publish order, and
/// publish order is not version order: a stable `1.2.1` cut as a fix *after*
/// `1.3.0-dev.1` is the first entry, and taking the head would offer it as an
/// upgrade to a build already ahead of it — then offer it again every six
/// hours, forever.
///
/// Drafts are dropped because their assets are not public: an offer pointing at
/// one is an update that cannot be carried out. A tag that does not parse is
/// dropped for the reason [`newer`] gives — an unrecognised version is not
/// something to guess at.
fn pick(releases: Vec<Release>) -> Option<Release> {
    releases
        .into_iter()
        .filter(|r| !r.draft)
        .filter_map(|r| parse(&r.tag_name).map(|v| (v, r)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, r)| r)
}

/// What a release offers this build, or `None` when it is not ahead of it.
///
/// The tail both channels share: the same comparison, the same artifact lookup
/// and the same checksums, so the only thing a channel decides is which release
/// arrives here.
fn offer(release: Release, install: PathBuf) -> Result<Option<Offer>> {
    let version = release.tag_name.trim_start_matches('v').to_string();
    if !newer(&version, CURRENT) {
        return Ok(None);
    }

    let asset = asset_name(&version);
    let Some(found) = release.assets.iter().find(|a| a.name == asset) else {
        bail!("{} publishes no build for {TARGET}", release.tag_name);
    };

    Ok(Some(Offer {
        version,
        url: found.browser_download_url.clone(),
        sums_url: release
            .assets
            .iter()
            .find(|a| a.name == "SHA256SUMS")
            .map(|a| a.browser_download_url.clone()),
        asset,
        install,
    }))
}

// ── fetching it ──────────────────────────────────────────────────────────

/// Download, verify and unpack an [`Offer`], swapping nothing.
///
/// Split from [`apply`] so the network happens while the question is still on
/// screen: by the time somebody answers yes, the only things left are a rename
/// and an exec, neither of which can hang on a slow link with the terminal
/// already torn down.
pub fn stage(offer: &Offer) -> Result<Staged> {
    let tarball = get_bytes(&offer.url).with_context(|| format!("download {}", offer.asset))?;

    // A release from before `release.yml` existed publishes no checksums, and
    // `install.sh` installs from those anyway rather than refusing. The same
    // judgement here: verify what can be verified, and say so in the log when
    // there is nothing to verify against.
    match &offer.sums_url {
        Some(url) => {
            let sums = get_string(url).context("download SHA256SUMS")?;
            verify(&tarball, &sums, &offer.asset)?;
        }
        None => tracing::warn!("release {} publishes no SHA256SUMS", offer.version),
    }

    let binary = extract_binary(&tarball, &offer.asset)?;
    let tmp = stage_beside(&offer.install, &binary)?;

    Ok(Staged { version: offer.version.clone(), tmp, install: offer.install.clone() })
}

/// Check a download against the release's `SHA256SUMS`.
///
/// The file is `<hex>  <name>` per line, as `sha256sum` writes it. A name with
/// no line is a release that published a checksum file covering something else,
/// which is a mismatch worth stopping on rather than shrugging at — unlike a
/// release with no checksum file at all.
fn verify(bytes: &[u8], sums: &str, asset: &str) -> Result<()> {
    use sha2::{Digest, Sha256};

    let want = sums
        .lines()
        .filter_map(|l| l.split_once("  ").or_else(|| l.split_once(' ')))
        .find(|(_, name)| name.trim() == asset)
        .map(|(hex, _)| hex.trim().to_ascii_lowercase());
    let Some(want) = want else {
        bail!("SHA256SUMS publishes no checksum for {asset}");
    };

    let got = format!("{:x}", Sha256::digest(bytes));
    if got != want {
        bail!("checksum mismatch for {asset}\n  expected {want}\n  got      {got}");
    }
    Ok(())
}

/// Pull the `butai` executable out of a release tarball.
///
/// The archive holds a single top-level directory —
/// `butai-1.1.0-<triple>/{butai,README.md,LICENSE}` — so the entry is matched
/// on its file name rather than a full path that carries the version in it.
/// Nothing is unpacked to disk here, so a tarball with `../` in a path has
/// nowhere to escape to.
fn extract_binary(tarball: &[u8], asset: &str) -> Result<Vec<u8>> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tarball));
    for entry in archive.entries().context("read the tarball")? {
        let mut entry = entry.context("read a tarball entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let is_binary =
            entry.path().map(|p| p.file_name() == Some(std::ffi::OsStr::new(BIN))).unwrap_or(false);
        if !is_binary {
            continue;
        }
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).context("unpack the butai binary")?;
        if buf.is_empty() {
            bail!("the butai binary inside {asset} is empty");
        }
        return Ok(buf);
    }
    bail!("no `{BIN}` executable inside {asset}")
}

/// Write the new binary beside the one it will replace, executable.
///
/// Beside, rather than in `/tmp`, so the swap is a same-directory `rename`:
/// atomic, and impossible to fail halfway across a filesystem boundary with
/// the old binary already gone.
fn stage_beside(install: &Path, binary: &[u8]) -> Result<PathBuf> {
    use std::os::unix::fs::OpenOptionsExt;

    let dir = install.parent().unwrap_or(Path::new("."));
    let tmp = dir.join(format!(".{BIN}-update-{}", std::process::id()));
    // A leftover from a previous run that died between staging and the rename.
    let _ = std::fs::remove_file(&tmp);

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o755)
        .open(&tmp)
        .with_context(|| format!("create {}", tmp.display()))?;
    file.write_all(binary).with_context(|| format!("write {}", tmp.display()))?;
    // Before it is renamed over the program the machine boots into.
    file.sync_all().with_context(|| format!("flush {}", tmp.display()))?;
    Ok(tmp)
}

// ── installing it ────────────────────────────────────────────────────────

/// Put the staged binary in place, over the one this process is running.
///
/// The daemon must already be stopped: renaming over a *running* butai leaves
/// anything that spawns one from `current_exe()` pointing at a deleted inode,
/// which is the whole reason the order in this module's header is the order it
/// is. Callers that hold a socket stop it by asking
/// (`butai_client::update::apply`); a daemon updating itself has already left
/// its own event loop by the time it calls this.
///
/// A `rename`, so it is atomic and cannot leave a half-written program on the
/// path — which is why [`stage_beside`] puts the download in the install
/// directory rather than a temp dir, `rename` not crossing filesystems.
pub fn swap(staged: &Staged) -> Result<()> {
    std::fs::rename(&staged.tmp, &staged.install).with_context(|| {
        format!("install {} over {}", staged.tmp.display(), staged.install.display())
    })
}

/// Replace this process with the newly installed butai.
///
/// Only returns on failure — `exec` does not come back. The arguments are this
/// invocation's own, so `butai -w foo` restarts as `butai -w foo`, and the path
/// is the resolved install path rather than `current_exe()`, which by this
/// point reports the deleted inode of the binary we just replaced.
pub fn restart(install: &Path) -> anyhow::Error {
    use std::os::unix::process::CommandExt;

    let err = std::process::Command::new(install).args(std::env::args_os().skip(1)).exec();
    anyhow::Error::new(err).context(format!("restart {}", install.display()))
}

// ── http ─────────────────────────────────────────────────────────────────

/// One agent per call. There are at most three requests in an update and they
/// are minutes apart at best, so a pooled connection would be closed by the far
/// end long before it were reused.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .user_agent(concat!("butai/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn get_string(url: &str) -> Result<String> {
    let mut resp = agent().get(url).call().with_context(|| format!("GET {url}"))?;
    resp.body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_string()
        .with_context(|| format!("read {url}"))
}

fn get_bytes(url: &str) -> Result<Vec<u8>> {
    let mut resp = agent().get(url).call().with_context(|| format!("GET {url}"))?;
    resp.body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .read_to_vec()
        .with_context(|| format!("read {url}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_compares_numbers_not_strings() {
        // The whole reason this is not `latest > mine` on `&str`.
        assert!(newer("1.0.10", "1.0.9"));
        assert!(!newer("1.0.9", "1.0.10"));

        assert!(newer("1.1.0", "1.0.0"));
        assert!(newer("2.0.0", "1.99.99"));
        assert!(newer("1.0.1", "1.0.0"));
    }

    #[test]
    fn the_same_version_is_not_newer() {
        assert!(!newer("1.0.0", "1.0.0"));
        assert!(!newer("0.9.0", "1.0.0"));
    }

    #[test]
    fn a_leading_v_is_tolerated_on_either_side() {
        assert!(newer("v1.1.0", "1.0.0"));
        assert!(!newer("v1.0.0", "v1.0.0"));
    }

    #[test]
    fn an_unparseable_tag_offers_nothing() {
        // Saying nothing is the only safe answer to a tag shaped like
        // something else — the alternative is offering a download named after
        // a version that does not exist.
        assert!(!newer("nightly", "1.0.0"));
        assert!(!newer("1.0", "1.0.0"));
        assert!(!newer("1.0.0.1", "1.0.0"));
        assert!(!newer("", "1.0.0"));
    }

    /// The rule that makes a dev channel able to move at all.
    #[test]
    fn a_prerelease_is_below_the_release_it_leads_to() {
        assert!(newer("1.1.0-rc1", "1.0.0"), "a higher triple wins whatever the suffix");
        assert!(!newer("1.0.0-rc1", "1.0.0"), "a prerelease is not ahead of its own release");
        assert!(newer("1.3.0", "1.3.0-dev.4"), "and the release is ahead of it");
    }

    /// The comparison the dev track lives on. It used to cut the suffix, which
    /// made every `-dev.N` of one release the same version as every other: a
    /// dev build was offered nothing until stable overtook it.
    #[test]
    fn one_dev_build_is_newer_than_the_one_before_it() {
        assert!(newer("1.3.0-dev.2", "1.3.0-dev.1"));
        assert!(!newer("1.3.0-dev.1", "1.3.0-dev.2"));
        assert!(!newer("1.3.0-dev.1", "1.3.0-dev.1"));
        // Numerically, for the same reason `1.0.10` beats `1.0.9` — a string
        // comparison puts `dev.10` below `dev.9`.
        assert!(newer("1.3.0-dev.10", "1.3.0-dev.9"));
        // A dev tag of the *next* release is ahead of a finished one.
        assert!(newer("1.4.0-dev.1", "1.3.0"));
    }

    #[test]
    fn a_numeric_identifier_ranks_below_a_worded_one() {
        // Semver's rule, and butai has a use for it: an `-rc.1` cut after the
        // `-dev.N` series is ahead of all of them.
        assert!(newer("1.3.0-rc.1", "1.3.0-dev.9"));
        assert!(!newer("1.3.0-dev.9", "1.3.0-rc.1"));
    }

    /// The list arrives in publish order, and this is the case that proves
    /// picking the head is wrong: a stable fix released *after* a dev tag it
    /// is behind.
    #[test]
    fn the_dev_channel_takes_the_highest_version_not_the_newest_entry() {
        let body = r#"[
            {"tag_name": "v1.2.1", "draft": false, "assets": []},
            {"tag_name": "v1.3.0-dev.2", "draft": false, "assets": []},
            {"tag_name": "v1.3.0-dev.1", "draft": false, "assets": []},
            {"tag_name": "v1.2.0", "draft": false, "assets": []}
        ]"#;
        let releases: Vec<Release> = serde_json::from_str(body).unwrap();
        assert_eq!(pick(releases).unwrap().tag_name, "v1.3.0-dev.2");
    }

    #[test]
    fn a_draft_is_never_picked() {
        // Its assets 404 for everyone but its author, so an offer pointing at
        // one is an update that cannot be carried out.
        let body = r#"[
            {"tag_name": "v9.9.9", "draft": true, "assets": []},
            {"tag_name": "v1.3.0-dev.1", "draft": false, "assets": []}
        ]"#;
        let releases: Vec<Release> = serde_json::from_str(body).unwrap();
        assert_eq!(pick(releases).unwrap().tag_name, "v1.3.0-dev.1");
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_passed_over_rather_than_guessed_at() {
        let body = r#"[
            {"tag_name": "nightly", "draft": false, "assets": []},
            {"tag_name": "v1.3.0-dev.1", "draft": false, "assets": []}
        ]"#;
        let releases: Vec<Release> = serde_json::from_str(body).unwrap();
        assert_eq!(pick(releases).unwrap().tag_name, "v1.3.0-dev.1");

        // And a list of nothing else is nothing to offer, not an error.
        let only = r#"[{"tag_name": "nightly", "draft": false, "assets": []}]"#;
        let releases: Vec<Release> = serde_json::from_str(only).unwrap();
        assert!(pick(releases).is_none());
    }

    /// SETTINGS writes `as_str`, serde reads it back. A word written here that
    /// serde would refuse is a setting that saves and then loses the whole
    /// config file to a parse warning on the next start.
    #[test]
    fn every_channel_writes_a_word_it_can_be_read_back_from() {
        for channel in Channel::ALL {
            let name = channel.as_str();
            assert_eq!(Channel::from_name(name), Some(channel));
            assert_eq!(serde_json::from_str::<Channel>(&format!("\"{name}\"")).unwrap(), channel);
        }
        assert_eq!(Channel::from_name("beta"), None);
    }

    #[test]
    fn the_channel_defaults_to_stable_and_reads_its_two_names() {
        assert_eq!(Channel::default(), Channel::Stable);
        assert_eq!(serde_json::from_str::<Channel>("\"stable\"").unwrap(), Channel::Stable);
        assert_eq!(serde_json::from_str::<Channel>("\"dev\"").unwrap(), Channel::Dev);
        // A typo is an error naming both, not a silent fall back to stable:
        // an install that believes it is on dev and is not would go quiet for
        // months without saying why.
        let err = serde_json::from_str::<Channel>("\"beta\"").unwrap_err().to_string();
        assert!(err.contains("stable") && err.contains("dev"), "{err}");
    }

    #[test]
    fn the_asset_is_named_for_this_build_not_this_machine() {
        // The contract with release.yml and scripts/release.sh. If this name
        // drifts, every update stops finding its artifact.
        assert_eq!(asset_name("1.1.0"), format!("butai-1.1.0-{TARGET}.tar.gz"));
        assert!(asset_name("1.1.0").starts_with("butai-1.1.0-"));
        assert!(asset_name("1.1.0").ends_with(".tar.gz"));
    }

    #[test]
    fn every_published_triple_makes_the_name_release_yml_writes() {
        // `stage="butai-$version-${{ matrix.target }}"`, tarred as
        // `$stage.tar.gz` — asserted here for all seven so a change to either
        // side has to change this test too.
        for target in [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "armv7-unknown-linux-gnueabihf",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
        ] {
            assert_eq!(
                format!("{BIN}-1.1.0-{target}.tar.gz"),
                format!("butai-1.1.0-{target}.tar.gz")
            );
        }
    }

    #[test]
    fn the_baked_in_target_is_a_real_triple() {
        // Guards the build script: an empty or absent TARGET would silently
        // produce `butai-1.1.0-.tar.gz` and never match anything.
        assert!(!TARGET.is_empty());
        assert!(TARGET.contains('-'), "{TARGET} does not look like a target triple");
    }

    #[test]
    fn the_env_var_switches_the_check_off() {
        // `enabled` reads the process environment, so this cannot run beside a
        // test that sets it differently; the config half is asserted here and
        // the env half is left to the one assertion that does not need a value.
        assert!(!enabled(false));
    }

    #[test]
    fn checksums_are_matched_by_asset_name() {
        let sums = "\
0000000000000000000000000000000000000000000000000000000000000000  butai-1.1.0-other.tar.gz
9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08  butai-1.1.0-mine.tar.gz
";
        // sha256("test")
        assert!(verify(b"test", sums, "butai-1.1.0-mine.tar.gz").is_ok());
    }

    #[test]
    fn a_mismatched_checksum_stops_the_update() {
        let sums = "0000000000000000000000000000000000000000000000000000000000000000  a.tar.gz\n";
        let err = verify(b"test", sums, "a.tar.gz").unwrap_err().to_string();
        assert!(err.contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn an_asset_missing_from_the_checksums_stops_the_update() {
        let sums = "0000000000000000000000000000000000000000000000000000000000000000  a.tar.gz\n";
        let err = verify(b"test", sums, "b.tar.gz").unwrap_err().to_string();
        assert!(err.contains("no checksum"), "{err}");
    }

    /// A release tarball, built the way `scripts/release.sh` builds one: a
    /// single top-level directory holding the binary beside its docs.
    fn tarball(binary: &[u8]) -> Vec<u8> {
        let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (name, body) in
            [("README.md", &b"# butai"[..]), ("butai", binary), ("LICENSE", &b"MPL"[..])]
        {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            ar.append_data(&mut header, format!("butai-1.1.0-{TARGET}/{name}"), body).unwrap();
        }
        ar.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn the_binary_comes_out_of_the_tarball_past_its_neighbours() {
        let out = extract_binary(&tarball(b"\x7fELF and so on"), "x.tar.gz").unwrap();
        assert_eq!(out, b"\x7fELF and so on");
    }

    #[test]
    fn a_tarball_with_no_binary_in_it_is_an_error() {
        let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        let mut header = tar::Header::new_gnu();
        header.set_size(6);
        header.set_mode(0o644);
        header.set_cksum();
        ar.append_data(&mut header, "butai-1.1.0/README.md", &b"# hello"[..6]).unwrap();
        let bytes = ar.into_inner().unwrap().finish().unwrap();

        let err = extract_binary(&bytes, "x.tar.gz").unwrap_err().to_string();
        assert!(err.contains("no `butai` executable"), "{err}");
    }

    #[test]
    fn a_cargo_build_does_not_replace_itself() {
        assert!(in_cargo_target(Path::new("/home/me/src/butai/target/debug/butai")));
        assert!(in_cargo_target(Path::new("/home/me/src/butai/target/release/butai")));
        // `scripts/release.sh` and `cross` put a cross build one level deeper.
        assert!(in_cargo_target(Path::new(
            "/src/butai/target/x86_64-unknown-linux-musl/release/butai"
        )));

        assert!(!in_cargo_target(Path::new("/usr/local/bin/butai")));
        assert!(!in_cargo_target(Path::new("/home/me/.local/bin/butai")));
        // A directory that merely happens to be called `release`.
        assert!(!in_cargo_target(Path::new("/home/me/release/butai")));
    }

    #[test]
    fn a_moved_target_directory_is_still_a_cargo_build() {
        // `CARGO_TARGET_DIR` points this tree's builds at a directory that is
        // not called `target`, so the name check alone would offer to replace a
        // `cargo build` with a release download.
        let root = std::env::temp_dir().join(format!("butai-cachedir-{}", std::process::id()));
        let dir = root.join("debug");
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("butai");
        std::fs::write(&exe, b"").unwrap();

        // Without the tag it is just a directory called `debug`.
        assert!(!in_cargo_target(&exe));

        std::fs::write(root.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55\n")
            .unwrap();
        assert!(in_cargo_target(&exe));

        // A file by that name with something else in it is not cargo's.
        std::fs::write(root.join("CACHEDIR.TAG"), "not the signature\n").unwrap();
        assert!(!in_cargo_target(&exe));

        std::fs::remove_dir_all(&root).ok();
    }
}
