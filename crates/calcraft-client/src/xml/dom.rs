//! A small namespace-resolved element tree built with hard limits.
//!
//! Hostile input is rejected rather than "repaired": DTDs (and therefore any
//! custom entity), unknown entity references, unbound prefixes, mismatched
//! tags, multiple roots, and documents exceeding the depth, element or size
//! limits all fail the whole parse.

use quick_xml::events::Event;
use quick_xml::name::ResolveResult;
use quick_xml::{NsReader, XmlVersion};

use crate::error::Error;

/// Limits for parsing one XML document.
#[derive(Debug, Clone, Copy)]
pub(crate) struct XmlLimits {
    /// Maximum document size in bytes.
    pub max_bytes: usize,
    /// Maximum element nesting depth.
    pub max_depth: usize,
    /// Maximum number of elements.
    pub max_elements: usize,
}

impl Default for XmlLimits {
    fn default() -> Self {
        Self {
            max_bytes: 16 * 1024 * 1024,
            max_depth: 32,
            max_elements: 200_000,
        }
    }
}

/// An element with its resolved namespace URI (empty when unbound).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Element {
    pub ns: String,
    pub name: String,
    /// Attributes without namespace prefix, as `(local name, value)`.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// Concatenated direct text content (character data and CDATA).
    pub text: String,
}

impl Element {
    pub fn is(&self, ns: &str, name: &str) -> bool {
        self.ns == ns && self.name == name
    }

    pub fn child(&self, ns: &str, name: &str) -> Option<&Self> {
        self.children.iter().find(|c| c.is(ns, name))
    }

    pub fn children_named<'a>(
        &'a self,
        ns: &'a str,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Self> + 'a {
        self.children.iter().filter(move |c| c.is(ns, name))
    }

    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Trimmed text, `None` when empty.
    pub fn text_value(&self) -> Option<String> {
        let text = self.text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }
}

fn xml_error(message: impl Into<String>) -> Error {
    Error::Xml(message.into())
}

fn resolve(result: &ResolveResult<'_>) -> Result<String, Error> {
    match result {
        ResolveResult::Bound(ns) => Ok(ns.as_ref().to_owned()),
        ResolveResult::Unbound => Ok(String::new()),
        ResolveResult::Unknown(prefix) => Err(xml_error(format!(
            "unbound namespace prefix {:?}",
            prefix.chars().take(32).collect::<String>()
        ))),
    }
}

/// Parses `input` into an element tree.
pub(crate) fn parse(input: &str, limits: &XmlLimits) -> Result<Element, Error> {
    if input.len() > limits.max_bytes {
        return Err(Error::BodyTooLarge(limits.max_bytes));
    }
    let mut reader = NsReader::from_str(input);
    let config = reader.config_mut();
    config.trim_text(false);
    config.check_end_names = true;
    config.expand_empty_elements = false;
    config.allow_unmatched_ends = false;

    let mut stack: Vec<Element> = Vec::new();
    let mut root: Option<Element> = None;
    let mut elements = 0usize;

    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|e| xml_error(e.to_string()))?;
        // Resolve now: the result borrows the reader.
        let ns = resolve(&ns);
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                let empty = matches!(event, Event::Empty(_));
                if root.is_some() {
                    return Err(xml_error("content after the root element"));
                }
                if stack.len() >= limits.max_depth {
                    return Err(xml_error("nesting too deep"));
                }
                elements += 1;
                if elements > limits.max_elements {
                    return Err(xml_error("too many elements"));
                }
                let ns = ns?;
                let name = start.local_name().as_ref().to_owned();
                let mut attrs = Vec::new();
                for attr in start.attributes() {
                    let attr = attr.map_err(|e| xml_error(e.to_string()))?;
                    let key = attr.key.as_ref();
                    if key == "xmlns" || key.starts_with("xmlns:") {
                        continue;
                    }
                    let (attr_ns, local) = reader.resolver().resolve_attribute(attr.key);
                    if !matches!(attr_ns, ResolveResult::Unbound) {
                        // Namespaced attributes (and xmlns declarations) are
                        // not needed by any DAV property we read.
                        continue;
                    }
                    let value = attr
                        .normalized_value(XmlVersion::Implicit1_0)
                        .map_err(|e| xml_error(e.to_string()))?;
                    attrs.push((local.as_ref().to_owned(), value.into_owned()));
                }
                let element = Element {
                    ns,
                    name,
                    attrs,
                    children: Vec::new(),
                    text: String::new(),
                };
                if empty {
                    close(&mut stack, &mut root, element);
                } else {
                    stack.push(element);
                }
            }
            Event::End(_) => {
                let element = stack.pop().ok_or_else(|| xml_error("unexpected end tag"))?;
                close(&mut stack, &mut root, element);
            }
            Event::Text(text) => {
                let content = text.xml10_content();
                match stack.last_mut() {
                    Some(top) => top.text.push_str(&content),
                    None if content.trim().is_empty() => {}
                    None => return Err(xml_error("text outside the root element")),
                }
            }
            Event::CData(cdata) => match stack.last_mut() {
                Some(top) => top.text.push_str(&cdata.xml10_content()),
                None => return Err(xml_error("CDATA outside the root element")),
            },
            Event::GeneralRef(reference) => {
                let top = stack
                    .last_mut()
                    .ok_or_else(|| xml_error("reference outside the root element"))?;
                if reference.is_char_ref() {
                    let c = reference
                        .resolve_char_ref()
                        .map_err(|e| xml_error(e.to_string()))?
                        .ok_or_else(|| xml_error("invalid character reference"))?;
                    top.text.push(c);
                } else {
                    let c = match reference.as_ref() {
                        "lt" => '<',
                        "gt" => '>',
                        "amp" => '&',
                        "apos" => '\'',
                        "quot" => '"',
                        _ => return Err(xml_error("unknown entity reference")),
                    };
                    top.text.push(c);
                }
            }
            Event::DocType(_) => return Err(xml_error("DTDs are not allowed")),
            Event::Decl(_) | Event::PI(_) | Event::Comment(_) => {}
            Event::Eof => break,
        }
    }
    if !stack.is_empty() {
        return Err(xml_error("unexpected end of document"));
    }
    root.ok_or_else(|| xml_error("empty document"))
}

fn close(stack: &mut [Element], root: &mut Option<Element>, element: Element) {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None => *root = Some(element),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_default(input: &str) -> Result<Element, Error> {
        parse(input, &XmlLimits::default())
    }

    #[test]
    fn resolves_arbitrary_prefixes_and_default_namespace() {
        let a = parse_default(r#"<D:multistatus xmlns:D="DAV:"><D:response/></D:multistatus>"#)
            .unwrap();
        let b = parse_default(r#"<multistatus xmlns="DAV:"><response/></multistatus>"#).unwrap();
        let c = parse_default(r#"<x0:multistatus xmlns:x0="DAV:"><x0:response/></x0:multistatus>"#)
            .unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert!(a.is(super::super::NS_DAV, "multistatus"));
        assert!(a.child("DAV:", "response").is_some());
    }

    #[test]
    fn entities_and_attributes() {
        let e = parse_default(
            r#"<?xml version="1.0"?><!-- c --><a xmlns:x="urn:x" n="&lt;v&gt;" x:o="1">A&amp;B&#13;&#x41;<![CDATA[<raw>]]></a>"#,
        )
        .unwrap();
        assert_eq!(e.text, "A&B\rA<raw>");
        assert_eq!(e.attr("n"), Some("<v>"));
        assert_eq!(e.attrs.len(), 1);
    }

    #[test]
    fn rejects_hostile_documents() {
        let deep = format!("{}{}", "<a>".repeat(100), "</a>".repeat(100));
        let cases = [
            r#"<!DOCTYPE a [<!ENTITY x "boom">]><a>&x;</a>"#.to_owned(),
            r#"<!DOCTYPE a SYSTEM "file:///etc/passwd"><a/>"#.to_owned(),
            "<a>&x;</a>".to_owned(),
            "<a>&#0;</a>".to_owned(),
            "<p:a/>".to_owned(),
            "<a><b></a></b>".to_owned(),
            "<a>".to_owned(),
            "<a/><b/>".to_owned(),
            "text<a/>".to_owned(),
            String::new(),
            deep,
        ];
        for case in cases {
            assert!(parse_default(&case).is_err(), "{case}");
        }
        let many = format!("<a>{}</a>", "<b/>".repeat(20));
        let limits = XmlLimits {
            max_elements: 10,
            ..XmlLimits::default()
        };
        assert!(parse(&many, &limits).is_err());
        let limits = XmlLimits {
            max_bytes: 10,
            ..XmlLimits::default()
        };
        assert!(matches!(
            parse(&many, &limits),
            Err(Error::BodyTooLarge(10))
        ));
    }
}
