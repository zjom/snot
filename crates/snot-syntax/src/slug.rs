//! Heading slugs (NOTE_SPEC 7.3).

use unicode_normalization::UnicodeNormalization;

use crate::inline;

/// The slug of heading text: tokens removed, normalised to NFC, lowercased,
/// every run of characters that are not letters or numbers replaced by `-`,
/// and leading and trailing `-` removed. May be empty.
///
/// ```
/// assert_eq!(snot_syntax::slug("Open Risks (Q4) @status:open"), "open-risks-q4");
/// assert_eq!(snot_syntax::slug("Café Über"), "café-über");
/// ```
pub fn slug(text: &str) -> String {
    let scanned = inline::scan(text, 0, 0..text.len(), false);
    let mut without_tokens = String::with_capacity(text.len());
    let mut at = 0;
    for token in &scanned.tokens {
        without_tokens.push_str(&text[at..token.span.start]);
        at = token.span.end;
    }
    without_tokens.push_str(&text[at..]);

    let lower = without_tokens.nfc().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut gap = false;
    for c in lower.chars() {
        if c.is_alphanumeric() {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    out
}
