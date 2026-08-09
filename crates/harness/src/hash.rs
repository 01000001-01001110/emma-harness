//! One digest function, used for every hash the harness reports.
//!
//! Kept here rather than pulled from a shared crate because it is six lines and
//! a shared crate would be a dependency edge bought for six lines. The
//! truncation to sixteen hex characters is not security — nothing here
//! authenticates — it is so a digest fits in a log line a human reads.

use sha2::{Digest, Sha256};

pub fn sha256_hex(input: &str) -> String {
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    format!("{:x}", h.finalize())
}

/// The short form that appears in snapshots and logs.
pub fn short(input: &str) -> String {
    sha256_hex(input)[..16].to_string()
}
