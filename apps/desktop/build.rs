use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/brand_assets/axiusflow.ico");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies manifest dir"))
            .join("assets/brand_assets/axiusflow.ico")
            .canonicalize()
            .expect("Axiusflow Windows icon exists");
    let resource = out_dir.join("axiusflow.rc");
    fs::write(
        &resource,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write Windows icon resource");
    embed_resource::compile_for(&resource, ["axiusflow_desktop"], embed_resource::NONE)
        .manifest_optional()
        .expect("compile Axiusflow desktop Windows icon");
}
