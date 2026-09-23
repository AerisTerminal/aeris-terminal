use std::{env, error::Error, fs, path::PathBuf};

const KIT_DISABLED_ENV: &str = "ASCEIFY_RITHMIC_KIT_DISABLED";
const LOGIN_TEMPLATE_VERSION: &str = "3.9";

fn main() -> Result<(), Box<dyn Error>> {
    println!("cargo:rustc-check-cfg=cfg(rithmic_kit)");
    println!("cargo:rerun-if-env-changed={KIT_DISABLED_ENV}");

    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("Cargo did not provide CARGO_MANIFEST_DIR to the Rithmic build script")?,
    );
    let proto_dir = manifest_dir.join("../../../provider_kit/current/proto");
    println!("cargo:rerun-if-changed={}", proto_dir.display());

    if env::var_os(KIT_DISABLED_ENV).is_some() || !proto_dir.is_dir() {
        return Ok(());
    }

    let mut schemas = fs::read_dir(&proto_dir)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "proto")
        })
        .collect::<Vec<_>>();
    schemas.sort();
    if schemas.is_empty() {
        return Err(format!(
            "Rithmic kit proto directory is empty: {}",
            proto_dir.display()
        )
        .into());
    }

    let change_log = proto_dir.join("change_log");
    println!("cargo:rerun-if-changed={}", change_log.display());
    let change_log_contents = fs::read_to_string(&change_log)?;
    let schema_template_version = change_log_contents
        .lines()
        .find_map(|line| line.split_once("template version"))
        .map(|(_, version)| version.trim())
        .filter(|version| {
            !version.is_empty()
                && version
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == '.')
        })
        .ok_or_else(|| {
            format!(
                "Rithmic kit template version is missing or invalid: {}",
                change_log.display()
            )
        })?;
    for schema in &schemas {
        println!("cargo:rerun-if-changed={}", schema.display());
    }

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut config = prost_build::Config::new();
    config
        .protoc_executable(protoc)
        .disable_comments(["."])
        .include_file("rithmic.protobuf.rs")
        .compile_protos(&schemas, &[proto_dir])?;
    println!("cargo:rustc-env=RITHMIC_SCHEMA_TEMPLATE_VERSION={schema_template_version}");
    println!("cargo:rustc-env=RITHMIC_TEMPLATE_VERSION={LOGIN_TEMPLATE_VERSION}");
    println!("cargo:rustc-cfg=rithmic_kit");
    Ok(())
}
