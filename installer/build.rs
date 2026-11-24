// Checks the embedded program files are there and adds the icon / manifest / version info.
#[path = "../res/resources.rs"]
mod resources;

fn main() {
    println!("cargo:rerun-if-env-changed=BH_PAYLOAD_DIR");
    let dir = std::env::var("BH_PAYLOAD_DIR").expect("set BH_PAYLOAD_DIR to the folder with batteryhub-taskbar.exe");
    for f in ["batteryhub-taskbar.exe"] {
        let p = std::path::Path::new(&dir).join(f);
        assert!(p.exists(), "missing payload file {}", p.display());
        println!("cargo:rerun-if-changed={}", p.display());
    }
    let res = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("..").join("res");
    resources::embed(&res.canonicalize().unwrap(), "BatteryHub Setup");
}
