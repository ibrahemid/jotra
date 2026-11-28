/// Platform file-type default-app policy — pure, no I/O, no platform dependencies.
///
/// The claimable-type table and comparison logic live here so they can be
/// unit-tested on every platform without any macOS SDK linkage.
use serde::{Deserialize, Serialize};

/// A group of file types Jotra can offer to become the default opener for.
///
/// HTML is deliberately absent: Jotra declares it can open `.html` (so it appears
/// in "Open With" and handles explicit opens) but must never register itself as
/// the default handler for `public.html`, which is what put it in the OS web-link
/// path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimableType {
    /// Stable id used across the IPC boundary and the settings UI.
    pub id: &'static str,
    /// Human label for the settings row.
    pub label: &'static str,
    /// Representative extensions, for display (e.g. "Default for .rs, .go, …").
    pub exts: &'static [&'static str],
    /// Concrete UTIs whose default handler this group sets.
    ///
    /// LaunchServices default handlers are per-concrete-UTI and do NOT cascade
    /// through conformance, so a group enumerates every UTI it owns rather than
    /// relying on an ancestor like `public.source-code`. The exported `com.writ.*`
    /// identifiers cover dev extensions macOS has no system UTI for.
    pub utis: &'static [&'static str],
    /// MIME types whose default handler this group sets on Linux: each one a
    /// shared-mime-info type or alias, and a `mimeType` the bundle's desktop
    /// entry declares.
    pub mimes: &'static [&'static str],
}

/// The set of type groups the in-app "Make default" control exposes.
///
/// Mirrors the `contentTypes` / `exportedType` declarations in `tauri.conf.json`:
/// a UTI here must be declared there, or Launch Services accepts the set call but
/// Finder never routes opens to Jotra.
pub fn claimable_types() -> &'static [ClaimableType] {
    const TYPES: &[ClaimableType] = &[
        ClaimableType {
            id: "plain-text",
            label: "Plain text & logs",
            exts: &["txt", "text", "log"],
            utis: &["public.plain-text", "com.apple.log"],
            mimes: &["text/plain"],
        },
        ClaimableType {
            id: "markdown",
            label: "Markdown",
            exts: &["md", "markdown"],
            utis: &["net.daringfireball.markdown"],
            mimes: &["text/markdown"],
        },
        ClaimableType {
            id: "config-data",
            label: "Config & data",
            exts: &[
                "json", "yaml", "yml", "toml", "cfg", "ini", "conf", "env", "csv", "xml",
            ],
            utis: &[
                "public.json",
                "public.yaml",
                "public.toml",
                "com.microsoft.ini",
                "com.writ.config-text",
                "public.comma-separated-values-text",
                "public.xml",
            ],
            mimes: &[
                "application/json",
                "text/yaml",
                "application/toml",
                "text/csv",
                "text/xml",
            ],
        },
        ClaimableType {
            id: "source-code",
            label: "Source code",
            exts: &[
                "rs", "go", "jsx", "ts", "tsx", "py", "sh", "js", "c", "cpp", "h", "swift",
            ],
            // Concrete UTIs only. `public.source-code` is intentionally absent:
            // default handlers don't cascade through it, so claiming it would
            // never route a real file yet would block this group from ever
            // reading as "default" (every listed UTI must be ours to qualify).
            utis: &[
                "com.writ.rust-source",
                "com.writ.go-source",
                "com.writ.jsx-source",
                "com.writ.typescript-source",
                "public.python-script",
                "public.shell-script",
                "com.netscape.javascript-source",
                "public.c-source",
                "public.c-plus-plus-source",
                "public.c-header",
                "public.swift-source",
            ],
            // `text/jsx` and `text/x-swift` are newer shared-mime-info types;
            // `known_mimes` leaves them out where the system has not got them.
            mimes: &[
                "text/rust",
                "text/x-go",
                "text/jsx",
                "application/typescript",
                "text/x-python",
                "text/x-shellscript",
                "text/javascript",
                "text/x-c",
                "text/x-swift",
            ],
        },
    ];
    TYPES
}

/// Look up a claimable type group by its stable id.
pub fn claimable_type(id: &str) -> Option<&'static ClaimableType> {
    claimable_types().iter().find(|t| t.id == id)
}

/// Whether Jotra is the registered default handler for a given UTI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DefaultAppStatus {
    /// Jotra is the default handler.
    IsDefault,
    /// Another application is the default handler.
    OtherApp {
        /// Display name of the current default, if resolvable.
        name: Option<String>,
    },
    /// No handler is registered for this UTI.
    NoHandler,
    /// The current platform does not support this query.
    Unsupported,
    /// The platform lets only the user pick a default app, in its own
    /// settings: Windows, where Jotra links to Default apps.
    SystemSettings,
}

impl DefaultAppStatus {
    /// Interpret a raw bundle-id string returned by the OS.
    ///
    /// The comparison is case-insensitive; macOS Launch Services normalises
    /// bundle ids to lowercase internally.
    pub fn from_handler_id(handler_id: Option<&str>, our_bundle_id: &str) -> Self {
        match handler_id {
            None => Self::NoHandler,
            Some(id) if id.eq_ignore_ascii_case(our_bundle_id) => Self::IsDefault,
            Some(_) => Self::OtherApp { name: None },
        }
    }

    /// Returns `true` when the result indicates the platform cannot answer.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported)
    }
}

/// Combine the per-UTI statuses of a claimable group into one row status.
///
/// A group is [`DefaultAppStatus::IsDefault`] only when Jotra owns every UTI in it.
/// If the platform cannot answer for any UTI the whole group is
/// [`DefaultAppStatus::Unsupported`]; otherwise it surfaces the first UTI Jotra does
/// not own so the row shows what would be displaced.
pub fn aggregate_status(statuses: &[DefaultAppStatus]) -> DefaultAppStatus {
    if statuses.is_empty() || statuses.iter().any(DefaultAppStatus::is_unsupported) {
        return DefaultAppStatus::Unsupported;
    }
    if statuses
        .iter()
        .any(|s| matches!(s, DefaultAppStatus::SystemSettings))
    {
        return DefaultAppStatus::SystemSettings;
    }
    if statuses
        .iter()
        .all(|s| matches!(s, DefaultAppStatus::IsDefault))
    {
        return DefaultAppStatus::IsDefault;
    }
    statuses
        .iter()
        .find(|s| !matches!(s, DefaultAppStatus::IsDefault))
        .cloned()
        .unwrap_or(DefaultAppStatus::NoHandler)
}

/// What the running system offers for making Jotra a default app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultAppSystem {
    /// macOS, where Launch Services sets the default per type.
    MacOs,
    /// Windows, where only the user picks a default, in Settings.
    Windows,
    /// Linux, where `xdg-mime` sets the default against Jotra's desktop entry.
    Linux {
        /// Jotra's desktop entry is installed.
        desktop_entry: bool,
        /// `xdg-mime` is on the search path.
        xdg_mime: bool,
    },
    /// Any other system.
    Other,
}

/// Whether `system` lets the default-app control be offered at all, answered
/// from what is installed without reading any type's current default.
pub fn offers_default_apps(system: DefaultAppSystem) -> bool {
    match system {
        DefaultAppSystem::MacOs | DefaultAppSystem::Windows => true,
        DefaultAppSystem::Linux {
            desktop_entry,
            xdg_mime,
        } => desktop_entry && xdg_mime,
        DefaultAppSystem::Other => false,
    }
}

/// The Windows settings page where the user picks default apps. Windows lets
/// no app make itself the default, so Jotra opens this page instead.
pub const WINDOWS_DEFAULT_APPS_URI: &str = "ms-settings:defaultapps";

/// The names Jotra's desktop entry ships under on Linux: the package's own
/// `Jotra.desktop`, `jotra.desktop` as distribution packages rename it, then
/// the two ids from before the rename that `mimeapps.list` entries may name.
pub const DESKTOP_ENTRY_NAMES: &[&str] = &[
    "Jotra.desktop",
    "jotra.desktop",
    crate::legacy_names::DESKTOP_ENTRY,
    crate::legacy_names::LOWERCASE_DESKTOP_ENTRY,
];

const CURRENT_DESKTOP_ENTRY_COUNT: usize = 2;

/// The XDG data folders, in order: `$XDG_DATA_HOME` (default
/// `~/.local/share`), then each of `$XDG_DATA_DIRS` (default
/// `/usr/local/share:/usr/share`).
pub fn data_dirs(
    data_home: Option<&str>,
    data_dirs: Option<&str>,
    home: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;

    let mut dirs = Vec::new();
    match data_home.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => dirs.push(PathBuf::from(value)),
        None => {
            if let Some(home) = home {
                dirs.push(home.join(".local/share"));
            }
        }
    }
    let data_dirs = data_dirs
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("/usr/local/share:/usr/share");
    dirs.extend(
        data_dirs
            .split(':')
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from),
    );
    dirs
}

/// The folders a desktop entry is looked up in: `applications` inside each of
/// [`data_dirs`], in the same order.
pub fn application_dirs(
    data_home: Option<&str>,
    data_dirs: Option<&str>,
    home: Option<&std::path::Path>,
) -> Vec<std::path::PathBuf> {
    self::data_dirs(data_home, data_dirs, home)
        .into_iter()
        .map(|dir| dir.join("applications"))
        .collect()
}

/// The names a shared-mime-info database knows, from the `mime/types` file of
/// one data folder (a type per line) and its `mime/aliases` file (an alias and
/// its type per line).
pub fn mime_database_names(types: &str, aliases: &str) -> std::collections::HashSet<String> {
    let types = types
        .lines()
        .filter_map(|line| line.split_whitespace().next());
    let aliases = aliases
        .lines()
        .flat_map(|line| line.split_whitespace().take(2));
    types
        .chain(aliases)
        .filter(|name| name.contains('/'))
        .map(str::to_owned)
        .collect()
}

/// The types of `mimes` the system's database names, in order, or all of
/// them when no database could be read (`known` is `None`). A default set
/// for a type the database lacks reads back as Jotra's while the files it is
/// meant for open as another type.
pub fn known_mimes<'a>(
    mimes: &[&'a str],
    known: Option<&std::collections::HashSet<String>>,
) -> Vec<&'a str> {
    mimes
        .iter()
        .copied()
        .filter(|mime| known.is_none_or(|known| known.contains(*mime)))
        .collect()
}

/// The desktop entry a default is written for: the first current name found in
/// `dirs`, else the first name from before the rename, or `None` when Jotra
/// has no desktop entry: an AppImage nobody integrated has none, and no
/// default can name it.
pub fn find_desktop_entry(
    dirs: &[std::path::PathBuf],
    exists: impl Fn(&std::path::Path) -> bool,
) -> Option<&'static str> {
    let (current, legacy) = DESKTOP_ENTRY_NAMES.split_at(CURRENT_DESKTOP_ENTRY_COUNT);
    [current, legacy].into_iter().find_map(|names| {
        dirs.iter()
            .find_map(|dir| names.iter().copied().find(|name| exists(&dir.join(name))))
    })
}

/// The status `xdg-mime query default <type>` answers, given Jotra's own
/// desktop entry `ours`. An answer naming `ours` or any of
/// [`DESKTOP_ENTRY_NAMES`] is Jotra. A blank answer means no default is set;
/// another entry is named without its `.desktop` suffix.
pub fn desktop_entry_status(answer: &str, ours: &str) -> DefaultAppStatus {
    let answer = answer.trim();
    if answer.is_empty() {
        return DefaultAppStatus::NoHandler;
    }
    if answer == ours || DESKTOP_ENTRY_NAMES.contains(&answer) {
        return DefaultAppStatus::IsDefault;
    }
    DefaultAppStatus::OtherApp {
        name: Some(
            answer
                .strip_suffix(".desktop")
                .unwrap_or(answer)
                .to_string(),
        ),
    }
}

/// The hidden desktop entry a release AppImage writes in the user's
/// applications folder so links under either scheme reach it: an image
/// nobody integrated has no entry of its own.
pub const APPIMAGE_LINK_ENTRY: &str = "jotra-links.desktop";

/// The link types [`APPIMAGE_LINK_ENTRY`] handles, one per scheme a link is
/// read under.
pub fn link_types() -> [String; 2] {
    [crate::jotra_link::SCHEME, crate::legacy_names::LINK_SCHEME]
        .map(|scheme| format!("x-scheme-handler/{scheme}"))
}

/// The text of [`APPIMAGE_LINK_ENTRY`] starting the image at `image` with the
/// link it is handed, or `None` when the path cannot be written into an
/// `Exec` value.
pub fn appimage_link_entry(image: &str, app_name: &str) -> Option<String> {
    let exec = crate::linux_rename::exec_argument(image)?;
    Some(format!(
        "[Desktop Entry]\nType=Application\nName={app_name}\nExec={exec} %u\nTerminal=false\nNoDisplay=true\nMimeType={};\n",
        link_types().join(";")
    ))
}

/// Whether an AppImage sets its link entry as the handler of a link type,
/// given what `xdg-mime query default` answers for it: only while no program
/// handles the type, so a handler already set, an installed package's among
/// them, is kept.
pub fn claims_link_type(answer: &str) -> bool {
    answer.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claimable_types_are_non_empty_with_unique_ids() {
        let types = claimable_types();
        assert!(!types.is_empty());
        let mut ids: Vec<&str> = types.iter().map(|t| t.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), types.len(), "claimable type ids must be unique");
        for t in types {
            assert!(!t.exts.is_empty(), "{} has no exts", t.id);
            assert!(!t.utis.is_empty(), "{} has no utis", t.id);
        }
    }

    #[test]
    fn html_is_never_claimable() {
        for t in claimable_types() {
            assert!(!t.exts.contains(&"html"), "{} claims html", t.id);
            assert!(!t.exts.contains(&"htm"), "{} claims htm", t.id);
            assert!(
                !t.utis.contains(&"public.html"),
                "{} claims public.html",
                t.id
            );
        }
    }

    #[test]
    fn no_group_claims_non_routing_ancestors() {
        // public.text is the broad parent of plain-text/rtf/source; claiming it
        // would make Jotra the fallback for nearly all text content. public.source-code
        // never routes a concrete file (defaults don't cascade) and, as a member of
        // an all-must-match group, would only keep the row from ever reading default.
        for t in claimable_types() {
            assert!(
                !t.utis.contains(&"public.text"),
                "{} claims public.text",
                t.id
            );
            assert!(
                !t.utis.contains(&"public.source-code"),
                "{} claims public.source-code",
                t.id
            );
        }
    }

    #[test]
    fn dev_extensions_without_system_utis_are_owned_via_exported_utis() {
        let src = claimable_type("source-code").unwrap();
        for ext in ["rs", "go", "jsx"] {
            assert!(src.exts.contains(&ext), "source-code missing {ext}");
        }
        for uti in [
            "com.writ.rust-source",
            "com.writ.go-source",
            "com.writ.jsx-source",
            "com.writ.typescript-source",
        ] {
            assert!(src.utis.contains(&uti), "source-code missing {uti}");
        }
        let cfg = claimable_type("config-data").unwrap();
        assert!(cfg.exts.contains(&"env") && cfg.exts.contains(&"conf"));
        assert!(cfg.utis.contains(&"com.writ.config-text"));
    }

    #[test]
    fn claimable_type_lookup() {
        assert_eq!(claimable_type("markdown").unwrap().id, "markdown");
        assert!(claimable_type("nope").is_none());
    }

    #[test]
    fn aggregate_all_default_is_default() {
        let s = [DefaultAppStatus::IsDefault, DefaultAppStatus::IsDefault];
        assert_eq!(aggregate_status(&s), DefaultAppStatus::IsDefault);
    }

    #[test]
    fn aggregate_partial_is_not_default() {
        let s = [
            DefaultAppStatus::IsDefault,
            DefaultAppStatus::OtherApp { name: None },
        ];
        assert!(matches!(
            aggregate_status(&s),
            DefaultAppStatus::OtherApp { .. }
        ));

        let s = [DefaultAppStatus::IsDefault, DefaultAppStatus::NoHandler];
        assert_eq!(aggregate_status(&s), DefaultAppStatus::NoHandler);
    }

    #[test]
    fn aggregate_any_unsupported_is_unsupported() {
        let s = [DefaultAppStatus::IsDefault, DefaultAppStatus::Unsupported];
        assert_eq!(aggregate_status(&s), DefaultAppStatus::Unsupported);
        assert_eq!(aggregate_status(&[]), DefaultAppStatus::Unsupported);
    }

    #[test]
    fn from_handler_id_is_default_when_bundle_matches() {
        let status = DefaultAppStatus::from_handler_id(Some("com.writ.editor"), "com.writ.editor");
        assert_eq!(status, DefaultAppStatus::IsDefault);
    }

    #[test]
    fn from_handler_id_is_case_insensitive() {
        let status = DefaultAppStatus::from_handler_id(Some("COM.WRIT.EDITOR"), "com.writ.editor");
        assert_eq!(status, DefaultAppStatus::IsDefault);
    }

    #[test]
    fn from_handler_id_other_app_when_different_bundle() {
        let status =
            DefaultAppStatus::from_handler_id(Some("com.apple.TextEdit"), "com.writ.editor");
        assert!(matches!(status, DefaultAppStatus::OtherApp { .. }));
    }

    #[test]
    fn from_handler_id_no_handler_when_none() {
        let status = DefaultAppStatus::from_handler_id(None, "com.writ.editor");
        assert_eq!(status, DefaultAppStatus::NoHandler);
    }

    #[test]
    fn unsupported_variant_reports_correctly() {
        assert!(DefaultAppStatus::Unsupported.is_unsupported());
        assert!(!DefaultAppStatus::IsDefault.is_unsupported());
        assert!(!DefaultAppStatus::NoHandler.is_unsupported());
        assert!(!DefaultAppStatus::OtherApp { name: None }.is_unsupported());
    }
}
