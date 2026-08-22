//! Named palettes for the workbench chrome.
//!
//! A theme maps semantic roles (`accent`, `danger`, `faint`, ...) to colors.
//! [`BUILTINS`] lists the ones that ship; any other name is read from
//! `~/.butai/themes/<name>.toml`, which may `extends` another theme and
//! override only the roles it cares about. Unknown roles warn rather than
//! error, so a theme written for a newer butai still loads.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::config::parse_hex_color;

/// One role's color. `Default` and `Ansi` defer to the terminal's own palette —
/// that is what the `terminal` built-in is made of, and why butai can still
/// inherit a user's colorscheme. `Rgb` pins an exact color.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeColor {
    Default,
    Ansi(u8),
    Rgb(u8, u8, u8),
}

impl ThemeColor {
    /// Accepts `"default"`, `"ansi:0"`..`"ansi:255"`, or `"#rrggbb"`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("default") {
            return Some(Self::Default);
        }
        if let Some(n) = s.strip_prefix("ansi:") {
            return n.trim().parse::<u8>().ok().map(Self::Ansi);
        }
        parse_hex_color(s).map(|(r, g, b)| Self::Rgb(r, g, b))
    }
}

/// Declares the role set once, deriving both the struct and the name lookup
/// used to apply `[colors]` tables — so adding a role cannot desync the two.
macro_rules! roles {
    ($($(#[$doc:meta])* $role:ident),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub struct Palette {
            $($(#[$doc])* pub $role: ThemeColor,)+
        }

        impl Palette {
            /// Every role name, in declaration order.
            pub const ROLES: &'static [&'static str] = &[$(stringify!($role),)+];

            /// Assign by role name; `false` if the name is not a role.
            fn set(&mut self, role: &str, color: ThemeColor) -> bool {
                match role {
                    $(stringify!($role) => { self.$role = color; true },)+
                    _ => false,
                }
            }

            /// Read by role name; `None` if the name is not a role.
            pub fn get(&self, role: &str) -> Option<ThemeColor> {
                match role {
                    $(stringify!($role) => Some(self.$role),)+
                    _ => None,
                }
            }
        }
    };
}

roles!(
    /// Window background.
    ground,
    /// Background of the focused stage.
    surface,
    /// Background of inset panels.
    sunken,
    /// Cursor-row background in a focused list.
    selection,
    /// Cursor-row background in an unfocused list.
    selection_dim,
    /// Primary text.
    ink,
    /// Secondary text.
    muted,
    /// Hints, section labels, dim rows.
    faint,
    /// Text painted on top of an `accent` or `attention` fill — so it is dark
    /// in a dark theme (where accents are light) and light in a light one.
    on_accent,
    /// Panel borders.
    rule,
    /// Border of the focused panel.
    rule_focus,
    /// Markers, the active tab, carets.
    accent,
    /// Informational status, e.g. an agent that finished.
    info,
    /// Success: `ok`, additions.
    ok,
    /// Working, badges, warnings.
    attention,
    /// `WAIT`, `FAIL(n)`, deletions.
    danger,
    /// Footer background.
    status_bg,
    /// Footer text.
    status_fg,
);

const fn rgb(r: u8, g: u8, b: u8) -> ThemeColor {
    ThemeColor::Rgb(r, g, b)
}

/// Selected when `config.toml` names no theme.
/// Re-exported from `config`, which owns it: it is the value a config with no
/// `[theme] name` takes, and this module is what turns that name into colours.
pub use crate::config::DEFAULT_THEME;

/// Every palette selectable without writing a file.
///
/// The order is the order the SETTINGS picker walks, so it is a reading order
/// rather than a list: the two house palettes first, then the four borrowed
/// ones alphabetically, then `terminal` last because it is the escape hatch
/// rather than a look — it pins nothing and lets your own colorscheme win.
///
/// The borrowed four were shipped as files in `examples/themes/` first. They
/// are built in now because a palette you have to copy a file to try is one
/// almost nobody tries, and the SETTINGS page applies a theme live as the
/// cursor passes it — which only works on themes that are already resolvable.
pub const BUILTINS: &[&str] = &[
    "blueprint-dark",
    "blueprint-light",
    "catppuccin-mocha",
    "gruvbox-dark",
    "nord",
    "solarized-light",
    "tokyonight",
    "terminal",
];

/// The dark palette: deep blue-grey grounds, one blue accent, amber and green
/// for state.
pub const BLUEPRINT_DARK: Palette = Palette {
    ground: rgb(0x15, 0x1a, 0x23),
    surface: rgb(0x1b, 0x22, 0x30),
    sunken: rgb(0x10, 0x15, 0x1d),
    selection: rgb(0x1f, 0x25, 0x35),
    selection_dim: rgb(0x19, 0x1f, 0x2b),
    ink: rgb(0xdd, 0xe4, 0xef),
    muted: rgb(0x8d, 0x9a, 0xae),
    faint: rgb(0x66, 0x73, 0x8a),
    on_accent: rgb(0x15, 0x1a, 0x23),
    rule: rgb(0x2b, 0x35, 0x47),
    rule_focus: rgb(0x7a, 0xa2, 0xf7),
    accent: rgb(0x7a, 0xa2, 0xf7),
    // A cyan of its own rather than a second name for `accent`. The two were the
    // same hex, which was invisible until the network gauge started using the
    // pair to mean *direction*: `↓` in `info` and `↑` in `accent` came out
    // identical, so the row said which way the traffic went in a colour that
    // said nothing. `tokyonight` already draws `info` this way.
    info: rgb(0x7d, 0xcf, 0xff),
    ok: rgb(0x9e, 0xce, 0x6a),
    attention: rgb(0xe0, 0xaf, 0x68),
    danger: rgb(0xf7, 0x76, 0x8e),
    status_bg: rgb(0x1b, 0x22, 0x30),
    status_fg: rgb(0x8d, 0x9a, 0xae),
};

/// The same palette inverted for light terminals.
pub const BLUEPRINT_LIGHT: Palette = Palette {
    ground: rgb(0xe9, 0xed, 0xf3),
    surface: rgb(0xf7, 0xf9, 0xfc),
    sunken: rgb(0xdf, 0xe5, 0xee),
    selection: rgb(0xdb, 0xe2, 0xee),
    selection_dim: rgb(0xe3, 0xe8, 0xf1),
    ink: rgb(0x1b, 0x23, 0x31),
    muted: rgb(0x5c, 0x69, 0x80),
    faint: rgb(0x8a, 0x95, 0xa8),
    on_accent: rgb(0xf7, 0xf9, 0xfc),
    rule: rgb(0xc6, 0xcf, 0xdd),
    rule_focus: rgb(0x2f, 0x56, 0xb8),
    accent: rgb(0x2f, 0x56, 0xb8),
    // Dark enough to hold its own on a near-white ground, and far enough round
    // the wheel from `accent` to read as a different thing. See BLUEPRINT_DARK.
    info: rgb(0x0e, 0x74, 0x90),
    ok: rgb(0x4a, 0x7c, 0x2a),
    attention: rgb(0x9c, 0x64, 0x07),
    danger: rgb(0xb3, 0x26, 0x1e),
    status_bg: rgb(0xdf, 0xe5, 0xee),
    status_fg: rgb(0x5c, 0x69, 0x80),
};

/// The preset `docs/design.md` names as the original default. Its `rule`,
/// `rule_focus`, `status_bg` and `status_fg` are the exact hexes the `[theme]`
/// table defaulted to before themes existed, so this restores the old chrome.
pub const TOKYONIGHT: Palette = Palette {
    ground: rgb(0x1a, 0x1b, 0x26),
    surface: rgb(0x1f, 0x23, 0x35),
    sunken: rgb(0x16, 0x16, 0x1e),
    selection: rgb(0x29, 0x2e, 0x42),
    selection_dim: rgb(0x22, 0x24, 0x36),
    ink: rgb(0xc0, 0xca, 0xf5),
    muted: rgb(0xa9, 0xb1, 0xd6),
    faint: rgb(0x56, 0x5f, 0x89),
    on_accent: rgb(0x1a, 0x1b, 0x26),
    rule: rgb(0x3b, 0x42, 0x61),
    rule_focus: rgb(0x7a, 0xa2, 0xf7),
    accent: rgb(0x7a, 0xa2, 0xf7),
    info: rgb(0x7d, 0xcf, 0xff),
    ok: rgb(0x9e, 0xce, 0x6a),
    attention: rgb(0xe0, 0xaf, 0x68),
    danger: rgb(0xf7, 0x76, 0x8e),
    status_bg: rgb(0x1f, 0x23, 0x35),
    status_fg: rgb(0xc0, 0xca, 0xf5),
};

/// Catppuccin's darkest flavour, using its named colors as-is.
///
/// Pairs with `syntax_theme = "base16-mocha.dark"` for the editor and diff
/// panes. `examples/themes/catppuccin-mocha.toml` is this palette written out
/// in full, with the Catppuccin name of every value in a comment beside it.
pub const CATPPUCCIN_MOCHA: Palette = Palette {
    ground: rgb(0x1e, 0x1e, 0x2e),
    surface: rgb(0x31, 0x32, 0x44),
    sunken: rgb(0x18, 0x18, 0x25),
    selection: rgb(0x45, 0x47, 0x5a),
    selection_dim: rgb(0x31, 0x32, 0x44),
    ink: rgb(0xcd, 0xd6, 0xf4),
    muted: rgb(0xa6, 0xad, 0xc8),
    faint: rgb(0x6c, 0x70, 0x86),
    on_accent: rgb(0x1e, 0x1e, 0x2e),
    rule: rgb(0x45, 0x47, 0x5a),
    rule_focus: rgb(0x89, 0xb4, 0xfa),
    accent: rgb(0x89, 0xb4, 0xfa),
    info: rgb(0x89, 0xdc, 0xeb),
    ok: rgb(0xa6, 0xe3, 0xa1),
    attention: rgb(0xf9, 0xe2, 0xaf),
    danger: rgb(0xf3, 0x8b, 0xa8),
    status_bg: rgb(0x18, 0x18, 0x25),
    status_fg: rgb(0xa6, 0xad, 0xc8),
};

/// Warm retro palette, all values from gruvbox's own ramps.
///
/// The state colors are gruvbox's *bright* variants, which are the ones that
/// carry on its dark grounds. Pairs with `syntax_theme = "base16-eighties.dark"`.
pub const GRUVBOX_DARK: Palette = Palette {
    ground: rgb(0x28, 0x28, 0x28),
    surface: rgb(0x32, 0x30, 0x2f),
    sunken: rgb(0x1d, 0x20, 0x21),
    selection: rgb(0x50, 0x49, 0x45),
    selection_dim: rgb(0x3c, 0x38, 0x36),
    ink: rgb(0xeb, 0xdb, 0xb2),
    muted: rgb(0xbd, 0xae, 0x93),
    faint: rgb(0x92, 0x83, 0x74),
    on_accent: rgb(0x28, 0x28, 0x28),
    rule: rgb(0x50, 0x49, 0x45),
    rule_focus: rgb(0x83, 0xa5, 0x98),
    accent: rgb(0x83, 0xa5, 0x98),
    info: rgb(0x8e, 0xc0, 0x7c),
    ok: rgb(0xb8, 0xbb, 0x26),
    attention: rgb(0xfa, 0xbd, 0x2f),
    danger: rgb(0xfb, 0x49, 0x34),
    status_bg: rgb(0x3c, 0x38, 0x36),
    status_fg: rgb(0xeb, 0xdb, 0xb2),
};

/// Arctic blue-greys: Polar Night for the grounds, Frost for the accent, Aurora
/// for state.
///
/// Two values are not Nord's own, and both are the same kind of gap. `sunken`
/// sits below `nord0`, which is already the darkest tone Nord defines, so the
/// tab strip would otherwise be indistinguishable from the ground. `faint` is
/// `#616e88` rather than `nord3` — the value nord-vim added for comments after
/// `nord3` on `nord0` turned out to be too low-contrast to read, which is
/// exactly the job `faint` does here.
pub const NORD: Palette = Palette {
    ground: rgb(0x2e, 0x34, 0x40),
    surface: rgb(0x3b, 0x42, 0x52),
    sunken: rgb(0x27, 0x2c, 0x36),
    selection: rgb(0x43, 0x4c, 0x5e),
    selection_dim: rgb(0x3b, 0x42, 0x52),
    ink: rgb(0xec, 0xef, 0xf4),
    muted: rgb(0xd8, 0xde, 0xe9),
    faint: rgb(0x61, 0x6e, 0x88),
    on_accent: rgb(0x2e, 0x34, 0x40),
    rule: rgb(0x4c, 0x56, 0x6a),
    rule_focus: rgb(0x88, 0xc0, 0xd0),
    accent: rgb(0x88, 0xc0, 0xd0),
    info: rgb(0x81, 0xa1, 0xc1),
    ok: rgb(0xa3, 0xbe, 0x8c),
    attention: rgb(0xeb, 0xcb, 0x8b),
    danger: rgb(0xbf, 0x61, 0x6a),
    status_bg: rgb(0x3b, 0x42, 0x52),
    status_fg: rgb(0xd8, 0xde, 0xe9),
};

/// Ethan Schoonover's Solarized, light background.
///
/// Worth pairing: set `syntax_theme = "Solarized (light)"` so the editor and
/// diff panes match — syntect ships that theme, and a dark one under a light
/// chrome looks wrong. Solarized defines only two light background tones
/// (base3, base2), so `selection_dim` is interpolated between them to keep the
/// unfocused cursor row distinguishable from both.
pub const SOLARIZED_LIGHT: Palette = Palette {
    ground: rgb(0xfd, 0xf6, 0xe3),
    surface: rgb(0xfd, 0xf6, 0xe3),
    sunken: rgb(0xee, 0xe8, 0xd5),
    selection: rgb(0xee, 0xe8, 0xd5),
    selection_dim: rgb(0xf7, 0xf1, 0xde),
    ink: rgb(0x58, 0x6e, 0x75),
    muted: rgb(0x65, 0x7b, 0x83),
    faint: rgb(0x93, 0xa1, 0xa1),
    on_accent: rgb(0xfd, 0xf6, 0xe3),
    rule: rgb(0x93, 0xa1, 0xa1),
    rule_focus: rgb(0x26, 0x8b, 0xd2),
    accent: rgb(0x26, 0x8b, 0xd2),
    info: rgb(0x2a, 0xa1, 0x98),
    ok: rgb(0x85, 0x99, 0x00),
    attention: rgb(0xb5, 0x89, 0x00),
    danger: rgb(0xdc, 0x32, 0x2f),
    status_bg: rgb(0xee, 0xe8, 0xd5),
    status_fg: rgb(0x65, 0x7b, 0x83),
};

/// Pins nothing: every role defers to the terminal's own palette. The ANSI
/// indices are exactly what butai emitted before themes existed, so this is the
/// escape hatch for anyone whose colorscheme should win.
pub const TERMINAL: Palette = Palette {
    ground: ThemeColor::Default,
    surface: ThemeColor::Default,
    sunken: ThemeColor::Default,
    selection: ThemeColor::Ansi(238),
    selection_dim: ThemeColor::Ansi(236),
    ink: ThemeColor::Default,
    muted: ThemeColor::Ansi(7),
    faint: ThemeColor::Ansi(8),
    on_accent: ThemeColor::Ansi(0),
    rule: ThemeColor::Ansi(8),
    rule_focus: ThemeColor::Ansi(6),
    accent: ThemeColor::Ansi(6),
    info: ThemeColor::Ansi(4),
    ok: ThemeColor::Ansi(2),
    attention: ThemeColor::Ansi(3),
    danger: ThemeColor::Ansi(1),
    status_bg: ThemeColor::Ansi(8),
    status_fg: ThemeColor::Ansi(15),
};

pub fn builtin(name: &str) -> Option<Palette> {
    match name {
        "blueprint-dark" => Some(BLUEPRINT_DARK),
        "blueprint-light" => Some(BLUEPRINT_LIGHT),
        "catppuccin-mocha" => Some(CATPPUCCIN_MOCHA),
        "gruvbox-dark" => Some(GRUVBOX_DARK),
        "nord" => Some(NORD),
        "solarized-light" => Some(SOLARIZED_LIGHT),
        "tokyonight" => Some(TOKYONIGHT),
        "terminal" => Some(TERMINAL),
        _ => None,
    }
}

impl Default for Palette {
    fn default() -> Self {
        BLUEPRINT_DARK
    }
}

/// A theme file. `name` is documentation only — the filename selects the theme.
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
struct ThemeFile {
    #[allow(dead_code)]
    name: Option<String>,
    extends: Option<String>,
    colors: HashMap<String, String>,
}

/// Directory searched for user themes — `~/.butai/themes`, see
/// [`butai_protocol::paths::butai_dir`]. `BUTAI_THEME_DIR` overrides it, matching the
/// `BUTAI_SOCKET` convention in [`crate::paths`].
pub fn themes_dir() -> PathBuf {
    if let Ok(p) = std::env::var("BUTAI_THEME_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    butai_protocol::paths::butai_dir().join("themes")
}

/// Whether `name` would resolve to a real theme: a built-in, or a
/// `<name>.toml` in [`themes_dir`]. [`Palette::resolve`] deliberately falls back
/// to the default rather than failing, which is right at startup but wrong at
/// the `:theme` prompt — there a typo should be rejected, not silently applied.
pub fn exists(name: &str) -> bool {
    builtin(name).is_some() || themes_dir().join(format!("{name}.toml")).is_file()
}

/// Every theme that can be selected right now: the built-ins in declaration
/// order, then user themes sorted by name. A file named after a built-in is
/// listed once — [`resolve_named`] checks built-ins first, so the built-in is
/// what would actually load.
pub fn available() -> Vec<String> {
    let mut names: Vec<String> = BUILTINS.iter().map(|s| (*s).to_string()).collect();
    let mut user: Vec<String> = std::fs::read_dir(themes_dir())
        .ok()
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|e| {
                    let path = e.path();
                    if path.extension()? != "toml" {
                        return None;
                    }
                    Some(path.file_stem()?.to_str()?.to_string())
                })
                .filter(|n| !BUILTINS.contains(&n.as_str()))
                .collect()
        })
        .unwrap_or_default();
    user.sort_unstable();
    names.extend(user);
    names
}

/// Guards against an `extends` cycle and runaway chains.
const MAX_EXTENDS_DEPTH: usize = 16;

impl Palette {
    /// Resolve `name` to a palette, then apply `overrides` (role name -> color
    /// string) on top. Returns human-readable warnings alongside a palette
    /// that is always usable — an unresolvable theme falls back to the default
    /// built-in rather than failing the load.
    pub fn resolve(name: &str, overrides: &[(&str, &str)]) -> (Self, Vec<String>) {
        let dir = themes_dir();
        let mut warnings = Vec::new();
        let mut palette =
            resolve_named(name, Some(dir.as_path()), &mut HashSet::new(), &mut warnings)
                .unwrap_or_else(|| {
                    // Only warn about a miss the user could have caused; the
                    // default built-in always resolves.
                    if name != DEFAULT_THEME {
                        warnings.push(format!(
                            "theme \"{name}\" not found (built-ins: {}); using {DEFAULT_THEME}",
                            BUILTINS.join(", ")
                        ));
                    }
                    builtin(DEFAULT_THEME).expect("default theme is built in")
                });

        for (role, value) in overrides {
            match ThemeColor::parse(value) {
                Some(color) => {
                    palette.set(role, color);
                }
                None => warnings.push(format!("[theme] {role}: {}", bad_value(value))),
            }
        }
        (palette, warnings)
    }
}

fn bad_value(value: &str) -> String {
    format!("invalid color \"{value}\"; expected #rrggbb, ansi:N, or default")
}

fn resolve_named(
    name: &str,
    dir: Option<&Path>,
    seen: &mut HashSet<String>,
    warnings: &mut Vec<String>,
) -> Option<Palette> {
    if let Some(p) = builtin(name) {
        return Some(p);
    }
    if seen.len() >= MAX_EXTENDS_DEPTH {
        warnings.push(format!("theme \"{name}\": extends chain too deep"));
        return None;
    }
    if !seen.insert(name.to_string()) {
        warnings.push(format!("theme \"{name}\": extends cycle"));
        return None;
    }

    let path = dir?.join(format!("{name}.toml"));
    let text = std::fs::read_to_string(&path).ok()?;
    let file: ThemeFile = match toml::from_str(&text) {
        Ok(f) => f,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return None;
        }
    };

    // An unresolvable base is worth a warning but not a failed load — the
    // file's own colors still apply over the default.
    let mut palette = match &file.extends {
        Some(base) => resolve_named(base, dir, seen, warnings).unwrap_or_else(|| {
            warnings.push(format!("theme \"{name}\": unknown base \"{base}\""));
            builtin(DEFAULT_THEME).expect("default theme is built in")
        }),
        None => builtin(DEFAULT_THEME).expect("default theme is built in"),
    };

    for (role, value) in &file.colors {
        match ThemeColor::parse(value) {
            Some(color) => {
                if !palette.set(role, color) {
                    warnings.push(format!("{}: unknown role \"{role}\"", path.display()));
                }
            }
            None => warnings.push(format!("{}: {role}: {}", path.display(), bad_value(value))),
        }
    }
    Some(palette)
}

#[cfg(test)]
mod tests {
    use crate::config::Config;

    /// A `config.toml` written before roles existed still resolves: the old key
    /// names map to roles in `config`, and the palette takes them from there.
    /// The mapping and the resolution were asserted together in `butai-core`;
    /// only the mapping half could stay behind when the palette moved.
    #[test]
    fn legacy_theme_keys_resolve_to_a_palette() {
        let text = concat!(
            "[theme]\n",
            "border = \"#101010\"\n",
            "border_focused = \"#ff0000\"\n",
            "status_bg = \"#202020\"\n",
            "status_fg = \"#f0f0f0\"\n",
        );
        let cfg: Config = toml::from_str(text).unwrap();
        let (palette, warnings) = Palette::resolve(&cfg.theme.name, &cfg.theme.role_overrides());
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(palette.rule, ThemeColor::Rgb(0x10, 0x10, 0x10));
        assert_eq!(palette.rule_focus, ThemeColor::Rgb(0xff, 0, 0));
    }

    use super::*;

    /// Serialises tests that set `BUTAI_THEME_DIR`, which is process-global.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct ThemeDir(PathBuf);

    impl ThemeDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("butai-themes-{}-{tag}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("BUTAI_THEME_DIR", &dir);
            Self(dir)
        }
        fn write(&self, name: &str, body: &str) {
            std::fs::write(self.0.join(format!("{name}.toml")), body).unwrap();
        }
    }

    impl Drop for ThemeDir {
        fn drop(&mut self) {
            std::env::remove_var("BUTAI_THEME_DIR");
            std::fs::remove_dir_all(&self.0).ok();
        }
    }

    #[test]
    fn parses_every_color_form() {
        assert_eq!(ThemeColor::parse("#7aa2f7"), Some(ThemeColor::Rgb(0x7a, 0xa2, 0xf7)));
        assert_eq!(ThemeColor::parse("ansi:8"), Some(ThemeColor::Ansi(8)));
        assert_eq!(ThemeColor::parse("ansi:255"), Some(ThemeColor::Ansi(255)));
        assert_eq!(ThemeColor::parse("default"), Some(ThemeColor::Default));
        assert_eq!(ThemeColor::parse("Default"), Some(ThemeColor::Default));
        assert_eq!(ThemeColor::parse("ansi:256"), None);
        assert_eq!(ThemeColor::parse("7aa2f7"), None);
        assert_eq!(ThemeColor::parse("puce"), None);
    }

    #[test]
    fn builtins_resolve_without_warnings() {
        let _guard = ENV_LOCK.lock().unwrap();
        for name in BUILTINS {
            let (palette, warnings) = Palette::resolve(name, &[]);
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            assert_eq!(palette, builtin(name).unwrap());
        }
    }

    /// The `terminal` theme is the compatibility guarantee: every role must
    /// defer to the terminal, never pin an RGB value.
    #[test]
    fn terminal_theme_pins_nothing() {
        for role in Palette::ROLES {
            let color = TERMINAL.get(role).expect("ROLES lists a real role");
            assert!(!matches!(color, ThemeColor::Rgb(..)), "terminal pinned {role}: {color:?}");
        }
    }

    /// Every built-in must define every role — `get` returning `None` would
    /// mean `ROLES` and the struct had drifted apart.
    #[test]
    fn every_builtin_covers_every_role() {
        for name in BUILTINS {
            let palette = builtin(name).unwrap();
            for role in Palette::ROLES {
                assert!(palette.get(role).is_some(), "{name} missing {role}");
            }
        }
    }

    /// `tokyonight` is the documented restore path for the pre-theme chrome,
    /// whose four colors came from the old `[theme]` defaults.
    #[test]
    fn tokyonight_matches_the_old_theme_defaults() {
        assert_eq!(TOKYONIGHT.rule, ThemeColor::Rgb(0x3b, 0x42, 0x61));
        assert_eq!(TOKYONIGHT.rule_focus, ThemeColor::Rgb(0x7a, 0xa2, 0xf7));
        assert_eq!(TOKYONIGHT.status_bg, ThemeColor::Rgb(0x1f, 0x23, 0x35));
        assert_eq!(TOKYONIGHT.status_fg, ThemeColor::Rgb(0xc0, 0xca, 0xf5));
    }

    /// The indices butai emitted for these roles before themes existed.
    #[test]
    fn terminal_theme_keeps_legacy_ansi_indices() {
        assert_eq!(TERMINAL.faint, ThemeColor::Ansi(8)); // was Color::DarkGray
        assert_eq!(TERMINAL.danger, ThemeColor::Ansi(1)); // was Color::Red
        assert_eq!(TERMINAL.attention, ThemeColor::Ansi(3)); // was Color::Yellow
        assert_eq!(TERMINAL.ok, ThemeColor::Ansi(2)); // was Color::Green
        assert_eq!(TERMINAL.accent, ThemeColor::Ansi(6)); // was Color::Cyan
        assert_eq!(TERMINAL.info, ThemeColor::Ansi(4)); // was Color::Blue
        assert_eq!(TERMINAL.on_accent, ThemeColor::Ansi(0)); // was Color::Black
        assert_eq!(TERMINAL.selection, ThemeColor::Ansi(238));
        assert_eq!(TERMINAL.selection_dim, ThemeColor::Ansi(236));
    }

    #[test]
    fn extends_inherits_then_overrides() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("extends");
        dir.write("mine", "extends = \"blueprint-dark\"\n[colors]\naccent = \"#ff0000\"\n");

        let (palette, warnings) = Palette::resolve("mine", &[]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(palette.accent, ThemeColor::Rgb(0xff, 0, 0), "override applied");
        assert_eq!(palette.ink, BLUEPRINT_DARK.ink, "rest inherited");
        assert_eq!(palette.faint, BLUEPRINT_DARK.faint);
    }

    #[test]
    fn user_theme_can_extend_another_user_theme() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("chain");
        dir.write("base", "extends = \"terminal\"\n[colors]\nok = \"#00ff00\"\n");
        dir.write("derived", "extends = \"base\"\n[colors]\ndanger = \"#ff0000\"\n");

        let (palette, warnings) = Palette::resolve("derived", &[]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(palette.ok, ThemeColor::Rgb(0, 0xff, 0), "from base");
        assert_eq!(palette.danger, ThemeColor::Rgb(0xff, 0, 0), "from derived");
        assert_eq!(palette.faint, TERMINAL.faint, "from terminal");
    }

    #[test]
    fn unknown_role_warns_but_still_loads() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("unknown");
        dir.write(
            "odd",
            "extends = \"terminal\"\n[colors]\nsparkle = \"#ff0000\"\nok = \"#00ff00\"\n",
        );

        let (palette, warnings) = Palette::resolve("odd", &[]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("sparkle"), "{warnings:?}");
        assert_eq!(palette.ok, ThemeColor::Rgb(0, 0xff, 0), "good roles still applied");
    }

    #[test]
    fn bad_color_value_warns() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("badvalue");
        dir.write("broken", "[colors]\naccent = \"blue\"\n");

        let (palette, warnings) = Palette::resolve("broken", &[]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("accent"), "{warnings:?}");
        assert_eq!(palette.accent, BLUEPRINT_DARK.accent, "left at the inherited value");
    }

    #[test]
    fn extends_cycle_is_caught() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("cycle");
        dir.write("a", "extends = \"b\"\n");
        dir.write("b", "extends = \"a\"\n");

        let (_, warnings) = Palette::resolve("a", &[]);
        assert!(warnings.iter().any(|w| w.contains("cycle")), "{warnings:?}");
    }

    #[test]
    fn missing_theme_falls_back_with_a_warning() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _dir = ThemeDir::new("missing");

        let (palette, warnings) = Palette::resolve("nope", &[]);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("nope"), "{warnings:?}");
        assert_eq!(palette, BLUEPRINT_DARK);
    }

    /// Every file in `examples/themes/` named after a built-in is documented as
    /// that built-in written out in full. If a role is added or recolored and
    /// the file isn't updated, it stops being a working starting point — and
    /// silently, because the name resolves to the built-in either way.
    ///
    /// Resolved under a *different* name on purpose: `resolve_named` checks
    /// built-ins first, so a file called `nord.toml` would never be read and
    /// this test would compare the built-in against itself and always pass.
    #[test]
    fn shipped_examples_match_their_builtins() {
        let _guard = ENV_LOCK.lock().unwrap();
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/themes");
        let dir = ThemeDir::new("example");

        let mut checked = Vec::new();
        for name in BUILTINS.iter().filter(|n| **n != "terminal") {
            let file = src.join(format!("{name}.toml"));
            if !file.is_file() {
                continue;
            }
            std::fs::copy(&file, dir.0.join("under-test.toml"))
                .unwrap_or_else(|e| panic!("{}: {e}", file.display()));
            let (palette, warnings) = Palette::resolve("under-test", &[]);
            assert!(warnings.is_empty(), "{name}: {warnings:?}");
            assert_eq!(
                palette,
                builtin(name).unwrap(),
                "{name}.toml has drifted from the built-in"
            );
            checked.push(*name);
        }
        // A rename that orphaned every example would otherwise pass an empty
        // loop. `terminal` is the one built-in with no file: it pins nothing,
        // so writing it out would be a file of the word "default".
        assert_eq!(
            checked,
            ["blueprint-light", "catppuccin-mocha", "gruvbox-dark", "nord", "solarized-light"],
            "an example file went missing"
        );
    }

    /// `accent` and `info` have to be two different colours, because one place
    /// uses the pair to mean *direction*: the NET gauge draws `↓` in `info` and
    /// `↑` in `accent`. Both blueprint palettes shipped them as the same hex,
    /// which cost nothing until that gauge existed and then quietly made the
    /// download and upload traces indistinguishable.
    ///
    /// `terminal` is exempt: it defers to the user's own ANSI palette, where
    /// these are indices rather than colours and what they resolve to is not
    /// ours to assert.
    #[test]
    fn every_palette_separates_accent_from_info() {
        for name in BUILTINS.iter().filter(|n| **n != "terminal") {
            let p = builtin(name).unwrap();
            assert_ne!(
                p.accent, p.info,
                "{name}: accent and info are the same colour, so ↓ and ↑ cannot be told apart"
            );
        }
    }

    /// What `:theme` offers: built-ins first, then whatever is in the themes
    /// directory. Non-`.toml` files are not themes and must not be listed.
    #[test]
    fn available_lists_builtins_then_user_themes() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("available");
        dir.write("zebra", "extends = \"terminal\"\n");
        dir.write("aardvark", "extends = \"terminal\"\n");
        std::fs::write(dir.0.join("notes.txt"), "not a theme").unwrap();

        let names = available();
        assert_eq!(names[..BUILTINS.len()], *BUILTINS);
        assert_eq!(names[BUILTINS.len()..], ["aardvark", "zebra"]);
    }

    /// A file named after a built-in cannot shadow it — `resolve_named` checks
    /// built-ins first — so listing it twice would offer a name that does not
    /// do what it says.
    #[test]
    fn available_does_not_repeat_a_shadowed_builtin() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("shadow");
        dir.write("tokyonight", "[colors]\naccent = \"#ff0000\"\n");

        let names = available();
        assert_eq!(names.iter().filter(|n| *n == "tokyonight").count(), 1);
        assert_eq!(names.len(), BUILTINS.len());
    }

    /// `exists` is what keeps a typo at the `:theme` prompt from silently
    /// applying the default the way `resolve` would.
    #[test]
    fn exists_accepts_builtins_and_files_only() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = ThemeDir::new("exists");
        dir.write("mine", "extends = \"terminal\"\n");

        assert!(exists("blueprint-dark"), "built-in");
        assert!(exists("mine"), "user file");
        assert!(!exists("nope"), "typo is rejected, not resolved to the default");
        assert!(!exists(""), "empty name is not a theme");
    }

    /// Every theme in `examples/themes/` is meant to be copied straight into a
    /// themes directory and selected. A typo'd hex or a role renamed out from
    /// under one would only warn at runtime, so it has to fail here instead.
    ///
    /// Each is copied under a name of its own that no built-in shadows, so a
    /// file that shares a built-in's name is still read rather than skipped.
    #[test]
    fn every_shipped_example_resolves_cleanly() {
        let _guard = ENV_LOCK.lock().unwrap();
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/themes");
        let dir = ThemeDir::new("examples");

        let mut names = Vec::new();
        for entry in std::fs::read_dir(&src).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "toml") {
                continue;
            }
            let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
            let under_test = format!("x-{stem}");
            std::fs::copy(&path, dir.0.join(format!("{under_test}.toml"))).unwrap();
            names.push((stem, under_test));
        }
        assert!(names.len() >= 2, "expected the shipped examples, found {}", names.len());

        for (stem, under_test) in names {
            let (_, warnings) = Palette::resolve(&under_test, &[]);
            assert!(warnings.is_empty(), "examples/themes/{stem}.toml: {warnings:?}");
        }
    }

    #[test]
    fn legacy_overrides_apply_last() {
        let _guard = ENV_LOCK.lock().unwrap();
        let _dir = ThemeDir::new("overrides");

        let (palette, warnings) =
            Palette::resolve("blueprint-dark", &[("rule", "#101010"), ("accent", "ansi:5")]);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(palette.rule, ThemeColor::Rgb(0x10, 0x10, 0x10));
        assert_eq!(palette.accent, ThemeColor::Ansi(5));
        assert_eq!(palette.ink, BLUEPRINT_DARK.ink, "untouched roles keep the theme");
    }
}
