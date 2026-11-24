//! Pure text shaping for the reranker input. No I/O, no environment reads.

/// The breadcrumb prepended to a chunk before it is sent to the reranker
/// (`RET-12`): `"{title} › {section}"` plus `" · p.{page}"` when a page is
/// known.
///
/// Empty parts are skipped cleanly: a missing/blank title or section never
/// leaves a dangling `›`, and a page on an otherwise empty breadcrumb never
/// leaves a leading ` · `. Text is trimmed, so a metadata field full of
/// whitespace cannot produce double spaces.
#[must_use]
pub fn breadcrumb(document_title: &str, section_path: Option<&str>, page: Option<u32>) -> String {
    let title = document_title.trim();
    let section = section_path.map(str::trim).filter(|s| !s.is_empty());
    let mut out = match (title.is_empty(), section) {
        (false, Some(section)) => format!("{title} › {section}"),
        (false, None) => title.to_string(),
        (true, Some(section)) => section.to_string(),
        (true, None) => String::new(),
    };
    if let Some(page) = page {
        if !out.is_empty() {
            out.push_str(" · ");
        }
        out.push_str(&format!("p.{page}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_section_and_page() {
        assert_eq!(
            breadcrumb("Kebijakan Cuti", Some("Bab IV › Cuti Tahunan"), Some(12)),
            "Kebijakan Cuti › Bab IV › Cuti Tahunan · p.12"
        );
    }

    #[test]
    fn no_section() {
        assert_eq!(
            breadcrumb("Kebijakan Cuti", None, Some(12)),
            "Kebijakan Cuti · p.12"
        );
        assert_eq!(breadcrumb("Kebijakan Cuti", None, None), "Kebijakan Cuti");
    }

    #[test]
    fn no_page() {
        assert_eq!(
            breadcrumb("Kebijakan Cuti", Some("Bab IV"), None),
            "Kebijakan Cuti › Bab IV"
        );
    }

    #[test]
    fn an_empty_title_does_not_leave_a_dangling_separator() {
        assert_eq!(breadcrumb("", Some("Bab IV"), Some(3)), "Bab IV · p.3");
        assert_eq!(breadcrumb("  ", Some("Bab IV"), None), "Bab IV");
    }

    #[test]
    fn an_empty_section_does_not_leave_a_dangling_separator() {
        assert_eq!(
            breadcrumb("Kebijakan Cuti", Some(""), Some(3)),
            "Kebijakan Cuti · p.3"
        );
        assert_eq!(
            breadcrumb("Kebijakan Cuti", Some("  "), None),
            "Kebijakan Cuti"
        );
    }

    #[test]
    fn a_multibyte_title_and_section_are_kept_intact() {
        assert_eq!(
            breadcrumb(
                "kebijakan cuti インドネシア",
                Some("第四巴 › 年次休暇"),
                Some(3)
            ),
            "kebijakan cuti インドネシア › 第四巴 › 年次休暇 · p.3"
        );
    }

    #[test]
    fn no_parts_but_a_page_is_just_the_page() {
        assert_eq!(breadcrumb("", None, Some(3)), "p.3");
        assert_eq!(breadcrumb("  ", Some("  "), Some(3)), "p.3");
        assert_eq!(breadcrumb("", None, None), "");
    }

    #[test]
    fn whitespace_around_parts_is_trimmed() {
        assert_eq!(
            breadcrumb("  Kebijakan Cuti  ", Some("  Bab IV  "), Some(3)),
            "Kebijakan Cuti › Bab IV · p.3"
        );
    }
}
