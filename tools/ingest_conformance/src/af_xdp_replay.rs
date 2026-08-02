//! Shared bounded `AF_PACKET` replay for the privileged `AF_XDP` lanes.
//!
//! Both the deterministic conformance lane and the seeded adversarial data-path
//! lane transmit complete Ethernet frames into one end of a veth pair through a
//! pcap-driven packet-socket sender and assert the transmitted packet count.

use axiusflow_testing::ConformanceHarnessError;
use std::{
    env,
    error::Error,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

pub const ETHERNET_HEADER_BYTES: usize = 14;
pub const AXIUSFLOW_EXPERIMENTAL_ETHERTYPE: [u8; 2] = [0x88, 0xb5];
pub const RECEIVE_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
pub const TRANSMIT_MAC: [u8; 6] = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
const REPLAY_SENDER_SOURCE: &str = include_str!("../send_pcap.py");
static PCAP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Sends complete Ethernet frames and fails unless every frame was transmitted.
pub fn replay_ethernet_frames(
    transmit_interface: &str,
    frames: &[Vec<u8>],
) -> Result<(), ConformanceHarnessError> {
    let path = temporary_pcap_path();
    write_pcap(&path, frames).map_err(|error| {
        context_error(&format!("write replay capture {}", path.display()), error)
    })?;
    let sender = temporary_sender_path();
    fs::write(&sender, REPLAY_SENDER_SOURCE).map_err(|error| {
        context_error(&format!("write replay sender {}", sender.display()), error)
    })?;
    let interpreter = replay_interpreter();
    let output = Command::new(&interpreter)
        .arg(&sender)
        .arg(transmit_interface)
        .arg(&path)
        .output()
        .map_err(|error| {
            context_error(
                &format!(
                    "spawn {} {} {transmit_interface}",
                    interpreter.display(),
                    sender.display()
                ),
                error,
            )
        });
    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&sender);
    let output = output?;
    if !output.status.success() {
        return Err(ConformanceHarnessError::Driver(format!(
            "AF_PACKET replay failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let successful_packets = stdout
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("successful_packets=")
                .and_then(|value| value.trim().parse::<usize>().ok())
        })
        .ok_or_else(|| {
            ConformanceHarnessError::Driver(format!(
                "AF_PACKET replay did not report a successful packet count: {}",
                stdout.trim()
            ))
        })?;
    if successful_packets != frames.len() {
        return Err(ConformanceHarnessError::Driver(format!(
            "AF_PACKET replay sent {successful_packets} of {} fixture packets: {}",
            frames.len(),
            stdout.trim()
        )));
    }
    Ok(())
}

/// Wraps a payload in the lane's deterministic Ethernet addressing.
pub fn ethernet_frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(ETHERNET_HEADER_BYTES.saturating_add(payload.len()));
    frame.extend_from_slice(&RECEIVE_MAC);
    frame.extend_from_slice(&TRANSMIT_MAC);
    frame.extend_from_slice(&AXIUSFLOW_EXPERIMENTAL_ETHERTYPE);
    frame.extend_from_slice(payload);
    frame
}

/// Validates one kernel interface name supplied on the command line.
pub fn require_interface_name(value: &str, role: &str) -> Result<(), Box<dyn Error>> {
    if value.trim().is_empty() || value.len() > 15 || value.chars().any(char::is_whitespace) {
        return Err(format!("AF_XDP {role} interface name is invalid").into());
    }
    Ok(())
}

/// Serializes an error with its operation for the harness error type.
pub fn context_error(operation: &str, error: impl std::fmt::Display) -> ConformanceHarnessError {
    ConformanceHarnessError::Driver(format!("failed to {operation}: {error}"))
}

fn write_pcap(path: &Path, frames: &[Vec<u8>]) -> Result<(), Box<dyn Error>> {
    let mut file = File::create(path)?;
    file.write_all(&0xa1b2_c3d4_u32.to_le_bytes())?;
    file.write_all(&2_u16.to_le_bytes())?;
    file.write_all(&4_u16.to_le_bytes())?;
    file.write_all(&0_i32.to_le_bytes())?;
    file.write_all(&0_u32.to_le_bytes())?;
    file.write_all(&65_535_u32.to_le_bytes())?;
    file.write_all(&1_u32.to_le_bytes())?;
    for (index, frame) in frames.iter().enumerate() {
        let length = u32::try_from(frame.len())?;
        file.write_all(&0_u32.to_le_bytes())?;
        file.write_all(&u32::try_from(index)?.to_le_bytes())?;
        file.write_all(&length.to_le_bytes())?;
        file.write_all(&length.to_le_bytes())?;
        file.write_all(frame)?;
    }
    file.flush()?;
    Ok(())
}

fn temporary_pcap_path() -> PathBuf {
    let sequence = PCAP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "axiusflow-af-xdp-{}-{sequence}.pcap",
        std::process::id()
    ))
}

fn temporary_sender_path() -> PathBuf {
    let sequence = PCAP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    env::temp_dir().join(format!(
        "axiusflow-af-xdp-send-{}-{sequence}.py",
        std::process::id()
    ))
}

/// Resolves the replay interpreter without depending on the inherited `PATH`.
///
/// The privileged harness runs with a read-only root and a reduced environment, where a
/// bare `python3` lookup is not guaranteed. Absolute candidates are preferred, and a
/// `PATH` lookup remains the final fallback.
fn replay_interpreter() -> PathBuf {
    const CANDIDATES: [&str; 3] = ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"];
    CANDIDATES
        .iter()
        .map(Path::new)
        .find(|candidate| candidate.is_file())
        .map_or_else(|| PathBuf::from("python3"), Path::to_path_buf)
}
