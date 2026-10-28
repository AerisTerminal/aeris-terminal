use std::{env, fs, path::PathBuf};

#[path = "release_config_rules.rs"]
mod release_config_rules;

use release_config_rules::{
    RELEASE_CONFIG_VARIABLES, ValidatedReleaseConfig, validate_release_config,
};

fn main() {
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    let windows_target = env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    embed_release_config(&out_dir, windows_target);
    if windows_target {
        embed_windows_icon(&out_dir);
    }
}

fn embed_release_config(out_dir: &std::path::Path, windows_target: bool) {
    println!("cargo:rerun-if-changed=release_config_rules.rs");
    for name in RELEASE_CONFIG_VARIABLES {
        println!("cargo:rerun-if-env-changed={name}");
    }
    let config = validate_release_config(|name| env::var(name).ok(), windows_target)
        .unwrap_or_else(|error| panic!("invalid Aeris release configuration: {error}"));
    fs::write(
        out_dir.join("release_config.rs"),
        render_release_config(config.as_ref()),
    )
    .expect("write the embedded release configuration");
}

fn render_release_config(config: Option<&ValidatedReleaseConfig>) -> String {
    let Some(config) = config else {
        return "static EMBEDDED_RELEASE_CONFIG: Option<ReleaseConfig> = None;\n".to_string();
    };
    let thumbprint = config
        .authenticode_thumbprint
        .as_ref()
        .map(|thumbprint| format!("    authenticode_thumbprint: {thumbprint:?},\n"))
        .unwrap_or_default();
    format!(
        "static EMBEDDED_RELEASE_CONFIG: Option<ReleaseConfig> = Some(ReleaseConfig {{\n    \
         verifying_key: {:?},\n    base_url: {:?},\n    bootstrap_minimum_generation: {},\n    \
         release_identity: {:?},\n    install_generation: {},\n{thumbprint}}});\n",
        config.verifying_key,
        config.base_url,
        config.bootstrap_minimum_generation,
        config.release_identity,
        config.install_generation,
    )
}

fn embed_windows_icon(out_dir: &std::path::Path) {
    println!(
        "cargo:rerun-if-changed=../../apps/desktop/assets/aeris_assets/icons/desktop/windows/aeris.ico"
    );
    let manifest =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies manifest dir"));
    let icon = manifest
        .join("../../apps/desktop/assets/aeris_assets/icons/desktop/windows/aeris.ico")
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
    embed_resource::compile_for(&resource, ["aeris_launcher"], embed_resource::NONE)
        .manifest_optional()
        .expect("compile Aeris launcher Windows icon");
}
