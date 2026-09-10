//! What kind of desktop session the app was launched into.
//!
//! The only question asked here is whether the window manager tiles. It
//! decides one thing — the *default* UI scale — and it decides it because a
//! tiling manager hands out windows the user did not size. A floating desktop
//! opens Mast at the 1100x720 the config asks for; Hyprland gives it whatever
//! fraction of the workspace is going, which is regularly half that. The same
//! type at the same scale is comfortable in the first and cramped in the
//! second.
//!
//! It is a default, never a policy: once the user sets a scale it wins, on
//! every session, whatever the compositor is called.

/// Window managers that tile by default.
///
/// Deliberately a list of names rather than a probe. There is no portable way
/// to ask a compositor "do you tile", and the alternatives — counting windows,
/// watching for resizes we did not ask for — are guesses dressed up as
/// measurements. A name we do not know simply gets the floating default, which
/// is the same thing every release before this one did.
const TILING: &[&str] = &[
    "hyprland",
    "sway",
    "i3",
    "river",
    "niri",
    "bspwm",
    "awesome",
    "dwm",
    "xmonad",
    "qtile",
    "herbstluftwm",
    "spectrwm",
    "leftwm",
    "dk",
];

/// Whether a desktop identifier names a tiling window manager.
///
/// `XDG_CURRENT_DESKTOP` is specified as colon-separated and is not
/// consistently cased — Omarchy reports `Hyprland`, some sessions report
/// `hyprland`, and a wrapped session can report `Hyprland:wlroots`. Any
/// component matching is enough.
pub fn is_tiling(desktop: &str) -> bool {
    desktop
        .split(':')
        .map(|part| part.trim().to_ascii_lowercase())
        .any(|part| TILING.contains(&part.as_str()))
}

/// The session's desktop identifier, from the first environment variable that
/// carries one. Empty values are skipped: an exported-but-empty
/// `XDG_CURRENT_DESKTOP` is common and means nothing.
pub fn desktop_name() -> Option<String> {
    ["XDG_CURRENT_DESKTOP", "XDG_SESSION_DESKTOP", "DESKTOP_SESSION"]
        .iter()
        .find_map(|key| match std::env::var(key) {
            Ok(value) if !value.trim().is_empty() => Some(value),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_compositor_omarchy_reports() {
        assert!(is_tiling("Hyprland"));
        assert!(is_tiling("hyprland"));
        assert!(is_tiling("Hyprland:wlroots"));
    }

    #[test]
    fn recognises_the_rest_of_the_list_however_it_is_cased() {
        for name in ["sway", "SWAY", "i3", "river", "niri", "bspwm", "xmonad"] {
            assert!(is_tiling(name), "{name} should be recognised as tiling");
        }
    }

    // The failure that matters is the false positive: telling a GNOME user
    // their desktop tiles shrinks their UI for no reason, on every launch.
    #[test]
    fn leaves_floating_desktops_alone() {
        for name in ["GNOME", "KDE", "XFCE", "X-Cinnamon", "LXQt", "Unity", "MATE"] {
            assert!(!is_tiling(name), "{name} must not be treated as tiling");
        }
    }

    #[test]
    fn an_unknown_or_empty_name_is_not_tiling() {
        assert!(!is_tiling(""));
        assert!(!is_tiling("something-nobody-has-written-yet"));
        assert!(!is_tiling(":"));
    }

    // "i3" as a substring of another name must not match — `is_tiling` splits
    // on ':' and compares whole components for exactly this reason.
    #[test]
    fn matches_whole_components_not_substrings() {
        assert!(!is_tiling("i3-like"));
        assert!(!is_tiling("notsway"));
        assert!(!is_tiling("GNOME-i3"));
    }
}
