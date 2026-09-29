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
    let resource = out_dir.join("aeris.rc");
    fs::write(
        &resource,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write Windows icon resource");
    embed_resource::compile_for(&resource, ["aeris_desktop"], embed_resource::NONE)
        .manifest_optional()
        .expect("compile Aeris desktop Windows icon");
}
