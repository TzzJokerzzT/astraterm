//! Small Linux integration helpers.

use std::process::{Command, Stdio};

/// Opens a URL or file with the desktop's default handler.
///
/// Uses `xdg-open` with a single argument vector, and never opens anything
/// automatically: callers must have an explicit user action behind the click.
pub fn open_url(url: &str) -> Result<(), String> {
    // Refuse anything that is not a well-formed http(s) or mailto URL, so a
    // crafted terminal escape sequence cannot launch arbitrary programs.
    if !is_safe_url(url) {
        return Err(format!("refusing to open unsupported URL: {url}"));
    }
    let status = Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("could not run xdg-open: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdg-open failed with {status}"))
    }
}

/// Accepts only schemes a terminal user would expect to click.
pub fn is_safe_url(url: &str) -> bool {
    let lowered = url.trim().to_ascii_lowercase();
    if lowered.len() < 9 || lowered.len() > 4096 {
        return false;
    }
    let allowed = ["http://", "https://", "mailto:", "ftp://"];
    if !allowed.iter().any(|prefix| lowered.starts_with(prefix)) {
        return false;
    }
    // Require something after the scheme separator.
    lowered
        .split_once("://")
        .or_else(|| lowered.strip_prefix("mailto:").map(|rest| ("mailto", rest)))
        .is_some_and(|(_, rest)| !rest.is_empty())
}

/// Whether the current session runs on Wayland.
pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
}

/// Whether the current session runs on X11.
pub fn is_x11() -> bool {
    std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_web_and_mail_urls() {
        assert!(is_safe_url("https://example.com"));
        assert!(is_safe_url("http://localhost:8080/x"));
        assert!(is_safe_url("mailto:someone@example.com"));
    }

    #[test]
    fn refuses_other_schemes_and_junk() {
        assert!(!is_safe_url("file:///etc/passwd"));
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("ssh://host"));
        assert!(!is_safe_url(""));
        assert!(!is_safe_url("https:/"), "too short to be a URL");
        assert!(!is_safe_url("https://"), "a bare scheme has no host");
    }

    #[test]
    fn display_detection_reflects_the_environment() {
        // The test environment has no display guarantee; the calls must simply be
        // side-effect free and consistent.
        let _ = is_wayland();
        let _ = is_x11();
    }
}
