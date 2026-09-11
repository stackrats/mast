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
