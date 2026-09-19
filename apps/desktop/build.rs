use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/tradingplot_assets/tradingplot.ico");
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo supplies OUT_DIR"));
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo supplies manifest dir"))
            .join("assets/tradingplot_assets/tradingplot.ico")
            .canonicalize()
            .expect("TradingPlot Windows icon exists");
    let resource = out_dir.join("tradingplot.rc");
    fs::write(
        &resource,
        format!(
            "1 ICON \"{}\"\n",
            icon.display().to_string().replace('\\', "/")
        ),
    )
    .expect("write Windows icon resource");
    embed_resource::compile_for(&resource, ["tradingplot_desktop"], embed_resource::NONE)
        .manifest_optional()
        .expect("compile TradingPlot desktop Windows icon");
}
