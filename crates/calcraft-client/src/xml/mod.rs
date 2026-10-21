//! XML request building and hardened response parsing.

mod dom;
mod multistatus;

pub(crate) use dom::{Element, XmlLimits, parse};
pub(crate) use multistatus::{Multistatus, parse_error_condition, parse_multistatus};

pub(crate) const NS_DAV: &str = "DAV:";
pub(crate) const NS_CALDAV: &str = "urn:ietf:params:xml:ns:caldav";
pub(crate) const NS_CARDDAV: &str = "urn:ietf:params:xml:ns:carddav";
pub(crate) const NS_CALSERVER: &str = "http://calendarserver.org/ns/";
pub(crate) const NS_APPLE_ICAL: &str = "http://apple.com/ns/ical/";

/// Namespace declarations used by every request body we build.
pub(crate) const XMLNS: &str = concat!(
    r#"xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" "#,
    r#"xmlns:card="urn:ietf:params:xml:ns:carddav" "#,
    r#"xmlns:cs="http://calendarserver.org/ns/" xmlns:ic="http://apple.com/ns/ical/""#
);

/// Escapes text for element content and attribute values. Characters that
/// are not allowed in XML 1.0 at all (most C0 controls) are dropped.
pub(crate) fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' => out.push(c),
            '\r' => out.push_str("&#13;"),
            c if (c as u32) < 0x20 || c == '\u{FFFE}' || c == '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(
            escape(r#"<a href="x">Tom & 'Jerry'</a>"#),
            "&lt;a href=&quot;x&quot;&gt;Tom &amp; &apos;Jerry&apos;&lt;/a&gt;"
        );
        assert_eq!(escape("a\u{0}b\u{1b}c\r\n"), "abc&#13;\n");
        let parsed = parse(
            &format!(
                "<d:x xmlns:d=\"DAV:\">{}</d:x>",
                escape("]]> & <![CDATA[ \r\n")
            ),
            &XmlLimits::default(),
        )
        .unwrap();
        assert_eq!(parsed.text, "]]> & <![CDATA[ \r\n");
    }
}
