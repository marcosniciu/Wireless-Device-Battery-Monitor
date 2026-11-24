// Shared by build.rs and installer/build.rs: writes a .rc file with the BatteryHub icon, the
// application manifest and version information, and links it into every binary of the package.
// Paths are made absolute here, so rc.exe finds the files from any directory.
#[allow(dead_code)]
pub fn embed(res_dir: &std::path::Path, description: &str) {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = res_dir.join("batteryhub.ico");
    let manifest = res_dir.join("app.manifest");
    println!("cargo:rerun-if-changed={}", ico.display());
    println!("cargo:rerun-if-changed={}", manifest.display());

    // The app version from the VERSION file next to res/ (may have four parts).
    let version_file = res_dir.parent().unwrap().join("VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());
    let version = std::fs::read_to_string(&version_file).unwrap().trim().to_string();
    let mut nums: Vec<String> = version.split('.').map(str::to_string).collect();
    nums.resize(4, "0".into());
    let comma = nums.join(",");
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let rc = format!(
        r#"#pragma code_page(65001)
1 ICON "{ico}"
1 24 "{manifest}"
1 VERSIONINFO
FILEVERSION {comma}
PRODUCTVERSION {comma}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "BatteryHub"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "ProductName", "BatteryHub"
      VALUE "ProductVersion", "{version}"
      VALUE "LegalCopyright", "GPL-3.0-or-later"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = esc(&ico),
        manifest = esc(&manifest),
    );
    let rc_path = out.join("batteryhub.rc");
    std::fs::write(&rc_path, rc).unwrap();
    embed_resource::compile(&rc_path, embed_resource::NONE).manifest_required().unwrap();
}
