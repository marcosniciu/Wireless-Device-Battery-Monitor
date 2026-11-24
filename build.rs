// Embeds the icon, res/app.manifest (Windows 10 compatibility - required for the taskbar strip's
// layered child window - per-monitor DPI awareness, common controls 6) and version info.
#[path = "res/resources.rs"]
mod resources;

fn main() {
    // The app version lives in the VERSION file (Cargo versions cannot have four parts).
    let root = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let version = std::fs::read_to_string(root.join("VERSION")).unwrap().trim().to_string();
    println!("cargo:rerun-if-changed=VERSION");
    println!("cargo:rustc-env=BH_VERSION={version}");
    let dir = root.join("res");
    resources::embed(&dir, "BatteryHub - battery levels of wireless devices");
}
