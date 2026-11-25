use std::ops::Range;

/// Links indexed by logical line in the text loaded into the editor.
#[derive(Default)]
pub struct PageLinks {
    lines: Vec<LinkLine>,
}

pub struct LinkLine {
    index: u32,
    pub bytes: Range<usize>,
    pub links: Vec<Link>,
}

pub struct Link {
    pub columns: Range<usize>,
    pub url: String,
}

impl PageLinks {
    pub fn new(text: &str, syntax: &str, partial_line: bool) -> Self {
        let mut lines = Vec::new();
        let mut offset = 0;
        for (index, line) in text.split_inclusive('\n').enumerate() {
            let end = offset + line.len();
            let links = links_in(line, syntax, partial_line && end == text.len());
            if !links.is_empty() {
                lines.push(LinkLine {
                    index: index as u32,
                    bytes: offset..end,
                    links,
                });
            }
            offset = end;
        }
        Self { lines }
    }

    pub fn line(&self, index: u32) -> Option<&LinkLine> {
        self.lines
            .binary_search_by_key(&index, |line| line.index)
            .ok()
            .map(|at| &self.lines[at])
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

fn links_in(mut text: &str, syntax: &str, partial_line: bool) -> Vec<Link> {
    let mut links = Vec::new();
    let mut column = 0;

    while let Some(start) = text.as_bytes().iter().enumerate().find_map(|(i, &byte)| {
        if !matches!(byte, b'h' | b'H') || (i > 0 && text.as_bytes()[i - 1].is_ascii_alphanumeric())
        {
            return None;
        }
        let starts_with = |scheme: &[u8]| {
            text.as_bytes()
                .get(i..i + scheme.len())
                .is_some_and(|part| part.eq_ignore_ascii_case(scheme))
        };
        (starts_with(b"https://") || starts_with(b"http://")).then_some(i)
    }) {
        column += text[..start].chars().count();
        let candidate = &text[start..];
        let end = candidate
            .find(|c: char| {
                c.is_whitespace()
                    || c.is_control()
                    || matches!(c, '<' | '>' | '"' | '\'' | '`' | '\\')
            })
            .unwrap_or(candidate.len());
        let raw = &candidate[..end];
        let quoted = start.checked_sub(1).is_some_and(|before| {
            let quote = text.as_bytes()[before];
            matches!(quote, b'"' | b'\'' | b'`') && candidate.as_bytes().get(end) == Some(&quote)
        });
        let url = if quoted { raw } else { trim_punctuation(raw) };
        if !(partial_line && end == candidate.len()) && url.len() <= crate::consts::URL_GUARD_LEN {
            let target = if matches!(syntax, "html" | "xml") && url.contains('&') {
                rustman_formats::html::html_to_text(url)
            } else {
                url.to_owned()
            };
            if let Ok(parsed) = reqwest::Url::parse(&target)
                && matches!(parsed.scheme(), "http" | "https")
                && parsed.has_host()
                && parsed.as_str().len() <= crate::consts::URL_GUARD_LEN
            {
                links.push(Link {
                    columns: column..column + url.chars().count(),
                    url: parsed.into(),
                });
            }
        }
        column += raw.chars().count();
        text = &candidate[end..];
    }
    links
}

fn trim_punctuation(url: &str) -> &str {
    let mut parentheses = 0_i32;
    let mut brackets = 0_i32;
    let mut braces = 0_i32;
    for byte in url.bytes() {
        match byte {
            b'(' => parentheses += 1,
            b')' => parentheses -= 1,
            b'[' => brackets += 1,
            b']' => brackets -= 1,
            b'{' => braces += 1,
            b'}' => braces -= 1,
            _ => {}
        }
    }
    let mut end = url.len();
    while let Some(byte) = url.as_bytes().get(..end).and_then(|text| text.last()) {
        match byte {
            b'.' | b',' | b';' | b'!' => {}
            b')' if parentheses < 0 => parentheses += 1,
            b']' if brackets < 0 => brackets += 1,
            b'}' if braces < 0 => braces += 1,
            _ => break,
        }
        end -= 1;
    }
    &url[..end]
}
