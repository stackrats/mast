//! Environment the webview needs set before GTK and WebKit initialise.
//!
//! Both of these are read once, at library init, so they have to be in the
//! process environment before `tauri::Builder` runs — after that they are
//! inert. Neither overrides a value the user has already exported: someone who
//! has set these deliberately knows more about their machine than a heuristic
//! does.

use std::path::Path;

/// Names GTK and WebKit read at init, and the value each is given.
pub const OVERLAY_SCROLLING: (&str, &str) = ("GTK_OVERLAY_SCROLLING", "0");
pub const DMABUF_RENDERER: (&str, &str) = ("WEBKIT_DISABLE_DMABUF_RENDERER", "1");

/// Set what this session needs. Called once, first thing.
pub fn prepare() {
    // The app styles its scrollbars: 8px, themed, transparent track. WebKitGTK
    // honours none of that while GTK's overlay scrollbars are on — it draws
    // GTK's instead, which fade out at rest and widen under the pointer. In a
    // dark theme the widened state is a dark rounded track that looks like a
    // layout element that arrived from somewhere else. This is a look the app
    // chose, not a workaround, so it applies everywhere.
    set_default(OVERLAY_SCROLLING);

    // Toggling a window between tiled and floating on Hyprland, inside a VM,
    // left the webview black with the process alive and the status bar still
    // painting — WebKit's DMABUF path does not survive the surface being
    // reconfigured on virtio-gpu. Disabling it falls back to software
    // compositing, which is a real cost on real hardware, so it is applied
    // only where the failure has been seen: under a hypervisor.
    if in_virtual_machine() {
        set_default(DMABUF_RENDERER);
    }
}

fn set_default((key, value): (&str, &str)) {
    if std::env::var_os(key).is_none() {
        // SAFETY: called from `main` before any other thread exists. Setting
        // the environment is only unsound while another thread may be reading
        // it, and at this point there is none.
        unsafe { std::env::set_var(key, value) };
    }
}

/// Force the webview to take the window's current size.
///
/// Under Hyprland, toggling a window from tiled to floating reconfigures the
/// surface but leaves WebKit drawing at its old allocation — the content
/// appears shifted, with the menubar off the top and the left edge clipped,
/// until some later, real resize comes along. Fullscreen in and out fixes it,
/// and so does dragging an edge, which is the tell: GTK short-circuits the
/// allocation when it believes nothing changed, and WebKit never relayouts.
///
/// `queue_allocate` is GTK's answer to exactly that — re-run allocation even
/// when the size request is unchanged. It is a no-op in cost when nothing was
/// wrong, so it runs on every resize rather than trying to identify the one
/// that needs it, which would mean relying on the compositor advertising its
/// tiled state, which not all of them do. wry's own `set_bounds` does nothing
/// for a main-window webview; it leaves the allocation to GTK, so this is the
/// layer that has to ask.
pub fn reallocate_webview(window: &tauri::WebviewWindow) {
    use gtk::prelude::{ContainerExt, WidgetExt};

    let Ok(gtk_window) = window.gtk_window() else {
        return;
    };
    fn walk(widget: &gtk::Widget) {
        widget.queue_allocate();
        widget.queue_draw();
        if let Some(container) = widget.downcast_ref::<gtk::Container>() {
            for child in container.children() {
                walk(&child);
            }
        }
    }
    use gtk::glib::Cast;
    walk(gtk_window.upcast_ref::<gtk::Widget>());
}

/// Whether the window should carry no titlebar of its own.
///
/// Running as native Wayland, GTK draws one — "Mast", minimise, maximise,
/// close — because GTK3 cannot ask the compositor to. It has no server-side
/// decoration support on Wayland whatsoever; `GTK_CSD=0`, which the first
/// version of this set, affects X11 only, which is why the bar never showed
/// under XWayland and why that variable did nothing here.
///
/// So the choice is the window's own: decorated or not. On a desktop that
/// floats windows the titlebar is how they are moved and closed by pointer,
/// and it stays. On one that tiles it is dead space above a window the
/// manager already places, sizes and closes by keybinding, and the manager
/// draws its own border regardless — so exactly there it goes.
pub fn wants_no_titlebar(desktop: Option<&str>) -> bool {
    desktop.is_some_and(crate::session::is_tiling)
}

/// Whether this process is running under a hypervisor, judged from the DMI
/// strings the firmware exposes. `systemd-detect-virt` would be authoritative
/// but is a subprocess and not universally present; DMI is a file read and is
/// how that tool makes the same call for the common cases.
pub fn in_virtual_machine() -> bool {
    ["/sys/class/dmi/id/product_name", "/sys/class/dmi/id/sys_vendor"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(Path::new(path)).ok())
        .any(|text| dmi_names_hypervisor(&text))
}

/// The DMI strings the common hypervisors write. QEMU reports "QEMU" or
/// "Standard PC" with a vendor of "QEMU"; libvirt's default machine reports
/// "KVM"; the rest name themselves.
pub fn dmi_names_hypervisor(text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "qemu",
        "kvm",
        "virtualbox",
        "vmware",
        "hyper-v",
        "microsoft corporation", // Hyper-V's sys_vendor
        "xen",
        "parallels",
        "bochs",
        "standard pc", // QEMU's Q35 and i440fx product names
    ];
    let lowered = text.to_ascii_lowercase();
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_the_guest_this_was_found_in() {
        // libvirt + QEMU, as the Omarchy VM reports itself.
        assert!(dmi_names_hypervisor("Standard PC (Q35 + ICH9, 2009)\n"));
        assert!(dmi_names_hypervisor("QEMU\n"));
        assert!(dmi_names_hypervisor("KVM\n"));
    }

    #[test]
    fn recognises_the_other_common_hypervisors() {
        // Hyper-V's product_name is the unhelpful "Virtual Machine"; its
        // sys_vendor is what carries the marker, and both files are read.
        for name in [
            "VirtualBox\n",
            "VMware Virtual Platform\n",
            "Microsoft Corporation\n",
            "Xen HVM domU\n",
            "Parallels Virtual Platform\n",
        ] {
            assert!(dmi_names_hypervisor(name), "{name:?} should read as a hypervisor");
        }
    }

    // The false positive is the one that costs: a real laptop wrongly judged
    // a VM loses hardware compositing for no reason, silently, forever.
    #[test]
    fn leaves_real_hardware_alone() {
        for name in [
            "ThinkPad X1 Carbon Gen 11\n",
            "LENOVO\n",
            "Dell Inc.\n",
            "XPS 13 9340\n",
            "Framework Laptop 13\n",
            "ASUSTeK COMPUTER INC.\n",
            "MacBookPro18,3\n",
            "System76\n",
            "To Be Filled By O.E.M.\n",
        ] {
            assert!(!dmi_names_hypervisor(name), "{name:?} must not read as a hypervisor");
        }
    }

    // The titlebar goes only where the manager draws its own border and moves
    // windows itself. A floating desktop would be left with a window nothing
    // can move or close by pointer.
    #[test]
    fn drops_the_titlebar_on_tiling_desktops_only() {
        assert!(wants_no_titlebar(Some("Hyprland")));
        assert!(wants_no_titlebar(Some("sway")));
        assert!(!wants_no_titlebar(Some("GNOME")));
        assert!(!wants_no_titlebar(Some("KDE")));
        assert!(!wants_no_titlebar(None));
    }

    #[test]
    fn ignores_case_and_surrounding_whitespace() {
        assert!(dmi_names_hypervisor("  qemu  "));
        assert!(dmi_names_hypervisor("VIRTUALBOX"));
    }

    #[test]
    fn empty_and_absent_strings_are_not_a_hypervisor() {
        assert!(!dmi_names_hypervisor(""));
        assert!(!dmi_names_hypervisor("\n"));
    }
}
