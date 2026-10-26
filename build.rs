//! Exposes the bundle build number from Cargo.toml
//! (`[package.metadata.zclients] build`) as `ZCLIENTS_BUILD`, so the
//! version shown in the app and the one in Info.plist come from one place.

fn main() {
    println!("cargo:rerun-if-changed=Cargo.toml");
    let manifest = std::fs::read_to_string("Cargo.toml").expect("Cargo.toml");
    let mut in_section = false;
    let mut build = None;
    for line in manifest.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_section = l == "[package.metadata.zclients]";
            continue;
        }
        if in_section && let Some(v) = l.strip_prefix("build").map(|r| r.trim_start()).and_then(|r| r.strip_prefix('=')) {
            build = Some(v.trim().trim_matches('"').to_string());
        }
    }
    let build = build.expect("[package.metadata.zclients] build = <number> in Cargo.toml");
    assert!(build.chars().all(|c| c.is_ascii_digit()), "build number must be digits");
    println!("cargo:rustc-env=ZCLIENTS_BUILD={build}");
    // "Download finished" notifications (UNUserNotificationCenter), called
    // through the Objective-C runtime rather than another binding crate.
    println!("cargo:rustc-link-lib=framework=UserNotifications");
    // NWPathMonitor (C API): retry offline pages when the network returns.
    println!("cargo:rustc-link-lib=framework=Network");
}
