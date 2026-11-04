use super::{Format, Limited};
use std::{io::Write, ops::Range};

fn key(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|ch| ch.is_alphanumeric() || "_.-/".contains(ch))
}
pub(super) fn assignments(text: &str) -> Option<(Format, Vec<u8>)> {
    let (source, export) = text
        .strip_prefix("export ")
        .map_or((text, false), |s| (s, true));
    if source.contains(['\r', '\n']) {
        return None;
    }
    let mut rest = source;
    let single_assignment = source.matches('=').count() == 1;
    let mut ranges: Vec<Range<usize>> = Vec::new();
    let mut spaced = false;
    let mut upper = true;
    while !rest.is_empty() {
        let start = source.len() - rest.len();
        let equals = rest.find('=')?;
        let name = rest[..equals].trim_end();
        if !key(name) {
            return None;
        }
        let padded = equals != name.len();
        spaced |= padded;
        upper &= name.chars().all(|c| !c.is_ascii_lowercase());
        rest = &rest[equals + 1..];
        if padded {
            rest = rest.trim_start();
        }
        if let Some(quote) = rest.chars().next().filter(|c| matches!(c, '\'' | '"')) {
            let mut escaped = false;
            let mut end = None;
            for (index, ch) in rest.char_indices().skip(1) {
                if escaped {
                    escaped = false;
                } else if ch == '\\' && quote == '"' {
                    escaped = true;
                } else if ch == quote {
                    end = Some(index + 1);
                    break;
                }
            }
            rest = &rest[end?..];
            if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
                return None;
            }
        } else {
            let end = if padded && single_assignment {
                rest.len()
            } else {
                rest.find(char::is_whitespace).unwrap_or(rest.len())
            };
            rest = &rest[end..];
        }
        let end = source.len() - rest.len();
        ranges.push(start..end);
        rest = rest.trim_start();
        if (upper || export || spaced) && rest.starts_with('#') {
            ranges.last_mut()?.end = source.len();
            rest = "";
        }
    }
    if ranges.is_empty() {
        return None;
    }
    let format = if ranges.len() > 1 && !spaced && !export && !upper {
        Format::Logfmt
    } else {
        Format::KeyValue
    };
    let mut out = Limited(Vec::new());
    if export {
        out.write_all(b"export ").ok()?;
    }
    for (index, range) in ranges.iter().enumerate() {
        if index > 0 {
            out.write_all(b"\n").ok()?;
        }
        out.write_all(source[range.clone()].as_bytes()).ok()?;
    }
    Some((format, out.0))
}
pub(super) fn query_candidate(text: &str) -> bool {
    if url::Url::parse(text)
        .is_ok_and(|url| matches!(url.scheme(), "http" | "https") && url.query().is_some())
    {
        return true;
    }
    let explicit = text.starts_with('?');
    let text = text.strip_prefix('?').unwrap_or(text);
    (explicit || text.contains('&'))
        && !text.contains("://")
        && !text.contains(['\'', '"'])
        && !text.contains(char::is_whitespace)
        && text.split('&').all(|pair| {
            pair.split_once('=')
                .is_some_and(|(name, _)| !name.is_empty())
        })
}
pub(super) fn query(text: &str) -> Option<Vec<u8>> {
    let url = url::Url::parse(text)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"));
    let query = if let Some(url) = &url {
        url.query()?
    } else {
        text.strip_prefix('?').unwrap_or(text)
    };
    if query.is_empty()
        || query.contains(char::is_whitespace)
        || (!query.contains('&') && url.is_none() && !text.starts_with('?'))
    {
        return None;
    }
    if !query.split('&').all(|pair| {
        pair.split_once('=')
            .is_some_and(|(name, _)| !name.is_empty())
    }) {
        return None;
    }
    let bytes = query.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'%'
            && (index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit())
        {
            return None;
        }
    }
    let mut out = Limited(Vec::new());
    if url.is_some() {
        out.write_all(b"# ").ok()?;
        out.write_all(text.split('?').next()?.as_bytes()).ok()?;
        out.write_all(b"\n").ok()?;
    }
    for (index, (name, value)) in url::form_urlencoded::parse(query.as_bytes()).enumerate() {
        if name.contains('\u{fffd}') || value.contains('\u{fffd}') {
            return None;
        }
        if index > 0 {
            out.write_all(b"\n").ok()?;
        }
        let name = serde_json::to_string(&name).ok()?;
        let value = serde_json::to_string(&value).ok()?;
        out.write_all(name.as_bytes()).ok()?;
        out.write_all(b" = ").ok()?;
        out.write_all(value.as_bytes()).ok()?;
    }
    Some(out.0)
}
