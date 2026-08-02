//! Build-time DPDK discovery for the `native` feature.
//!
//! The native lifecycle path links the exact reviewed DPDK release and refuses to
//! build against any other version. Portable builds never enter this path.

use std::{env, process::Command};

const EXPECTED_DPDK_VERSION: &str = "25.11.0";
const DPDK_LINK_LIBRARIES: [&str; 10] = [
    "rte_bus_vdev",
    "rte_eal",
    "rte_ethdev",
    "rte_kvargs",
    "rte_log",
    "rte_mbuf",
    "rte_mempool",
    "rte_net",
    "rte_ring",
    "rte_telemetry",
];

fn main() {
    if env::var_os("CARGO_FEATURE_NATIVE").is_none()
        || env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux")
    {
        return;
    }
    let output = Command::new("pkg-config")
        .args(["--modversion", "libdpdk"])
        .output()
        .expect("pkg-config must run for the DPDK native feature");
    assert!(
        output.status.success(),
        "libdpdk {EXPECTED_DPDK_VERSION} development metadata is required for the DPDK native feature"
    );
    let version = String::from_utf8(output.stdout)
        .expect("libdpdk modversion must be UTF-8")
        .trim()
        .to_string();
    assert_eq!(
        version, EXPECTED_DPDK_VERSION,
        "DPDK native feature requires exactly libdpdk {EXPECTED_DPDK_VERSION}, found {version}"
    );
    let raw_libdir = Command::new("pkg-config")
        .args(["--variable=libdir", "libdpdk"])
        .output()
        .ok()
        .filter(|search| search.status.success())
        .and_then(|search| String::from_utf8(search.stdout).ok())
        .map(|libdir| libdir.trim().to_string())
        .filter(|libdir| !libdir.is_empty())
        .expect("libdpdk libdir must be discoverable for the DPDK native feature");
    let libdir = match env::var("PKG_CONFIG_SYSROOT_DIR") {
        Ok(sysroot) if !sysroot.is_empty() && !raw_libdir.starts_with(&sysroot) => {
            format!("{sysroot}{raw_libdir}")
        }
        _ => raw_libdir,
    };
    println!("cargo:rustc-link-search=native={libdir}");
    // The net_ring PMD self-registers from a constructor, so an `--as-needed`
    // link would drop it; EAL loads it explicitly through `-d` at runtime.
    println!("cargo:rustc-env=AXIUSFLOW_DPDK_LIBDIR={libdir}");
    for library in DPDK_LINK_LIBRARIES {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
}
