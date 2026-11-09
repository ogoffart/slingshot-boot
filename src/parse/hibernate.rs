// Copyright © Olivier Goffart
// SPDX-License-Identifier: GPL-3.0-or-later

//! Hibernation: systemd-sleep writes where it hibernated into the
//! `HibernateLocation` variable (systemd's vendor GUID), as JSON, and
//! systemd-hibernate-resume clears it once resumed. While it's set, the
//! system that hibernated must be started again, or its session is lost.

use alloc::string::String;

/// What `HibernateLocation` says about the system that hibernated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hibernated {
    /// `uname -r` of the kernel that hibernated ("6.17.1-arch1-1").
    pub kernel_version: String,
    /// `ID=` of its os-release ("arch", "fedora").
    pub os_id: String,
}

/// Reads the variable's value: JSON, in UTF-16 (as systemd writes its
/// string variables) or UTF-8. `None` if it isn't that.
pub fn parse(data: &[u8]) -> Option<Hibernated> {
    let text = if data.len() >= 2 && data[1] == 0 {
        let units: alloc::vec::Vec<u16> = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(data).into_owned()
    };
    let text = text.trim_end_matches('\0').trim();
    if !text.starts_with('{') {
        return None;
    }
    // A string member, as systemd writes them (no escapes in these values).
    let member = |name: &str| -> String {
        let key = alloc::format!("\"{name}\"");
        let Some(at) = text.find(&key) else { return String::new() };
        let rest = text[at + key.len()..].trim_start().strip_prefix(':').unwrap_or_default().trim_start();
        rest.strip_prefix('"').and_then(|r| r.split('"').next()).unwrap_or_default().into()
    };
    Some(Hibernated { kernel_version: member("kernelVersion"), os_id: member("osReleaseId") })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn systemd_json() {
        let json = r#"{"uuid":"d4a1…","offset":0,"kernelVersion":"6.17.1-arch1-1","osReleaseId":"arch","osReleaseVersionId":null}"#;
        let expected = Hibernated { kernel_version: "6.17.1-arch1-1".into(), os_id: "arch".into() };
        assert_eq!(parse(json.as_bytes()), Some(expected.clone()));
        let utf16: alloc::vec::Vec<u8> = json.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect();
        assert_eq!(parse(&utf16), Some(expected));
        assert_eq!(parse(b"{ \"kernelVersion\" : \"6.1\" }").unwrap().kernel_version, "6.1");
        assert_eq!(parse(b"garbage"), None);
    }
}
