//! Recorded dependency review text for the `PostgreSQL` client.

pub const CLIENT_DEPENDENCY_SELECTED: bool = true;
pub const SAFETY_REVIEW: &str = "postgres 0.19.14 is pure Rust with no unsafe FFI; every statement is parameterized and every identifier is validated before interpolation into DDL";
pub const LICENSE_REVIEW: &str =
    "postgres 0.19.14 is MIT OR Apache-2.0 licensed, compatible with this proprietary workspace";
pub const PROVENANCE_REVIEW: &str =
    "postgres 0.19.14 registry checksum recorded in Cargo.lock from crates.io";
pub const MAINTENANCE_REVIEW: &str = "rust-postgres is the maintained synchronous PostgreSQL client with 16M downloads; sqlx and tokio-postgres were rejected because the workspace is synchronous and an async runtime must not leak into the persistence boundary";
pub const BUILD_REVIEW: &str = "postgres compiles on every supported target without platform code; TLS deployment is a runtime configuration, not a build mode";
pub const MISSING_DURABLE_EVIDENCE: &str = "TLS, role-based access, backup/PITR drills, cross-region disaster recovery, query deadlines under load";
