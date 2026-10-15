//! Shared zip reading for the OOXML and EPUB parsers.
//!
//! All three container formats are the same zip with a different set of
//! entries, so they share one open/read pair and differ only in what they look
//! for inside.

use std::io::{Cursor, Read};

use zip::ZipArchive;

use crate::ExtractError;

/// A zip container opened over borrowed bytes.
pub type Archive<'a> = ZipArchive<Cursor<&'a [u8]>>;

/// Open a zip container from bytes.
pub fn open(bytes: &[u8]) -> Result<Archive<'_>, ExtractError> {
    ZipArchive::new(Cursor::new(bytes))
        .map_err(|e| ExtractError::Container(format!("not a zip container: {e}")))
}

/// Read one entry as UTF-8 text. A missing entry is a container error, not a
/// parse error: it means the file is not the format it claims to be.
pub fn read_text(archive: &mut Archive<'_>, name: &str) -> Result<String, ExtractError> {
    let mut entry = archive
        .by_name(name)
        .map_err(|_| ExtractError::Container(format!("the archive has no entry named {name}")))?;
    let mut text = String::new();
    entry
        .read_to_string(&mut text)
        .map_err(|e| ExtractError::Container(format!("could not read {name}: {e}")))?;
    Ok(text)
}

/// Every entry name, in stored order.
#[must_use]
pub fn names(archive: &Archive<'_>) -> Vec<String> {
    archive.file_names().map(str::to_string).collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::testkit::zip_of;

    #[test]
    fn entries_can_be_read_by_name() {
        let bytes = zip_of(&[("word/document.xml", "<w:document/>")]);
        let mut archive = open(&bytes).unwrap();
        assert_eq!(read_text(&mut archive, "word/document.xml").unwrap(), "<w:document/>");
        assert_eq!(names(&archive), ["word/document.xml"]);
    }

    #[test]
    fn a_missing_entry_names_itself() {
        let bytes = zip_of(&[("a.txt", "a")]);
        let mut archive = open(&bytes).unwrap();
        let err = read_text(&mut archive, "ppt/slides/slide1.xml").unwrap_err();
        assert!(
            err.to_string().contains("ppt/slides/slide1.xml"),
            "{err}"
        );
    }

    #[test]
    fn non_zip_bytes_are_a_container_error() {
        let err = open(b"not a zip at all").unwrap_err();
        assert!(matches!(err, ExtractError::Container(_)), "{err}");
    }
}