use super::{CurlForm, ParsedCurl};

/// A multipart body copied as literal data, rather than as curl `-F` options.
pub enum RawMultipart {
    Fields(Vec<CurlForm>),
    /// The body can't be represented as editable text fields. Keep its framing.
    PreserveBytes,
}

impl ParsedCurl {
    /// Recognize browser-generated multipart bodies with an explicit boundary.
    pub fn raw_multipart(&self) -> Option<RawMultipart> {
        if !self.form.is_empty() || self.body_file.is_some() {
            return None;
        }
        let body = self.body.as_deref()?;
        let content_type = self.header.get("content-type")?;
        let (media_type, parameters) = content_type.split_once(';').unwrap_or((content_type, ""));
        if !media_type
            .trim()
            .eq_ignore_ascii_case("multipart/form-data")
        {
            return None;
        }
        Some(
            boundary(parameters)
                .and_then(|boundary| text_fields(body, boundary))
                .map_or(RawMultipart::PreserveBytes, RawMultipart::Fields),
        )
    }
}

fn boundary(parameters: &str) -> Option<&str> {
    let value = parameters.split(';').find_map(|parameter| {
        let (key, value) = parameter.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("boundary")
            .then_some(value.trim())
    })?;
    let boundary = value
        .strip_prefix('"')
        .and_then(|quoted| quoted.strip_suffix('"'))
        .unwrap_or(value);
    (!boundary.is_empty()
        && boundary.len() <= 70
        && !boundary.ends_with(' ')
        && boundary
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"'()+_,-./:=? ".contains(&byte)))
    .then_some(boundary)
}

fn text_fields(body: &str, boundary: &str) -> Option<Vec<CurlForm>> {
    let delimiter = format!("--{boundary}");
    let after_value = format!("\r\n{delimiter}");
    let mut rest = body.strip_prefix(&delimiter)?;
    let mut fields = Vec::new();
    loop {
        if matches!(rest, "--" | "--\r\n") {
            return Some(fields);
        }
        let (headers, content) = rest.strip_prefix("\r\n")?.split_once("\r\n\r\n")?;
        let name = text_field_name(headers)?;
        // Only a complete delimiter line ends the value. Its leading CRLF
        // belongs to the framing, so earlier CRLFs stay in the field.
        let (end, tail) = content.match_indices(&after_value).find_map(|(at, _)| {
            let tail = &content[at + after_value.len()..];
            (tail.starts_with("\r\n") || matches!(tail, "--" | "--\r\n")).then_some((at, tail))
        })?;
        fields.push(CurlForm {
            key: name.to_owned(),
            value: content[..end].to_owned(),
            is_file: false,
            content_type: None,
        });
        rest = tail;
    }
}

/// The form table can reconstruct a plain Content-Disposition text part.
/// File parts and extra part headers need the original byte representation.
fn text_field_name(headers: &str) -> Option<&str> {
    if headers.contains(['\r', '\n']) {
        return None;
    }
    let (header, disposition) = headers.split_once(':')?;
    let (kind, parameter) = disposition.split_once(';')?;
    let (key, value) = parameter.split_once('=')?;
    if !header.eq_ignore_ascii_case("content-disposition")
        || !kind.trim().eq_ignore_ascii_case("form-data")
        || !key.trim().eq_ignore_ascii_case("name")
    {
        return None;
    }
    let value = value.trim();
    let name = if let Some(quoted) = value.strip_prefix('"') {
        quoted.strip_suffix('"')?
    } else if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
    {
        value
    } else {
        return None;
    };
    (!name.contains(['"', '\\']) && !name.chars().any(char::is_control)).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_complete_boundary_lines_delimit_fields() {
        let body = "--CaseSensitive\r\nContent-Disposition: form-data; name=\"note\"\r\n\r\nline one\r\n--CaseSensitiveSuffix\r\nline two\r\n\r\n--CaseSensitive--\r\n";
        let boundary = boundary("charset=UTF-8; BOUNDARY=\"CaseSensitive\"").unwrap();
        let fields = text_fields(body, boundary).unwrap();
        assert_eq!(fields.len(), 1);
        assert_eq!(
            fields[0].value,
            "line one\r\n--CaseSensitiveSuffix\r\nline two\r\n"
        );
        assert!(text_fields(body.trim_end_matches("--CaseSensitive--\r\n"), boundary).is_none());
    }
}
