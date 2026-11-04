//! The smallest PDF writer that does the job: text pages, Helvetica, nothing
//! else.
//!
//! The leak matrix needs PDFs because the page-image arm only renders PDFs, and
//! it needs them to carry the canary text in a real text layer so a page that
//! leaks brings a canary chunk with it. Hand-writing PDF bytes is the cheap way
//! to get there without a dependency; the one thing that is easy to get wrong
//! by hand is the cross-reference table, so the offsets are computed from the
//! bytes as they are written rather than typed in.

/// Characters per line before a word wraps. Helvetica at 10 pt averages about
/// 5 pt a character, so 90 fits inside a 612 pt page with 72 pt margins.
const WRAP: usize = 90;
/// Lines per page before the writer starts another one.
const LINES_PER_PAGE: usize = 54;

/// A PDF with one page per entry of `pages`, each page's text laid out as lines
/// (long lines are word-wrapped, and a page that overflows continues on the
/// next one).
#[must_use]
pub fn text_pdf(pages: &[Vec<String>]) -> Vec<u8> {
    let mut laid_out: Vec<Vec<String>> = Vec::new();
    for page in pages {
        let mut lines: Vec<String> = Vec::new();
        for line in page {
            lines.extend(wrap(&ascii(line), WRAP));
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        for chunk in lines.chunks(LINES_PER_PAGE) {
            laid_out.push(chunk.to_vec());
        }
    }
    if laid_out.is_empty() {
        laid_out.push(vec![String::new()]);
    }

    // Object numbers: 1 catalog, 2 pages, 3 font, then (page, contents) pairs.
    let n_pages = laid_out.len();
    let page_obj = |i: usize| 4 + 2 * i;
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    let kids: Vec<String> = (0..n_pages)
        .map(|i| format!("{} 0 R", page_obj(i)))
        .collect();
    objects.push(
        format!(
            "<< /Type /Pages /Kids [{}] /Count {n_pages} >>",
            kids.join(" ")
        )
        .into_bytes(),
    );
    objects.push(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec());
    for (i, lines) in laid_out.iter().enumerate() {
        objects.push(
            format!(
                "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 3 0 R >> >> \
                 /MediaBox [0 0 612 792] /Contents {} 0 R >>",
                page_obj(i) + 1
            )
            .into_bytes(),
        );
        let mut stream = String::from("BT /F1 10 Tf 12 TL 72 720 Td\n");
        for line in lines {
            stream.push_str(&format!("({}) Tj T*\n", escape(line)));
        }
        stream.push_str("ET");
        let mut object = format!("<< /Length {} >>\nstream\n", stream.len()).into_bytes();
        object.extend_from_slice(stream.as_bytes());
        object.extend_from_slice(b"\nendstream");
        objects.push(object);
    }

    let mut out: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::with_capacity(objects.len());
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    // Each entry is exactly 20 bytes, the trailing space included.
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// The standard 14 fonts take WinAnsi; anything else becomes `?` rather than
/// bytes a text extractor would read back as something else.
fn ascii(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '\\' | '(' | ')') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Word-wrap at `width`; a word longer than the line stays whole, so a canary
/// token is never split across two lines.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        if !current.is_empty() && current.len() + 1 + word.len() > width {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// The part that is easy to get wrong by hand: every xref entry must point
    /// at the first byte of its `N 0 obj`, and `startxref` at the table.
    #[test]
    fn the_cross_reference_table_points_at_every_object() {
        let pdf = text_pdf(&[
            vec!["Hello (world)".to_string()],
            vec!["page two".to_string()],
        ]);
        let text = String::from_utf8(pdf.clone()).unwrap();
        let start: usize = text
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(&pdf[start..start + 4], b"xref");
        let table: Vec<&str> = text[start..].lines().skip(2).collect();
        // 1 catalog + 1 pages + 1 font + 2 × (page, contents).
        assert!(table.len() >= 8, "{table:?}");
        for (n, entry) in table.iter().enumerate().take(8).skip(1) {
            let offset: usize = entry[..10].parse().unwrap();
            let header = format!("{n} 0 obj");
            assert_eq!(
                &pdf[offset..offset + header.len()],
                header.as_bytes(),
                "xref entry {n}"
            );
            assert_eq!(entry.len(), 19, "an xref line is 20 bytes with its newline");
        }
        assert!(text.contains("/Count 2"));
        assert!(find(&pdf, b"(Hello \\(world\\)) Tj").is_some());
    }

    #[test]
    fn stream_lengths_are_the_bytes_between_stream_and_endstream() {
        let pdf = text_pdf(&[vec!["a canary vorlaqjx0042 line".to_string()]]);
        let text = String::from_utf8(pdf).unwrap();
        let at = text.find("/Length ").unwrap() + "/Length ".len();
        let length: usize = text[at..].split(' ').next().unwrap().parse().unwrap();
        let body_start = text.find("stream\n").unwrap() + "stream\n".len();
        let body_end = text.find("\nendstream").unwrap();
        assert_eq!(body_end - body_start, length);
    }

    #[test]
    fn a_long_line_wraps_on_spaces_and_never_splits_a_token() {
        let long = format!("{} vorlaqjx0042 {}", "word ".repeat(30), "tail ".repeat(30));
        let lines = wrap(&long, 40);
        assert!(lines.iter().all(|l| l.len() <= 40));
        assert!(
            lines
                .iter()
                .any(|l| l.split(' ').any(|w| w == "vorlaqjx0042"))
        );
    }
}
