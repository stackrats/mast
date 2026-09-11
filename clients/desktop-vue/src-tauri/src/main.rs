#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // Before anything else: GTK and WebKit read their environment once, at
    // init, and `run()` is where that init happens.
    #[cfg(target_os = "linux")]
    mast_desktop_lib::linux_env::prepare();
    mast_desktop_lib::run();
}
