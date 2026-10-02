/// Longest entity we decode, e.g. `&#x10FFFF;`; bounds the scan for `;`.
const MAX_ENTITY_CHARS: usize = 12;

#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Fallback when Telegram rejects the markup: drop tags, decode entities.
#[must_use]
pub fn plain(html: &str) -> String {
    unescape(&strip_tags(html))
}

fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        let after = tail.get(1..).unwrap_or_default();
        match after.find('>') {
            Some(end) if end > 0 => rest = after.get(end + 1..).unwrap_or_default(),
            Some(_) | None => {
                out.push('<');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        let (before, tail) = rest.split_at(start);
        out.push_str(before);
        if let Some((decoded, used)) = decode_entity(tail) {
            out.push(decoded);
            rest = tail.get(used..).unwrap_or_default();
        } else {
            out.push('&');
            rest = tail.get(1..).unwrap_or_default();
        }
    }
    out.push_str(rest);
    out
}

/// `tail` starts with `&`; returns the character and the byte length of the entity.
fn decode_entity(tail: &str) -> Option<(char, usize)> {
    let (end, _) = tail.char_indices().take(MAX_ENTITY_CHARS).find(|&(_, c)| c == ';')?;
    let name = tail.get(1..end)?;
    let decoded = match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)?
        }
    };
    Some((decoded, end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_characters() {
        assert_eq!(
            escape(r#"a < b & "c" > 'd'"#),
            "a &lt; b &amp; &quot;c&quot; &gt; &#x27;d&#x27;"
        );
    }

    #[test]
    fn plain_drops_tags_and_decodes_entities() {
        assert_eq!(plain("<b>a &amp; b</b> <code>&lt;x&gt;</code>"), "a & b <x>");
    }

    #[test]
    fn plain_keeps_lone_brackets_and_unknown_entities() {
        assert_eq!(plain("a <> b &nbsp c &#65; &#x42;"), "a <> b &nbsp c A B");
    }

    #[test]
    fn plain_round_trips_escape() {
        let text = r#"if a < b && c > "d" { 'e' }"#;
        assert_eq!(plain(&escape(text)), text);
    }
}
