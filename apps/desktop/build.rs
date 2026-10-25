use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/aeris_assets/icons/desktop/windows/aeris.ico");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies manifest dir"))
            .join("assets/aeris_assets/icons/desktop/windows/aeris.ico")
            .canonicalize()
            .expect("Aeris Windows icon exists");
    let version = env::var("CARGO_PKG_VERSION").expect("Cargo supplies the package version");
    let numeric = ["MAJOR", "MINOR", "PATCH"]
        .map(|part| {
            env::var(format!("CARGO_PKG_VERSION_{part}"))
                .expect("Cargo supplies the package version parts")
        })
        .join(",");
    let resource = out_dir.join("aeris.rc");
    // The version block names the process "Aeris Terminal" in Task Manager, the
    // file properties and Windows' installed-apps list.
    fs::write(
        &resource,
        format!(
            r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric},0
PRODUCTVERSION {numeric},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "CompanyName", "Aeris Terminal"
      VALUE "FileDescription", "Aeris Terminal"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "aeris_desktop"
      VALUE "OriginalFilename", "aeris_desktop.exe"
      VALUE "ProductName", "Aeris Terminal"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
            icon = icon.display().to_string().replace('\\', "/"),
        ),
    )
    .expect("write Windows icon and version resource");
    embed_resource::compile_for(&resource, ["aeris_desktop"], embed_resource::NONE)
        .manifest_optional()
        .expect("compile Aeris desktop Windows resources");
}
