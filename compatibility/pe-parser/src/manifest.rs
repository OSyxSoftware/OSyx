//! The execution level an application manifest requests.
//!
//! A Windows executable states in its RT_MANIFEST resource whether it must
//! run with administrator rights:
//! `<requestedExecutionLevel level="requireAdministrator" .../>`. The scan is
//! a byte search for that element's `level` attribute; the manifest is not
//! validated as XML (the loader has no use for the rest of it).

/// `requestedExecutionLevel level=...`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionLevel {
    /// Run with the rights of the caller (also: no element, no manifest).
    AsInvoker,
    /// Use the most rights the user has; an administrator is elevated.
    HighestAvailable,
    /// Always run elevated.
    RequireAdministrator,
}

impl ExecutionLevel {
    /// Whether launching the program needs the user's consent to elevate.
    pub fn needs_elevation(self) -> bool {
        self != ExecutionLevel::AsInvoker
    }
}

/// The level requested by `manifest` (UTF-8 text; a UTF-16 manifest is read
/// as no request).
pub fn execution_level(manifest: &[u8]) -> ExecutionLevel {
    let Some(at) = find(manifest, b"requestedExecutionLevel", 0) else { return ExecutionLevel::AsInvoker };
    let tag_end = manifest[at..].iter().position(|&b| b == b'>').map_or(manifest.len(), |n| at + n);
    let tag = &manifest[at..tag_end];
    // `level` as a whole attribute name: not the tail of another word.
    let mut from = 0;
    while let Some(i) = find(tag, b"level", from) {
        from = i + 1;
        if i > 0 && !matches!(tag[i - 1], b' ' | b'\t' | b'\r' | b'\n') {
            continue;
        }
        let mut j = i + 5;
        while j < tag.len() && matches!(tag[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        if tag.get(j) != Some(&b'=') {
            continue;
        }
        j += 1;
        while j < tag.len() && matches!(tag[j], b' ' | b'\t' | b'\r' | b'\n') {
            j += 1;
        }
        let Some(&quote) = tag.get(j).filter(|q| matches!(q, b'"' | b'\'')) else { continue };
        let value = &tag[j + 1..];
        let end = value.iter().position(|&b| b == quote).unwrap_or(value.len());
        let value = &value[..end];
        return if value.eq_ignore_ascii_case(b"requireAdministrator") {
            ExecutionLevel::RequireAdministrator
        } else if value.eq_ignore_ascii_case(b"highestAvailable") {
            ExecutionLevel::HighestAvailable
        } else {
            ExecutionLevel::AsInvoker
        };
    }
    ExecutionLevel::AsInvoker
}

/// First case-sensitive occurrence of `needle` in `hay` at or after `from`.
fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(xml: &str) -> ExecutionLevel {
        execution_level(xml.as_bytes())
    }

    #[test]
    fn reads_the_requested_level() {
        let nsis = r#"<trustInfo><security><requestedPrivileges><requestedExecutionLevel level="requireAdministrator" uiAccess="false"/></requestedPrivileges></security></trustInfo>"#;
        assert_eq!(level(nsis), ExecutionLevel::RequireAdministrator);
        assert_eq!(level(r#"<requestedExecutionLevel uiAccess="false" level='highestAvailable' />"#), ExecutionLevel::HighestAvailable);
        assert_eq!(level(r#"<requestedExecutionLevel level="asInvoker" uiAccess="false"/>"#), ExecutionLevel::AsInvoker);
        assert_eq!(level(r#"<requestedExecutionLevel level = "REQUIREADMINISTRATOR"/>"#), ExecutionLevel::RequireAdministrator);
    }

    #[test]
    fn no_request_means_the_callers_rights() {
        assert_eq!(level("<assembly></assembly>"), ExecutionLevel::AsInvoker);
        assert_eq!(level(""), ExecutionLevel::AsInvoker);
        // `level` inside another attribute name is not the attribute.
        assert_eq!(level(r#"<requestedExecutionLevel sublevel="requireAdministrator"/>"#), ExecutionLevel::AsInvoker);
        // A level elsewhere in the file does not count.
        assert_eq!(level(r#"<x level="requireAdministrator"/><requestedExecutionLevel/>"#), ExecutionLevel::AsInvoker);
        assert!(!ExecutionLevel::AsInvoker.needs_elevation());
        assert!(ExecutionLevel::HighestAvailable.needs_elevation());
    }

    #[test]
    fn truncated_input_does_not_panic() {
        for cut in 0..60 {
            let s = r#"<requestedExecutionLevel level="requireAdministrator"/>"#;
            let _ = execution_level(&s.as_bytes()[..cut.min(s.len())]);
        }
    }
}
