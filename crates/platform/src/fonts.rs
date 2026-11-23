//! Font discovery through fontconfig.
//!
//! `fc-list` is the portable way to enumerate families on Linux without linking
//! fontconfig directly. Failures are reported as an empty list: a missing font
//! database must not prevent the terminal from starting.

use std::process::{Command, Stdio};

/// Lists installed font families, sorted and de-duplicated.
pub fn list_families() -> Vec<String> {
    let output = Command::new("fc-list")
        .args(["--format", "%{family}\n"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut families: Vec<String> = text
        .lines()
        .flat_map(|line| line.split(','))
        .map(|family| family.trim().to_owned())
        .filter(|family| !family.is_empty())
        .collect();
    families.sort();
    families.dedup();
    families
}

/// Whether a specific family is installed.
pub fn has_family(family: &str) -> bool {
    list_families()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(family))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_never_panics() {
        let families = list_families();
        // The list may be empty on a bare system; it must never panic.
        assert!(families.len() < 100_000);
    }

    #[test]
    fn known_missing_family_is_reported_missing() {
        assert!(!has_family("Definitely Not An Installed Font Family"));
    }
}
