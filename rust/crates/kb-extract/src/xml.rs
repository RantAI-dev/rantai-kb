//! A forgiving XML reader over the parts of `quick-xml` these parsers need.
//!
//! Office and EPUB files are machine-written but not always well-formed, and the
//! TypeScript parsers (mammoth's DOM shim, `epub2`'s SAX) are lenient. The
//! reader therefore allows a dangling `&`, ignores mismatched end tags, and
//! resolves entity references as text — the three ways real documents differ
//! from the spec.

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::events::BytesText;

use crate::ExtractError;

/// A reader configured the way the document parsers need it.
pub fn reader_from_str(xml: &str) -> Reader<&[u8]> {
    let mut reader = Reader::from_str(xml);
    let config = reader.config_mut();
    // A bare `&` is kept as text instead of failing the whole document.
    config.allow_dangling_amp = true;
    // Mismatched close tags are ignored, as a DOM parse would.
    config.check_end_names = false;
    reader
}

/// The resolved character data of a text event: entities decoded.
pub fn text_of(event: &BytesText<'_>) -> String {
    match event.decode() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => String::new(),
    }
}

/// The resolved text of an entity reference such as `amp` or `#38`.
pub fn reference_of(name: &str) -> String {
    match quick_xml::escape::unescape(&format!("&{name};")) {
        Ok(resolved) => resolved.into_owned(),
        Err(_) => format!("&{name};"),
    }
}

/// The value of an attribute, entities decoded.
pub fn attribute(element: &quick_xml::events::BytesStart<'_>, name: &str) -> Option<String> {
    let attribute = element.try_get_attribute(name).ok()??;
    match attribute.normalized_value(quick_xml::XmlVersion::Explicit1_0) {
        Ok(value) => Some(value.into_owned()),
        Err(_) => None,
    }
}

/// The next event, or `None` at the end of the document.
///
/// Malformed XML is an error rather than a truncated document: the TypeScript
/// parsers either fail the whole extract or hand back a parse error, and a
/// silently half-read document is exactly what must not reach the chunker.
pub fn next_event<'a>(
    reader: &'a mut Reader<&[u8]>,
    buffer: &'a mut Vec<u8>,
) -> Result<Option<Event<'a>>, ExtractError> {
    match reader.read_event_into(buffer) {
        Ok(Event::Eof) => Ok(None),
        Ok(event) => Ok(Some(event)),
        Err(err) => Err(ExtractError::Xml(format!("malformed XML: {err}"))),
    }
}

/// Percent-decoding for EPUB manifest hrefs, which are URIs and may escape
/// spaces and non-ASCII characters.
#[must_use]
pub fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = (bytes[index + 1] as char).to_digit(16);
            let low = (bytes[index + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                let value = u8::try_from(high * 16 + low).unwrap_or(b'%');
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn entities_become_text() {
        let mut reader = reader_from_str("<a>x &amp; y &#38; z</a>");
        let mut buffer = Vec::new();
        let mut text = String::new();
        while let Some(event) = next_event(&mut reader, &mut buffer).unwrap() {
            match event {
                Event::Text(t) => text.push_str(&text_of(&t)),
                Event::GeneralRef(r) => text.push_str(&reference_of(&String::from_utf8_lossy(&r))),
                _ => {}
            }
        }
        assert_eq!(text, "x & y & z");
    }

    #[test]
    fn a_dangling_ampersand_does_not_fail_the_document() {
        let mut reader = reader_from_str("<a>Tom & Jerry</a>");
        let mut buffer = Vec::new();
        let mut text = String::new();
        while let Some(event) = next_event(&mut reader, &mut buffer).unwrap() {
            if let Event::Text(t) = event {
                text.push_str(&text_of(&t));
            }
        }
        assert_eq!(text, "Tom & Jerry");
    }

    #[test]
    fn attributes_are_read_and_unescaped() {
        let mut reader = reader_from_str("<item href=\"a&amp;b.xhtml\"/>");
        let mut buffer = Vec::new();
        let mut href = None;
        while let Some(Event::Start(element) | Event::Empty(element)) = next_event(&mut reader, &mut buffer).unwrap() {
            href = attribute(&element, "href");
        }
        assert_eq!(href.as_deref(), Some("a&b.xhtml"));
    }

    #[test]
    fn percent_escapes_are_decoded() {
        assert_eq!(percent_decode("chapter%201.xhtml"), "chapter 1.xhtml");
        assert_eq!(percent_decode("a%ZZb"), "a%ZZb");
        assert_eq!(percent_decode("caf%C3%A9.xhtml"), "café.xhtml");
    }
}