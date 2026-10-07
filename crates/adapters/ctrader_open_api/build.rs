use std::{env, error::Error, path::PathBuf};

fn main() -> Result<(), Box<dyn Error>> {
    let manifest_dir = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR")
            .ok_or("Cargo did not provide CARGO_MANIFEST_DIR to the cTrader build script")?,
    );
    let proto_dir = manifest_dir.join("../../../third_party/ctrader_open_api");
    let schemas = [
        "OpenApiCommonMessages.proto",
        "OpenApiCommonModelMessages.proto",
        "OpenApiMessages.proto",
        "OpenApiModelMessages.proto",
    ]
    .map(|name| proto_dir.join(name));
    for schema in &schemas {
        println!("cargo:rerun-if-changed={}", schema.display());
    }

    prost_build::Config::new()
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?)
        .include_file("ctrader.protobuf.rs")
        .compile_protos(&schemas, &[proto_dir])?;
    Ok(())
}
