//! This host's identity: the one hostname source for health samples, rate
//! limit samples, wake leases, `host_status`, and the cluster instance id.

/// This host's name, or `"unknown"` when the system does not report one.
///
/// Every per-host row legion writes keys on this value, and peers read those
/// rows back by the same key, so all writers must agree on it.
pub fn hostname() -> String {
    sysinfo::System::host_name().unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_is_never_empty() {
        assert!(!hostname().is_empty());
    }
}
