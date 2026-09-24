//! Bounded, decoded body and header previews for the TUI and logs.

use brotli::Decompressor as BrotliDecoder;
use encoding_rs::{Encoding, UTF_8};
use flate2::read::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Read};

use crate::tunnel::limits::{
    LOG_TEXT_MAX_BYTES, PRESENTATION_MAX_EVENT_BYTES, UI_BODY_PREVIEW_BYTES,
};

pub(crate) struct DecodedBodyPreview {
    bytes: Vec<u8>,
    truncated: bool,
    content_encoded: bool,
}

pub(crate) fn body_preview(bytes: &[u8], headers: &HashMap<String, String>) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }

    let decoded = match decode_body_preview(bytes, headers) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Some(format!(
                "[body preview unavailable: {error}; {} bytes received]",
                bytes.len()
            ));
        }
    };

    let content_type = header_value(headers, "content-type");
    if !is_text_body(content_type, &decoded.bytes) {
        let content_type = content_type
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown content type");
        let encoding = header_value(headers, "content-encoding")
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity"));

        return Some(match encoding {
            Some(encoding) => format!(
                "[binary body: {} encoded bytes; content-type: {content_type}; content-encoding: {encoding}]",
                bytes.len()
            ),
            None => format!(
                "[binary body: {} bytes; content-type: {content_type}]",
                bytes.len()
            ),
        });
    }

    let mut preview = match decode_text(&decoded.bytes, content_type, decoded.truncated) {
        Ok(text) => sanitize_preview_text(&text),
        Err(error) => {
            return Some(format!(
                "[text body preview unavailable: {error}; {} bytes received]",
                bytes.len()
            ));
        }
    };

    if decoded.truncated {
        if decoded.content_encoded {
            preview.push_str(&format!(
                "\n… [decoded preview truncated; {} encoded bytes received]",
                bytes.len()
            ));
        } else {
            preview.push_str(&format!(
                "\n… [preview truncated; {} bytes total]",
                bytes.len()
            ));
        }
    }

    Some(preview)
}

pub(crate) fn decode_body_preview(
    bytes: &[u8],
    headers: &HashMap<String, String>,
) -> std::result::Result<DecodedBodyPreview, String> {
    let encodings = content_encodings(headers)?;
    let content_encoded = !encodings.is_empty();
    let mut reader: Box<dyn Read + '_> = Box::new(Cursor::new(bytes));

    // Content codings are listed in application order, so decoding wraps them
    // in reverse order. Keeping this as a reader chain avoids materializing an
    // unbounded intermediate body for stacked encodings.
    for encoding in encodings.iter().rev() {
        reader = match encoding.as_str() {
            "gzip" | "x-gzip" => Box::new(MultiGzDecoder::new(reader)),
            "deflate" => {
                let mut buffered = BufReader::new(reader);
                let is_zlib_wrapped = buffered
                    .fill_buf()
                    .map_err(|error| format!("could not inspect deflate body: {error}"))?
                    .get(..2)
                    .is_some_and(|prefix| is_zlib_header(prefix[0], prefix[1]));

                if is_zlib_wrapped {
                    Box::new(ZlibDecoder::new(buffered))
                } else {
                    // Some older servers use raw DEFLATE despite RFC 9110
                    // defining the coding as a zlib-wrapped stream.
                    Box::new(DeflateDecoder::new(buffered))
                }
            }
            "br" => Box::new(BrotliDecoder::new(reader, 4_096)),
            "zstd" => Box::new(
                zstd::stream::read::Decoder::new(reader)
                    .map_err(|error| format!("could not initialize zstd decoder: {error}"))?,
            ),
            _ => unreachable!("content_encodings validates supported values"),
        };
    }

    let mut limited = reader.take((UI_BODY_PREVIEW_BYTES + 1) as u64);
    let mut preview = Vec::with_capacity(UI_BODY_PREVIEW_BYTES.min(bytes.len()));
    limited
        .read_to_end(&mut preview)
        .map_err(|error| format!("could not decode response body: {error}"))?;
    let truncated = preview.len() > UI_BODY_PREVIEW_BYTES;
    preview.truncate(UI_BODY_PREVIEW_BYTES);

    Ok(DecodedBodyPreview {
        bytes: preview,
        truncated,
        content_encoded,
    })
}

/// Maximum number of stacked content codings accepted in a single
/// `Content-Encoding` header. Real servers apply one coding (rarely two);
/// each accepted coding costs a nested decoder (zstd eagerly allocates
/// ~128 KiB of window) plus one level of recursion on every read, and the
/// header is remote-controlled on the protocol v2 request path.
pub(crate) const MAX_CONTENT_ENCODINGS: usize = 4;

pub(crate) fn content_encodings(
    headers: &HashMap<String, String>,
) -> std::result::Result<Vec<String>, String> {
    let Some(value) = header_value(headers, "content-encoding") else {
        return Ok(Vec::new());
    };

    let codings = value
        .split(',')
        .map(str::trim)
        .filter(|encoding| !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity"));

    // Reject oversized lists before any coding is validated or any decoder is
    // built so the cost of a hostile header stays proportional to one scan.
    let count = codings.clone().count();
    if count > MAX_CONTENT_ENCODINGS {
        return Err(format!(
            "too many content encodings ({count} > {MAX_CONTENT_ENCODINGS})"
        ));
    }

    codings
        .map(|encoding| {
            let normalized = encoding.to_ascii_lowercase();
            match normalized.as_str() {
                "gzip" | "x-gzip" | "deflate" | "br" | "zstd" => Ok(normalized),
                _ => Err(format!("unsupported content-encoding {encoding:?}")),
            }
        })
        .collect()
}

pub(crate) fn is_zlib_header(cmf: u8, flags: u8) -> bool {
    cmf & 0x0f == 8 && (u16::from(cmf) << 8 | u16::from(flags)) % 31 == 0
}

pub(crate) fn header_value<'a>(
    headers: &'a HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

pub(crate) fn is_text_body(content_type: Option<&str>, bytes: &[u8]) -> bool {
    match content_type.and_then(|value| value.split(';').next()) {
        Some(media_type) => is_text_media_type(media_type.trim()),
        None => looks_like_text(bytes),
    }
}

pub(crate) fn is_text_media_type(media_type: &str) -> bool {
    let media_type = media_type.to_ascii_lowercase();

    media_type.starts_with("text/")
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
        || matches!(
            media_type.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/graphql"
                | "application/x-www-form-urlencoded"
                | "application/sql"
                | "application/rtf"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/x-ndjson"
                | "image/svg+xml"
        )
}

pub(crate) fn looks_like_text(bytes: &[u8]) -> bool {
    (0..=3.min(bytes.len())).any(|trim| std::str::from_utf8(&bytes[..bytes.len() - trim]).is_ok())
        && !bytes.contains(&0)
        && bytes
            .iter()
            .filter(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t'))
            .count()
            <= bytes.len() / 100
}

pub(crate) fn decode_text(
    bytes: &[u8],
    content_type: Option<&str>,
    truncated: bool,
) -> std::result::Result<String, String> {
    let charset = content_type.and_then(content_type_charset);
    let encoding = match charset {
        Some(charset) => Encoding::for_label(charset.as_bytes())
            .ok_or_else(|| format!("unsupported charset {charset:?}"))?,
        None => UTF_8,
    };

    let max_trim = if truncated { 8.min(bytes.len()) } else { 0 };
    for trim in 0..=max_trim {
        let candidate = &bytes[..bytes.len() - trim];
        let (decoded, _, had_errors) = encoding.decode(candidate);
        if !had_errors {
            return Ok(decoded.into_owned());
        }
    }

    Err(format!("body is not valid {} text", encoding.name()))
}

pub(crate) fn content_type_charset(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\"', '\'']))
    })
}

pub(crate) fn sanitize_preview_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

pub(crate) fn bounded_presentation_headers<I>(headers: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut retained = HashMap::new();
    let mut bytes = 0usize;

    for (name, value) in headers {
        let pair_bytes = name.len().saturating_add(value.len());
        if bytes.saturating_add(pair_bytes) > PRESENTATION_MAX_EVENT_BYTES / 2 {
            break;
        }
        bytes += pair_bytes;
        retained.insert(name, value);
    }

    retained
}

pub(crate) fn bounded_presentation_text(value: &str) -> String {
    const MAX_TEXT_BYTES: usize = PRESENTATION_MAX_EVENT_BYTES / 8;

    if value.len() <= MAX_TEXT_BYTES {
        return value.to_string();
    }

    let mut end = MAX_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &value[..end])
}

pub(crate) fn bounded_control_neutral_log_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(LOG_TEXT_MAX_BYTES));
    let mut truncated = false;

    for character in value.chars() {
        let rendered = if character.is_control() {
            character.escape_default().collect::<String>()
        } else {
            character.to_string()
        };

        if output.len().saturating_add(rendered.len()) > LOG_TEXT_MAX_BYTES - 3 {
            truncated = true;
            break;
        }
        output.push_str(&rendered);
    }

    if truncated {
        output.push_str("...");
    }
    output
}
