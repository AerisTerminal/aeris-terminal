#[cfg(windows)]
#[test]
fn probe_refuses_overridden_production_root_without_creating_it() {
    use std::process::Command;

    let local_app_data = std::env::temp_dir()
        .join("aeris_validation")
        .join(format!("probe-production-refusal-{}", std::process::id()));
    let root = local_app_data.join("Aeris");
    assert!(!local_app_data.exists(), "test root must start absent");

    let output = Command::new(env!("CARGO_BIN_EXE_market_lifecycle_probe"))
        .env("LOCALAPPDATA", &local_app_data)
        .arg("--validation-root")
        .arg(&root)
        .arg("--out")
        .arg(root.join("probe.json"))
        .output()
        .expect("launch probe");

    assert!(!output.status.success(), "production root must be refused");
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
    assert!(
        stderr.contains(
            "the validation data folder must not equal or contain the production data folder"
        ),
        "expected explicit production-root refusal, got: {stderr}"
    );
    assert!(!root.exists(), "refused root must remain absent");
    assert!(!local_app_data.exists(), "probe must not create its parent");
}
