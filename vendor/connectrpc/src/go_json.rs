//! Go protojson syntax-error text for JSON request payloads.
//!
//! The pinned oracle (envd 0.10.2, connect-go 1.19.2) answers a malformed
//! JSON request with the connect-go wrapping
//! `unmarshal message: unmarshal into *<package>.<Message>: <detail>`
//! (`envelope.go:312` and `codec.go:181`), where `<detail>` is produced by
//! `google.golang.org/protobuf/internal/encoding/json`. This module reproduces
//! that decoder's error surface — token classification, the `errRegexp`
//! 32-character truncation, line/column positions, and the unexpected-EOF
//! cases — so the wire text matches the oracle byte for byte.
//!
//! Only syntax errors are reproduced. A syntactically valid document that
//! fails the *schema* (unknown field, wrong value type, bad enum) has no
//! counterpart here: the caller keeps serde's message for that class, and the
//! campaign records it as a disclosed gap.

/// A Go JSON-decoder failure, rendered later with the document for position.
pub(crate) enum GoJsonError {
    /// `proto: unexpected EOF` — the document ended inside a value.
    UnexpectedEof,
    /// `proto: syntax error (line L:C): <detail>`.
    ///
    /// `replacement` is set when the offending token was decoded from bytes
    /// that are not valid UTF-8: Go's `encoding/json` then writes the
    /// replacement rune as the six-character U+FFFD escape, not as raw UTF-8.
    Syntax {
        pos: usize,
        detail: String,
        replacement: bool,
    },
}

impl GoJsonError {
    pub(crate) fn text(&self, data: &[u8]) -> String {
        match self {
            Self::UnexpectedEof => "proto: unexpected EOF".to_owned(),
            Self::Syntax { pos, detail, .. } => {
                let (line, column) = position(data, *pos);
                format!("proto: syntax error (line {line}:{column}): {detail}")
            }
        }
    }

    /// Whether the rendered text contains a U+FFFD that stands in for invalid
    /// UTF-8, which Go's JSON encoder would escape instead of writing raw.
    pub(crate) fn escapes_replacement(&self) -> bool {
        matches!(
            self,
            Self::Syntax {
                replacement: true,
                ..
            }
        )
    }
}

/// Return the protojson syntax error for `data`, or `None` when the document
/// is syntactically valid JSON.
pub(crate) fn syntax_error(data: &[u8]) -> Option<GoJsonError> {
    let mut scanner = Scanner { data, index: 0 };
    scanner.document().err()
}

/// Go `json.Decoder.Position` (decode.go:220-228): lines are newline counts
/// plus one; columns are rune counts after the last newline plus one.
fn position(data: &[u8], index: usize) -> (usize, usize) {
    let prefix = &data[..index.min(data.len())];
    let line = prefix.iter().filter(|byte| **byte == b'\n').count() + 1;
    let line_start = prefix
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |offset| offset + 1);
    let column = String::from_utf8_lossy(&prefix[line_start..])
        .chars()
        .count()
        + 1;
    (line, column)
}

/// Go `errRegexp = ^([-+._a-zA-Z0-9]{1,32}|.)`: up to 32 class characters,
/// otherwise exactly one rune (invalid UTF-8 renders as U+FFFD).
///
/// The flag is true when that rune came from an invalid UTF-8 byte sequence.
fn error_token(data: &[u8], index: usize) -> (String, bool) {
    let rest = &data[index..];
    let class =
        |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.' | b'_');
    let length = rest
        .iter()
        .take(32)
        .take_while(|byte| class(**byte))
        .count();
    if length > 0 {
        return (String::from_utf8_lossy(&rest[..length]).into_owned(), false);
    }
    // One rune: lossy decoding yields U+FFFD for an invalid byte, matching
    // Go's rune-based regexp match on a byte string.
    let head = &rest[..rest.len().min(4)];
    let invalid = std::str::from_utf8(head)
        .err()
        .is_some_and(|error| error.valid_up_to() == 0);
    let token = String::from_utf8_lossy(head)
        .chars()
        .next()
        .map(String::from)
        .unwrap_or_default();
    (token, invalid)
}

fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

struct Scanner<'a> {
    data: &'a [u8],
    index: usize,
}

impl Scanner<'_> {
    fn document(&mut self) -> Result<(), GoJsonError> {
        self.skip_space();
        match self.data.get(self.index) {
            None => Err(GoJsonError::UnexpectedEof),
            Some(b'{') => {
                self.object()?;
                self.skip_space();
                if self.index < self.data.len() {
                    return Err(self.unexpected_token());
                }
                Ok(())
            }
            // `parseNext` yields a token for these starts, and protojson's
            // top-level `unexpectedTokenError` names it; anything else is a
            // failed token parse (`invalid value ...`).
            Some(_) => match self.token_text() {
                Some(token) => Err(GoJsonError::Syntax {
                    pos: self.index,
                    detail: format!("unexpected token {token}"),
                    replacement: false,
                }),
                None => Err(self.invalid_value()),
            },
        }
    }

    fn object(&mut self) -> Result<(), GoJsonError> {
        self.index += 1;
        loop {
            self.skip_space();
            match self.data.get(self.index) {
                None => return Err(GoJsonError::UnexpectedEof),
                Some(b'}') => {
                    self.index += 1;
                    return Ok(());
                }
                Some(b'"') => {}
                Some(_) => return Err(self.invalid_value()),
            }
            self.string()?;
            self.skip_space();
            match self.data.get(self.index) {
                None => return Err(GoJsonError::UnexpectedEof),
                Some(b':') => self.index += 1,
                Some(byte) => {
                    return Err(GoJsonError::Syntax {
                        pos: self.index,
                        detail: format!(
                            "unexpected character {}, missing \":\" after field name",
                            char_text(*byte)
                        ),
                        replacement: false,
                    });
                }
            }
            self.value()?;
            self.skip_space();
            match self.data.get(self.index) {
                None => return Err(GoJsonError::UnexpectedEof),
                Some(b',') => self.index += 1,
                Some(b'}') => {
                    self.index += 1;
                    return Ok(());
                }
                Some(_) => return Err(self.invalid_value()),
            }
        }
    }

    fn array(&mut self) -> Result<(), GoJsonError> {
        self.index += 1;
        self.skip_space();
        if self.data.get(self.index) == Some(&b']') {
            self.index += 1;
            return Ok(());
        }
        loop {
            self.value()?;
            self.skip_space();
            match self.data.get(self.index) {
                None => return Err(GoJsonError::UnexpectedEof),
                Some(b',') => self.index += 1,
                Some(b']') => {
                    self.index += 1;
                    return Ok(());
                }
                Some(_) => return Err(self.invalid_value()),
            }
        }
    }

    fn value(&mut self) -> Result<(), GoJsonError> {
        self.skip_space();
        match self.data.get(self.index) {
            None => Err(GoJsonError::UnexpectedEof),
            Some(b'"') => self.string(),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'n') => self.literal(b"null"),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            // A token that is not a value (closing brace/bracket, comma)
            // reaches protojson's unexpected-token arm, not its invalid-value
            // arm (`decode.go:130-166`).
            Some(b'}' | b']' | b',') => Err(self.unexpected_token()),
            Some(_) => Err(self.invalid_value()),
        }
    }

    /// Go `matchWithDelim`: the literal must be followed by a delimiter.
    fn literal(&mut self, word: &[u8]) -> Result<(), GoJsonError> {
        if literal_is_delimited(&self.data[self.index..], word) {
            self.index += word.len();
            return Ok(());
        }
        Err(self.invalid_value())
    }

    fn number(&mut self) -> Result<(), GoJsonError> {
        let rest = &self.data[self.index..];
        let length = number_length(rest);
        if length == 0 {
            return Err(self.invalid_value());
        }
        self.index += length;
        Ok(())
    }

    fn string(&mut self) -> Result<(), GoJsonError> {
        self.index += 1;
        loop {
            match self.data.get(self.index) {
                None => return Err(GoJsonError::UnexpectedEof),
                Some(b'"') => {
                    self.index += 1;
                    return Ok(());
                }
                Some(b'\\') => {
                    self.index += 1;
                    match self.data.get(self.index) {
                        None => return Err(GoJsonError::UnexpectedEof),
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => {
                            self.index += 1;
                        }
                        Some(b'u') => {
                            self.index += 1;
                            for _ in 0..4 {
                                match self.data.get(self.index) {
                                    None => return Err(GoJsonError::UnexpectedEof),
                                    Some(byte) if byte.is_ascii_hexdigit() => self.index += 1,
                                    Some(_) => return Err(self.invalid_value()),
                                }
                            }
                        }
                        Some(_) => return Err(self.invalid_value()),
                    }
                }
                Some(byte) if *byte < 0x20 => {
                    return Err(GoJsonError::Syntax {
                        pos: self.index,
                        detail: format!("invalid character {} in string literal", char_text(*byte)),
                        replacement: false,
                    });
                }
                Some(_) => self.index += 1,
            }
        }
    }

    fn skip_space(&mut self) {
        while self
            .data
            .get(self.index)
            .is_some_and(|byte| is_space(*byte))
        {
            self.index += 1;
        }
    }

    fn invalid_value(&self) -> GoJsonError {
        let (token, replacement) = error_token(self.data, self.index);
        GoJsonError::Syntax {
            pos: self.index,
            detail: format!("invalid value {token}"),
            replacement,
        }
    }

    fn unexpected_token(&self) -> GoJsonError {
        let (token, replacement) = match self.token_text() {
            Some(token) => (token, false),
            None => error_token(self.data, self.index),
        };
        GoJsonError::Syntax {
            pos: self.index,
            detail: format!("unexpected token {token}"),
            replacement,
        }
    }

    /// The raw text of the token starting here, as `Token.RawString` reports
    /// it; `None` when `parseNext` would fail instead of returning a token.
    fn token_text(&self) -> Option<String> {
        let rest = &self.data[self.index..];
        let first = *rest.first()?;
        match first {
            b'{' | b'}' | b'[' | b']' | b',' => Some(char_text(first)),
            b'"' => {
                let mut end = 1;
                while let Some(&byte) = rest.get(end) {
                    end += 1;
                    if byte == b'\\' {
                        end += 1;
                    } else if byte == b'"' {
                        break;
                    }
                }
                Some(String::from_utf8_lossy(&rest[..end.min(rest.len())]).into_owned())
            }
            b'n' if literal_is_delimited(rest, b"null") => Some("null".to_owned()),
            b't' if literal_is_delimited(rest, b"true") => Some("true".to_owned()),
            b'f' if literal_is_delimited(rest, b"false") => Some("false".to_owned()),
            b'-' | b'0'..=b'9' => {
                let length = number_length(rest);
                (length > 0).then(|| String::from_utf8_lossy(&rest[..length]).into_owned())
            }
            _ => None,
        }
    }
}

fn literal_is_delimited(rest: &[u8], word: &[u8]) -> bool {
    rest.starts_with(word)
        && rest
            .get(word.len())
            .copied()
            .is_none_or(|byte| is_space(byte) || matches!(byte, b',' | b'}' | b']'))
}

fn char_text(byte: u8) -> String {
    (byte as char).to_string()
}

/// Length of a valid JSON number token at the start of `data` (0 if invalid),
/// matching Go's `parseNumber` grammar.
fn number_length(data: &[u8]) -> usize {
    let mut index = 0;
    if data.get(index) == Some(&b'-') {
        index += 1;
    }
    match data.get(index) {
        Some(b'0') => index += 1,
        Some(b'1'..=b'9') => {
            index += 1;
            while data.get(index).is_some_and(u8::is_ascii_digit) {
                index += 1;
            }
        }
        _ => return 0,
    }
    if data.get(index) == Some(&b'.') {
        index += 1;
        let start = index;
        while data.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return 0;
        }
    }
    if matches!(data.get(index), Some(b'e' | b'E')) {
        index += 1;
        if matches!(data.get(index), Some(b'+' | b'-')) {
            index += 1;
        }
        let start = index;
        while data.get(index).is_some_and(u8::is_ascii_digit) {
            index += 1;
        }
        if index == start {
            return 0;
        }
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(data: &[u8]) -> String {
        syntax_error(data)
            .map(|error| error.text(data))
            .unwrap_or_default()
    }

    #[test]
    fn matches_the_oracle_corpus_syntax_texts() {
        assert_eq!(text(b"{"), "proto: unexpected EOF");
        assert_eq!(
            text(b"\xff\xfe\x00\x01"),
            "proto: syntax error (line 1:1): invalid value \u{fffd}"
        );
        assert_eq!(
            text(&vec![b'A'; 256 * 1024]),
            "proto: syntax error (line 1:1): invalid value AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        );
        assert_eq!(
            text(b"not-json"),
            "proto: syntax error (line 1:1): invalid value not-json"
        );
        assert_eq!(
            text(b"\x00\x00\x00\x00\x02{}"),
            "proto: syntax error (line 1:1): invalid value \u{0}"
        );
        assert_eq!(
            text(b"\x0f\xff\xff\xff"),
            "proto: syntax error (line 1:1): invalid value \u{f}"
        );
    }

    #[test]
    fn accepts_valid_documents_and_reports_nested_positions() {
        assert!(syntax_error(b"{}").is_none());
        assert!(syntax_error(b"{\"a\":[1,2,{\"b\":null}]}").is_none());
        assert_eq!(
            text(b"{\"a\":}"),
            "proto: syntax error (line 1:6): unexpected token }"
        );
        assert_eq!(
            text(b"{\n  \"a\": x}"),
            "proto: syntax error (line 2:8): invalid value x"
        );
    }
}
