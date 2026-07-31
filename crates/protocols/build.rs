use std::{env, error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("Cargo did not provide CARGO_MANIFEST_DIR to the protocol build script")?,
    );
    let schema_root = manifest_dir.join("../../schemas/protobuf");
    let schemas = [
        schema_root.join("axiusflow/common/v1/event_envelope.proto"),
        schema_root.join("axiusflow/instrument/v1/instrument.proto"),
        schema_root.join("axiusflow/market/v1/market_bar.proto"),
    ];

    println!("cargo:rerun-if-changed={}", schema_root.display());
    for schema in &schemas {
        println!("cargo:rerun-if-changed={}", schema.display());
    }

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut config = prost_build::Config::new();
    config
        .protoc_executable(protoc)
        .include_file("axiusflow.protobuf.rs")
        .compile_protos(&schemas, &[schema_root])?;

    Ok(())
}
