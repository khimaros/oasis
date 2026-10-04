//! string helpers: input cleaning, json and url encoding, log line escaping.

use std::fmt::Write;

/// strips control characters and surrounding whitespace. newlines survive
/// only when `multiline`. returns None when empty or longer than `max` bytes.
pub fn clean(input: &str, max: usize, multiline: bool) -> Option<String> {
    let keep = |c: &char| !c.is_control() || (multiline && *c == '\n');
    let cleaned: String = input.chars().filter(keep).collect();
    let trimmed = cleaned.trim();
    (!trimmed.is_empty() && trimmed.len() <= max).then(|| trimmed.to_string())
}

/// quoted json string.
pub fn json(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 2);
    out.push('"');
    for c in input.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// decodes application/x-www-form-urlencoded text.
pub fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut pos = 0;
    while pos < bytes.len() {
        let hex = bytes.get(pos + 1..pos + 3).and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[pos], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                pos += 2;
            }
            (b'+', _) => out.push(b' '),
            (byte, _) => out.push(byte),
        }
        pos += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// splits `a=1&b=2` into decoded pairs.
pub fn parse_params(input: &str) -> Vec<(String, String)> {
    let pair = |part: &str| {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        (url_decode(key), url_decode(value))
    };
    input.split('&').filter(|part| !part.is_empty()).map(pair).collect()
}

/// comparison whose duration does not depend on where the inputs differ.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let diff = a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y));
    a.len() == b.len() && diff == 0
}
