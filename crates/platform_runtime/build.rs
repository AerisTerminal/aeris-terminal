use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=../../apps/desktop/assets/aeris_assets/aeris.ico");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies manifest dir"));
    let icon = manifest
        .join("../../apps/desktop/assets/aeris_assets/aeris.ico")
        .canonicalize()
        .expect("Aeris Windows icon exists");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let resource = out_dir.join("aeris.rc");
    fs::write(
        &resource,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write Windows icon resource");
    embed_resource::compile_for(&resource, ["aeris_launcher"], embed_resource::NONE)
        .manifest_optional()
        .expect("compile Aeris launcher Windows icon");
}
