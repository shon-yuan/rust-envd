//! Go `protojson` compatible JSON decoding for registered message schemas.
//!
//! A direct port of `google.golang.org/protobuf` v1.36.12
//! `encoding/protojson` and `internal/encoding/json`, restricted to what
//! connect-go 1.18.1 exposes: `DiscardUnknown`, no `Any`, no extensions.
//! serde's error model cannot reproduce Go's texts, byte positions or
//! accept/reject decisions, so registered message types are decoded here into
//! canonical JSON and then handed to buffa's serde decoder.
//!
//! Messages without a registered schema keep the plain serde path.

use base64::Engine;
use base64::alphabet as b64_alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Field kinds, named as Go's `protoreflect.Kind` prints them in errors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldKind {
    /// `bool`
    Bool,
    /// `int32`, `sint32`, `sfixed32`
    Int32,
    /// `int64`, `sint64`, `sfixed64`
    Int64,
    /// `uint32`, `fixed32`
    Uint32,
    /// `uint64`, `fixed64`
    Uint64,
    /// `float`
    Float,
    /// `double`
    Double,
    /// `string`
    String,
    /// `bytes`
    Bytes,
    /// `enum`
    Enum,
    /// `message`
    Message,
}

impl FieldKind {
    fn label(self) -> &'static str {
        match self {
            FieldKind::Bool => "bool",
            FieldKind::Int32 => "int32",
            FieldKind::Int64 => "int64",
            FieldKind::Uint32 => "uint32",
            FieldKind::Uint64 => "uint64",
            FieldKind::Float => "float",
            FieldKind::Double => "double",
            FieldKind::String => "string",
            FieldKind::Bytes => "bytes",
            FieldKind::Enum => "enum",
            FieldKind::Message => "message",
        }
    }
}

/// One field of a message.
#[derive(Clone, Copy, Debug)]
pub struct FieldSchema {
    /// Proto field name (accepted as an alias of `json_name`).
    pub name: &'static str,
    /// JSON name, printed in error texts.
    pub json_name: &'static str,
    /// Value kind. For maps this is the value kind.
    pub kind: FieldKind,
    /// Repeated (non-map) field.
    pub repeated: bool,
    /// `Some(value kind)` when the field is a map.
    pub map_value: Option<FieldKind>,
    /// Map key kind (`String` for envd's maps).
    pub map_key: FieldKind,
    /// Full name of the message type for `Message` fields and map values.
    pub message: Option<&'static str>,
    /// Full name of the enum type for `Enum` fields and map values.
    pub enum_name: Option<&'static str>,
    /// Index into [`MessageSchema::oneofs`] for real (non-synthetic) oneofs.
    pub oneof: Option<usize>,
}

impl FieldSchema {
    /// A simple scalar field.
    #[must_use]
    pub const fn scalar(name: &'static str, json_name: &'static str, kind: FieldKind) -> Self {
        FieldSchema {
            name,
            json_name,
            kind,
            repeated: false,
            map_value: None,
            map_key: FieldKind::String,
            message: None,
            enum_name: None,
            oneof: None,
        }
    }
}

/// One message type.
#[derive(Clone, Copy, Debug)]
pub struct MessageSchema {
    /// Fully-qualified proto name, e.g. `process.ListRequest`.
    pub full_name: &'static str,
    /// Fields in declaration order.
    pub fields: &'static [FieldSchema],
    /// Fully-qualified oneof names, indexed by [`FieldSchema::oneof`].
    pub oneofs: &'static [&'static str],
}

/// One enum type.
#[derive(Clone, Copy, Debug)]
pub struct EnumSchema {
    /// Fully-qualified proto name.
    pub full_name: &'static str,
    /// `(value name, number)` pairs.
    pub values: &'static [(&'static str, i32)],
}

/// Every message and enum the application can decode.
#[derive(Debug)]
pub struct SchemaSet {
    /// Message schemas.
    pub messages: &'static [MessageSchema],
    /// Enum schemas.
    pub enums: &'static [EnumSchema],
}

impl SchemaSet {
    fn message(&self, full_name: &str) -> Option<&'static MessageSchema> {
        self.messages.iter().find(|m| m.full_name == full_name)
    }

    fn enumeration(&self, full_name: &str) -> Option<&'static EnumSchema> {
        self.enums.iter().find(|e| e.full_name == full_name)
    }
}

static REGISTRY: OnceLock<&'static SchemaSet> = OnceLock::new();

/// Install the schema set used by [`decode`]. Idempotent; the first call wins.
pub fn register(set: &'static SchemaSet) {
    let _ = REGISTRY.set(set);
}

/// Decode `data` as `full_name` with the registered schema.
///
/// Returns `None` when no schema set is registered or the message type (or one
/// of its nested types) is not described by it; the caller then falls back to
/// the plain serde decoder. On a schema error, returns the Go detail text
/// without the `proto: ` prefix, which the caller adds once.
#[must_use]
pub fn decode(full_name: &str, data: &[u8]) -> Option<Result<Vec<u8>, Detail>> {
    let set = REGISTRY.get().copied()?;
    let message = set.message(full_name)?;
    decode_with(set, message, data)
}

/// A Go-style protojson error detail, e.g. `(line 1:9): invalid value ...`.
#[derive(Debug, Clone)]
pub struct Detail {
    /// The detail text, without the `proto: ` prefix.
    pub text: String,
    /// True when an echoed raw token contained invalid UTF-8 that was replaced
    /// with U+FFFD; the wire encoder must then escape it as `�`.
    pub escapes_replacement: bool,
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Invalid,
    Eof,
    Null,
    Bool,
    Number,
    String,
    Name,
    ObjectOpen,
    ObjectClose,
    ArrayOpen,
    ArrayClose,
    Comma,
}

/// Null | Bool | Number | String, as Go's `scalar` mask.
const SCALAR: u16 = (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5);

impl Kind {
    fn bit(self) -> u16 {
        1 << (self as u16)
    }
}

#[derive(Clone, Debug)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
    bool_value: bool,
    str_value: String,
}

impl Token {
    fn empty() -> Self {
        Token {
            kind: Kind::Invalid,
            start: 0,
            end: 0,
            bool_value: false,
            str_value: String::new(),
        }
    }
}

struct Decoder<'a> {
    orig: &'a [u8],
    pos: usize,
    last: Token,
    last_was_peek: bool,
    open: Vec<Kind>,
    /// Set once an error text has echoed invalid UTF-8 as U+FFFD.
    lossy: bool,
}

const UNEXPECTED_EOF: &str = "unexpected EOF";

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Decoder {
            orig: data,
            pos: 0,
            last: Token::empty(),
            last_was_peek: false,
            open: Vec::new(),
            lossy: false,
        }
    }

    fn read(&mut self) -> Result<Token, String> {
        // Go's `Read` returns the token cached by a preceding `Peek`.
        if self.last_was_peek {
            self.last_was_peek = false;
            return Ok(self.last.clone());
        }
        let mut tok = self.parse_next()?;
        match tok.kind {
            Kind::Eof => {
                // Go's check is `((kind&scalar)|ObjectClose|ArrayClose) == 0`,
                // which is always false because the close bits are non-zero,
                // so only the open-stack depth matters.
                if !self.open.is_empty() {
                    return Err(UNEXPECTED_EOF.to_string());
                }
            }
            Kind::Null | Kind::Bool | Kind::Number => {
                if !self.is_value_next() {
                    return Err(self.unexpected(&tok));
                }
            }
            Kind::String => {
                if !self.is_value_next() {
                    if self.last.kind.bit() & (Kind::ObjectOpen.bit() | Kind::Comma.bit()) == 0 {
                        return Err(self.unexpected(&tok));
                    }
                    if self.pos == self.orig.len() {
                        return Err(UNEXPECTED_EOF.to_string());
                    }
                    let c = self.orig[self.pos];
                    if c != b':' {
                        let shown = self.lossy_text(&[c]);
                        let msg =
                            format!("unexpected character {shown}, missing \":\" after field name");
                        return Err(self.syntax_error(self.pos, msg));
                    }
                    tok.kind = Kind::Name;
                    self.consume(1);
                }
            }
            Kind::ObjectOpen | Kind::ArrayOpen => {
                if !self.is_value_next() {
                    return Err(self.unexpected(&tok));
                }
                self.open.push(tok.kind);
            }
            Kind::ObjectClose => {
                if self.open.is_empty()
                    || self.last.kind.bit() & (Kind::Name.bit() | Kind::Comma.bit()) != 0
                    || self.open.last() != Some(&Kind::ObjectOpen)
                {
                    return Err(self.unexpected(&tok));
                }
                self.open.pop();
            }
            Kind::ArrayClose => {
                if self.open.is_empty()
                    || self.last.kind == Kind::Comma
                    || self.open.last() != Some(&Kind::ArrayOpen)
                {
                    return Err(self.unexpected(&tok));
                }
                self.open.pop();
            }
            Kind::Comma => {
                if self.open.is_empty()
                    || self.last.kind.bit()
                        & (SCALAR | Kind::ObjectClose.bit() | Kind::ArrayClose.bit())
                        == 0
                {
                    return Err(self.unexpected(&tok));
                }
            }
            Kind::Invalid | Kind::Name => {
                unreachable!("tokenizer never produces Invalid or Name")
            }
        }
        self.last = tok.clone();
        if self.last.kind == Kind::Comma {
            return self.read();
        }
        Ok(tok)
    }

    fn peek(&mut self) -> Result<Token, String> {
        if !self.last_was_peek {
            let tok = self.read()?;
            self.last_was_peek = true;
            return Ok(tok);
        }
        Ok(self.last.clone())
    }

    fn parse_next(&mut self) -> Result<Token, String> {
        self.consume(0);
        if self.pos >= self.orig.len() {
            return Ok(self.consume_token(Kind::Eof, 0));
        }
        let input = &self.orig[self.pos..];
        match input[0] {
            b'n' => {
                if let Some(n) = match_with_delim(b"null", input) {
                    return Ok(self.consume_token(Kind::Null, n));
                }
            }
            b't' => {
                if let Some(n) = match_with_delim(b"true", input) {
                    return Ok(self.consume_bool_token(true, n));
                }
            }
            b'f' => {
                if let Some(n) = match_with_delim(b"false", input) {
                    return Ok(self.consume_bool_token(false, n));
                }
            }
            b'-' | b'0'..=b'9' => {
                if let Some(n) = parse_number(input) {
                    return Ok(self.consume_token(Kind::Number, n));
                }
            }
            b'"' => {
                let (value, size) = self.parse_string(input)?;
                return Ok(self.consume_string_token(value, size));
            }
            b'{' => return Ok(self.consume_token(Kind::ObjectOpen, 1)),
            b'}' => return Ok(self.consume_token(Kind::ObjectClose, 1)),
            b'[' => return Ok(self.consume_token(Kind::ArrayOpen, 1)),
            b']' => return Ok(self.consume_token(Kind::ArrayClose, 1)),
            b',' => return Ok(self.consume_token(Kind::Comma, 1)),
            _ => {}
        }
        let raw = err_regexp(input);
        let shown = self.lossy_text(raw);
        Err(self.syntax_error(self.pos, format!("invalid value {shown}")))
    }

    /// Port of Go's `parseString`. `self.pos` is the opening quote throughout,
    /// which is what `d.currPos()` reports in every error here.
    fn parse_string(&mut self, input: &[u8]) -> Result<(String, usize), String> {
        if input.is_empty() {
            return Err(UNEXPECTED_EOF.to_string());
        }
        if input[0] != b'"' {
            let shown = go_quote_rune(i64::from(input[0]));
            return Err(self.syntax_error(
                self.pos,
                format!("invalid character {shown} at start of string"),
            ));
        }
        let mut rest = &input[1..];
        let mut out: Vec<u8> = Vec::new();
        loop {
            if rest.is_empty() {
                return Err(UNEXPECTED_EOF.to_string());
            }
            let (r, n) = decode_rune(rest);
            if r == REPLACEMENT && n == 1 && std::str::from_utf8(&rest[..1]).is_err() {
                return Err(self.syntax_error(self.pos, "invalid UTF-8 in string".to_string()));
            }
            if (r as u32) < 0x20 {
                let shown = go_quote_rune(i64::from(r as u32));
                return Err(
                    self.syntax_error(self.pos, format!("invalid character {shown} in string"))
                );
            }
            match r {
                '"' => {
                    rest = &rest[1..];
                    let size = input.len() - rest.len();
                    let value = String::from_utf8(out).unwrap_or_default();
                    return Ok((value, size));
                }
                '\\' => {
                    if rest.len() < 2 {
                        return Err(UNEXPECTED_EOF.to_string());
                    }
                    match rest[1] {
                        b'"' | b'\\' | b'/' => {
                            out.push(rest[1]);
                            rest = &rest[2..];
                        }
                        b'b' => {
                            out.push(0x08);
                            rest = &rest[2..];
                        }
                        b'f' => {
                            out.push(0x0c);
                            rest = &rest[2..];
                        }
                        b'n' => {
                            out.push(b'\n');
                            rest = &rest[2..];
                        }
                        b'r' => {
                            out.push(b'\r');
                            rest = &rest[2..];
                        }
                        b't' => {
                            out.push(b'\t');
                            rest = &rest[2..];
                        }
                        b'u' => {
                            if rest.len() < 6 {
                                return Err(UNEXPECTED_EOF.to_string());
                            }
                            let Some(value) = parse_hex4(&rest[2..6]) else {
                                return Err(self.invalid_escape(&rest[..6]));
                            };
                            rest = &rest[6..];
                            let mut code = u32::from(value);
                            if (0xD800..=0xDFFF).contains(&code) {
                                if rest.len() < 6 {
                                    return Err(UNEXPECTED_EOF.to_string());
                                }
                                let second = parse_hex4(&rest[2..6]);
                                let decoded = second.and_then(|low| combine_surrogates(code, low));
                                if rest[0] != b'\\' || rest[1] != b'u' {
                                    return Err(self.invalid_escape(&rest[..6]));
                                }
                                let Some(combined) = decoded else {
                                    return Err(self.invalid_escape(&rest[..6]));
                                };
                                code = combined;
                                rest = &rest[6..];
                            }
                            push_code_point(&mut out, code);
                        }
                        _ => return Err(self.invalid_escape(&rest[..2])),
                    }
                }
                _ => {
                    out.extend_from_slice(&rest[..n]);
                    rest = &rest[n..];
                }
            }
        }
    }

    fn invalid_escape(&mut self, raw: &[u8]) -> String {
        let quoted = go_quote_bytes(raw);
        self.syntax_error(self.pos, format!("invalid escape code {quoted} in string"))
    }

    fn is_value_next(&self) -> bool {
        let Some(top) = self.open.last() else {
            return self.last.kind == Kind::Invalid;
        };
        match top {
            Kind::ObjectOpen => self.last.kind.bit() & Kind::Name.bit() != 0,
            Kind::ArrayOpen => {
                self.last.kind.bit() & (Kind::ArrayOpen.bit() | Kind::Comma.bit()) != 0
            }
            _ => false,
        }
    }

    fn consume(&mut self, n: usize) {
        self.pos = (self.pos + n).min(self.orig.len());
        while self.pos < self.orig.len() {
            match self.orig[self.pos] {
                b' ' | b'\n' | b'\r' | b'\t' => self.pos += 1,
                _ => return,
            }
        }
    }

    fn consume_token(&mut self, kind: Kind, size: usize) -> Token {
        let tok = Token {
            kind,
            start: self.pos,
            end: (self.pos + size).min(self.orig.len()),
            bool_value: false,
            str_value: String::new(),
        };
        self.consume(size);
        tok
    }

    fn consume_bool_token(&mut self, value: bool, size: usize) -> Token {
        let mut tok = self.consume_token(Kind::Bool, size);
        tok.bool_value = value;
        tok
    }

    fn consume_string_token(&mut self, value: String, size: usize) -> Token {
        let mut tok = self.consume_token(Kind::String, size);
        tok.str_value = value;
        tok
    }

    fn raw(&self, tok: &Token) -> &'a [u8] {
        &self.orig[tok.start..tok.end]
    }

    /// Raw token text for an echoed error, replacing invalid UTF-8 as Go's
    /// `errors` formatting plus `encoding/json` marshalling would.
    fn raw_lossy(&mut self, tok: &Token) -> String {
        let bytes = &self.orig[tok.start..tok.end];
        self.lossy_text(bytes)
    }

    fn lossy_text(&mut self, bytes: &[u8]) -> String {
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(_) => {
                self.lossy = true;
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }

    fn unexpected(&mut self, tok: &Token) -> String {
        let raw = self.raw_lossy(tok);
        self.syntax_error(tok.start, format!("unexpected token {raw}"))
    }

    fn syntax_error(&self, pos: usize, msg: String) -> String {
        let (line, column) = position(self.orig, pos);
        format!("syntax error (line {line}:{column}): {msg}")
    }

    fn new_error(&self, pos: usize, msg: String) -> String {
        let (line, column) = position(self.orig, pos);
        format!("(line {line}:{column}): {msg}")
    }
}

const REPLACEMENT: char = '\u{FFFD}';

/// Go's `utf8.DecodeRune`: invalid encodings yield `(RuneError, 1)`.
fn decode_rune(bytes: &[u8]) -> (char, usize) {
    if bytes.is_empty() {
        return (REPLACEMENT, 1);
    }
    let max = bytes.len().min(4);
    for n in 1..=max {
        if let Ok(text) = std::str::from_utf8(&bytes[..n]) {
            if let Some(c) = text.chars().next() {
                if c.len_utf8() == n {
                    return (c, n);
                }
            }
        }
    }
    (REPLACEMENT, 1)
}

fn push_code_point(out: &mut Vec<u8>, code: u32) {
    let c = char::from_u32(code).unwrap_or(REPLACEMENT);
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

fn combine_surrogates(high: u32, low: u16) -> Option<u32> {
    let low = u32::from(low);
    if !(0xDC00..=0xDFFF).contains(&low) {
        return None;
    }
    Some(0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00))
}

fn parse_hex4(bytes: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(bytes).ok()?;
    u16::from_str_radix(text, 16).ok()
}

fn match_with_delim(literal: &[u8], input: &[u8]) -> Option<usize> {
    if !input.starts_with(literal) {
        return None;
    }
    let n = literal.len();
    if n < input.len() && is_not_delim(input[n]) {
        return None;
    }
    Some(n)
}

/// Go's `isNotDelim`: an identifier-ish byte that may continue a token.
fn is_not_delim(c: u8) -> bool {
    c == b'-' || c == b'+' || c == b'.' || c == b'_' || c.is_ascii_alphanumeric()
}

/// Go's `errRegexp = ^([-+._a-zA-Z0-9]{1,32}|.)`.
fn err_regexp(input: &[u8]) -> &[u8] {
    let mut n = 0;
    while n < input.len() && n < 32 && is_not_delim(input[n]) {
        n += 1;
    }
    if n > 0 {
        return &input[..n];
    }
    if input.is_empty() {
        return input;
    }
    let (_, rune_len) = decode_rune(input);
    &input[..rune_len.min(input.len())]
}

fn position(orig: &[u8], idx: usize) -> (usize, usize) {
    let bytes = &orig[..idx.min(orig.len())];
    let line = bytes.iter().filter(|&&c| c == b'\n').count() + 1;
    let start = bytes
        .iter()
        .rposition(|&c| c == b'\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    (line, go_rune_count(&bytes[start..]) + 1)
}

fn go_rune_count(mut bytes: &[u8]) -> usize {
    let mut count = 0;
    while !bytes.is_empty() {
        let (_, n) = decode_rune(bytes);
        let n = n.max(1).min(bytes.len());
        bytes = &bytes[n..];
        count += 1;
    }
    count
}

// ---------------------------------------------------------------------------
// Numbers
// ---------------------------------------------------------------------------

struct NumberParts<'a> {
    neg: bool,
    intp: &'a [u8],
    frac: &'a [u8],
    exp: &'a [u8],
}

/// Port of Go's `parseNumber`.
fn parse_number(input: &[u8]) -> Option<usize> {
    let mut n = 0usize;
    let mut s = input;
    if s.is_empty() {
        return None;
    }
    if s[0] == b'-' {
        s = &s[1..];
        n += 1;
        if s.is_empty() {
            return None;
        }
    }
    match s[0] {
        b'0' => {
            s = &s[1..];
            n += 1;
        }
        b'1'..=b'9' => {
            s = &s[1..];
            n += 1;
            while !s.is_empty() && s[0].is_ascii_digit() {
                s = &s[1..];
                n += 1;
            }
        }
        _ => return None,
    }
    if s.len() >= 2 && s[0] == b'.' && s[1].is_ascii_digit() {
        s = &s[2..];
        n += 2;
        while !s.is_empty() && s[0].is_ascii_digit() {
            s = &s[1..];
            n += 1;
        }
    }
    if s.len() >= 2 && (s[0] == b'e' || s[0] == b'E') {
        s = &s[1..];
        n += 1;
        if s[0] == b'+' || s[0] == b'-' {
            s = &s[1..];
            n += 1;
            if s.is_empty() {
                return None;
            }
        }
        while !s.is_empty() && s[0].is_ascii_digit() {
            s = &s[1..];
            n += 1;
        }
    }
    if n < input.len() && is_not_delim(input[n]) {
        return None;
    }
    Some(n)
}

/// Port of Go's `parseNumberParts`.
fn parse_number_parts(input: &[u8]) -> Option<NumberParts<'_>> {
    let mut i = 0usize;
    if input.is_empty() {
        return None;
    }
    let mut neg = false;
    if input[i] == b'-' {
        neg = true;
        i += 1;
        if i == input.len() {
            return None;
        }
    }
    let mut intp: &[u8] = b"";
    match input[i] {
        b'0' => i += 1,
        b'1'..=b'9' => {
            let start = i;
            i += 1;
            while i < input.len() && input[i].is_ascii_digit() {
                i += 1;
            }
            intp = &input[start..i];
        }
        _ => return None,
    }
    let mut frac: &[u8] = b"";
    if input.len() >= i + 2 && input[i] == b'.' && input[i + 1].is_ascii_digit() {
        let start = i + 1;
        i += 2;
        while i < input.len() && input[i].is_ascii_digit() {
            i += 1;
        }
        frac = &input[start..i];
    }
    let mut exp: &[u8] = b"";
    if input.len() >= i + 2 && (input[i] == b'e' || input[i] == b'E') {
        i += 1;
        let start = i;
        let mut len = 0usize;
        if input[i] == b'+' || input[i] == b'-' {
            i += 1;
            len += 1;
            if i == input.len() {
                return None;
            }
        }
        while i < input.len() && input[i].is_ascii_digit() {
            i += 1;
            len += 1;
        }
        exp = &input[start..start + len];
    }
    let mut frac_end = frac.len();
    while frac_end > 0 && frac[frac_end - 1] == b'0' {
        frac_end -= 1;
    }
    Some(NumberParts {
        neg,
        intp,
        frac: &frac[..frac_end],
        exp,
    })
}

/// Port of Go's `normalizeToIntString`.
fn normalize_to_int_string(parts: &NumberParts<'_>) -> Option<String> {
    let intp_size = parts.intp.len() as i64;
    let frac_size = parts.frac.len() as i64;
    if intp_size == 0 && frac_size == 0 {
        return Some("0".to_string());
    }
    let mut exp: i64 = 0;
    if !parts.exp.is_empty() {
        let text = std::str::from_utf8(parts.exp).ok()?;
        exp = i64::from(text.parse::<i32>().ok()?);
    }
    let mut num: Vec<u8> = Vec::new();
    if exp >= 0 {
        if frac_size > exp {
            return None;
        }
        const MAX_DIGITS: i64 = 20;
        if intp_size + exp > MAX_DIGITS {
            return None;
        }
        num.extend_from_slice(parts.intp);
        num.extend_from_slice(parts.frac);
        for _ in 0..(exp - frac_size) {
            num.push(b'0');
        }
    } else {
        if frac_size > 0 {
            return None;
        }
        let index = intp_size + exp;
        if index < 0 {
            return None;
        }
        let index = usize::try_from(index).ok()?;
        if parts.intp[index..].iter().any(|&c| c != b'0') {
            return None;
        }
        num.extend_from_slice(&parts.intp[..index]);
    }
    let mut text = String::from_utf8(num).ok()?;
    if parts.neg {
        text.insert(0, '-');
    }
    Some(text)
}

fn token_int(tok: &Token, raw: &[u8], bits: u32) -> Option<i64> {
    if tok.kind != Kind::Number {
        return None;
    }
    let parts = parse_number_parts(raw)?;
    let text = normalize_to_int_string(&parts)?;
    match bits {
        32 => text.parse::<i32>().ok().map(i64::from),
        _ => text.parse::<i64>().ok(),
    }
}

fn token_uint(tok: &Token, raw: &[u8], bits: u32) -> Option<u64> {
    if tok.kind != Kind::Number {
        return None;
    }
    let parts = parse_number_parts(raw)?;
    let text = normalize_to_int_string(&parts)?;
    match bits {
        32 => text.parse::<u32>().ok().map(u64::from),
        _ => text.parse::<u64>().ok(),
    }
}

/// Port of Go's quoted-number path: trim, then decode the string as exactly one
/// number token followed by EOF.
fn quoted_number(text: &str) -> Option<(Token, Vec<u8>)> {
    let trimmed = text.trim_matches(go_space);
    if trimmed.len() != text.len() {
        return None;
    }
    let bytes = trimmed.as_bytes();
    let mut decoder = Decoder::new(bytes);
    let tok = decoder.read().ok()?;
    let next = decoder.read().ok()?;
    if next.kind != Kind::Eof {
        return None;
    }
    let raw = bytes[tok.start..tok.end].to_vec();
    Some((tok, raw))
}

fn go_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{000b}'
            | '\u{000c}'
            | '\r'
            | ' '
            | '\u{0085}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'
            ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

fn token_int_flexible(tok: &Token, raw: &[u8], bits: u32) -> Option<i64> {
    match tok.kind {
        Kind::Number => token_int(tok, raw, bits),
        Kind::String => {
            let (number, raw) = quoted_number(&tok.str_value)?;
            token_int(&number, &raw, bits)
        }
        _ => None,
    }
}

fn token_uint_flexible(tok: &Token, raw: &[u8], bits: u32) -> Option<u64> {
    match tok.kind {
        Kind::Number => token_uint(tok, raw, bits),
        Kind::String => {
            let (number, raw) = quoted_number(&tok.str_value)?;
            token_uint(&number, &raw, bits)
        }
        _ => None,
    }
}

fn token_float(tok: &Token, raw: &[u8], single: bool) -> Option<f64> {
    match tok.kind {
        Kind::Number => number_float(raw, single),
        Kind::String => match tok.str_value.as_str() {
            "NaN" => Some(f64::NAN),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => {
                let (_, raw) = quoted_number(&tok.str_value)?;
                number_float(&raw, single)
            }
        },
        _ => None,
    }
}

fn number_float(raw: &[u8], single: bool) -> Option<f64> {
    let text = std::str::from_utf8(raw).ok()?;
    if single {
        text.parse::<f32>().ok().map(f64::from)
    } else {
        text.parse::<f64>().ok()
    }
}

fn format_float(value: f64) -> String {
    if value.is_nan() {
        return "\"NaN\"".to_string();
    }
    if value.is_infinite() {
        return if value > 0.0 {
            "\"Infinity\"".to_string()
        } else {
            "\"-Infinity\"".to_string()
        };
    }
    let mut text = format!("{value}");
    if !text.contains('.') && !text.contains('e') && !text.contains('E') {
        text.push_str(".0");
    }
    text
}

// ---------------------------------------------------------------------------
// Go quoting
// ---------------------------------------------------------------------------

/// `strconv.QuoteRune` for the characters that can reach these messages.
fn go_quote_rune(code: i64) -> String {
    let c = char::from_u32(u32::try_from(code).unwrap_or(0xFFFD)).unwrap_or(REPLACEMENT);
    let mut out = String::from("'");
    match c {
        '\'' => out.push_str("\\'"),
        '\\' => out.push_str("\\\\"),
        '\u{07}' => out.push_str("\\a"),
        '\u{08}' => out.push_str("\\b"),
        '\u{0c}' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{0b}' => out.push_str("\\v"),
        c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
            out.push_str(&format!("\\x{:02x}", c as u32));
        }
        c if (c as u32) < 0x7f => out.push(c),
        c if is_go_printable(c) => out.push(c),
        c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
        c => out.push_str(&format!("\\U{:08x}", c as u32)),
    }
    out.push('\'');
    out
}

/// `strconv.Quote` for the byte slices echoed as escape codes.
fn go_quote_bytes(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut rest = bytes;
    while !rest.is_empty() {
        let (c, n) = decode_rune(rest);
        if c == REPLACEMENT && n == 1 && std::str::from_utf8(&rest[..1]).is_err() {
            out.push_str(&format!("\\x{:02x}", rest[0]));
            rest = &rest[1..];
            continue;
        }
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{07}' => out.push_str("\\a"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0b}' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if is_go_printable(c) => out.push(c),
            c if (c as u32) <= 0xffff => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push_str(&format!("\\U{:08x}", c as u32)),
        }
        rest = &rest[n..];
    }
    out.push('"');
    out
}

/// Go's `strconv.IsPrint`, approximated for the ranges that can appear here.
fn is_go_printable(c: char) -> bool {
    let code = c as u32;
    (0x20..0x7f).contains(&code) || (0xa0..=0xff).contains(&code) || code > 0xff
}

// ---------------------------------------------------------------------------
// Base64 (Go semantics)
// ---------------------------------------------------------------------------

fn base64_engine(url_safe: bool, padded: bool) -> GeneralPurpose {
    let alphabet = if url_safe {
        &b64_alphabet::URL_SAFE
    } else {
        &b64_alphabet::STANDARD
    };
    let padding = if padded {
        DecodePaddingMode::RequireCanonical
    } else {
        DecodePaddingMode::RequireNone
    };
    GeneralPurpose::new(
        alphabet,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(padding),
    )
}

/// Port of protojson's `unmarshalBytes`. Go ignores CR/LF when decoding but
/// chooses the padding mode from the original length.
fn decode_go_base64(text: &str) -> Option<Vec<u8>> {
    let url_safe = text.contains('-') || text.contains('_');
    let padded = text.len() % 4 == 0;
    let cleaned: String = text.chars().filter(|c| *c != '\r' && *c != '\n').collect();
    base64_engine(url_safe, padded)
        .decode(cleaned.as_bytes())
        .ok()
}

fn encode_go_base64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ---------------------------------------------------------------------------
// Well-known type scalars
// ---------------------------------------------------------------------------

/// Smallest valid `Timestamp.seconds` (`0001-01-01T00:00:00Z`).
const MIN_TIMESTAMP_SECONDS: i64 = -62_135_596_800;
/// Largest valid `Timestamp.seconds` (`9999-12-31T23:59:59Z`).
const MAX_TIMESTAMP_SECONDS: i64 = 253_402_300_799;
/// Largest valid `Duration.seconds` magnitude.
const MAX_DURATION_SECONDS: i64 = 315_576_000_000;

/// Parse exactly what Go's `time.Parse(time.RFC3339Nano, s)` accepts.
///
/// The layout is fixed (`2006-01-02T15:04:05.999999999Z07:00`) and every byte
/// must be consumed. Go accepts `.` or `,` before the fraction and truncates
/// it to nine digits; zone offsets allow a 24-hour hour and 60 minutes.
fn parse_go_timestamp(s: &str) -> Option<(i64, i32)> {
    fn digits(b: &[u8], start: usize, len: usize) -> Option<i64> {
        let slice = b.get(start..start + len)?;
        let mut value: i64 = 0;
        for &c in slice {
            if !c.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(c - b'0');
        }
        Some(value)
    }

    if !s.is_ascii() {
        return None;
    }
    let b = s.as_bytes();
    let year = digits(b, 0, 4)?;
    if b.get(4) != Some(&b'-') {
        return None;
    }
    let month = digits(b, 5, 2)?;
    if b.get(7) != Some(&b'-') {
        return None;
    }
    let day = digits(b, 8, 2)?;
    if b.get(10) != Some(&b'T') {
        return None;
    }
    let hour = digits(b, 11, 2)?;
    if b.get(13) != Some(&b':') {
        return None;
    }
    let minute = digits(b, 14, 2)?;
    if b.get(16) != Some(&b':') {
        return None;
    }
    let second = digits(b, 17, 2)?;

    let mut i = 19;
    let mut nanos: i32 = 0;
    if b.len() >= i + 2 && (b[i] == b'.' || b[i] == b',') && b[i + 1].is_ascii_digit() {
        i += 1;
        let mut n = 0;
        while i < b.len() && b[i].is_ascii_digit() {
            if n < 9 {
                nanos = nanos * 10 + i32::from(b[i] - b'0');
            }
            n += 1;
            i += 1;
        }
        for _ in n.min(9)..9 {
            nanos *= 10;
        }
    }

    let offset: i64 = if b.get(i) == Some(&b'Z') {
        i += 1;
        0
    } else {
        let sign = match b.get(i) {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => return None,
        };
        let oh = digits(b, i + 1, 2)?;
        if b.get(i + 3) != Some(&b':') {
            return None;
        }
        let om = digits(b, i + 4, 2)?;
        if oh > 24 || om > 60 {
            return None;
        }
        i += 6;
        sign * (oh * 3600 + om * 60)
    };
    if i != b.len() {
        return None;
    }

    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let secs = days_from_civil(year, month as u32, day as u32) * 86_400
        + hour * 3600
        + minute * 60
        + second
        - offset;
    Some((secs, nanos))
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = year - era * 400;
    let mp = (i64::from(month) + 9) % 12;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// Minimal fractional seconds (0, 3, 6 or 9 digits), as marshalTimestamp emits.
fn format_fraction(nanos: i32) -> String {
    if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1_000 == 0 {
        format!(".{:06}", nanos / 1_000)
    } else {
        format!(".{nanos:09}")
    }
}

fn format_go_timestamp(secs: i64, nanos: i32) -> String {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}{}Z",
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        format_fraction(nanos)
    )
}

/// Port of Go's `parseDuration` (well_known_types.go).
fn parse_go_duration(input: &str) -> Option<(i64, i32)> {
    let mut b = input.as_bytes();
    if b.len() < 2 || b[b.len() - 1] != b's' {
        return None;
    }
    b = &b[..b.len() - 1];

    let mut negative = false;
    match b.first()? {
        b'-' => {
            negative = true;
            b = &b[1..];
        }
        b'+' => b = &b[1..],
        _ => {}
    }
    if b.is_empty() {
        return None;
    }

    let mut int_part: &[u8] = &[];
    match b[0] {
        b'0' => b = &b[1..],
        b'1'..=b'9' => {
            let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            int_part = &b[..n];
            b = &b[n..];
        }
        b'.' => {}
        _ => return None,
    }

    let mut nanos: i64 = 0;
    if !b.is_empty() {
        if b[0] != b'.' {
            return None;
        }
        b = &b[1..];
        if b.len() > 9 || !b.iter().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let mut value: i64 = 0;
        for &c in b {
            value = value * 10 + i64::from(c - b'0');
        }
        nanos = value * 10_i64.pow(9 - b.len() as u32);
    }

    let secs: i64 = if int_part.is_empty() {
        0
    } else {
        std::str::from_utf8(int_part).ok()?.parse().ok()?
    };
    let (secs, nanos) = if negative {
        (-secs, -nanos)
    } else {
        (secs, nanos)
    };
    Some((secs, i32::try_from(nanos).ok()?))
}

fn format_go_duration(secs: i64, nanos: i32) -> String {
    // Go formats the absolute values with 0, 3, 6 or 9 fractional digits.
    let negative = secs < 0 || nanos < 0;
    let sign = if negative { "-" } else { "" };
    format!(
        "{sign}{}{}s",
        secs.unsigned_abs(),
        format_fraction(nanos.unsigned_abs() as i32)
    )
}

/// Go's `strs.JSONSnakeCase`.
fn json_snake_case(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len() + 4);
    for mut c in s.bytes() {
        if c.is_ascii_uppercase() {
            out.push(b'_');
            c += b'a' - b'A';
        }
        out.push(c);
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Go's `protoreflect.FullName.IsValid`.
fn is_valid_full_name(s: &str) -> bool {
    fn is_ident_letter(c: u8) -> bool {
        c == b'_' || c.is_ascii_alphabetic()
    }
    fn consume_ident(b: &[u8]) -> Option<usize> {
        if b.is_empty() || !is_ident_letter(b[0]) {
            return None;
        }
        let mut i = 1;
        while i < b.len() && (is_ident_letter(b[i]) || b[i].is_ascii_digit()) {
            i += 1;
        }
        Some(i)
    }

    let b = s.as_bytes();
    let Some(mut i) = consume_ident(b) else {
        return false;
    };
    while i < b.len() {
        if b[i] != b'.' {
            return false;
        }
        i += 1;
        let Some(n) = consume_ident(&b[i..]) else {
            return false;
        };
        i += n;
    }
    true
}

// ---------------------------------------------------------------------------
// protojson walk
// ---------------------------------------------------------------------------

fn decode_with(
    set: &'static SchemaSet,
    top: &'static MessageSchema,
    data: &[u8],
) -> Option<Result<Vec<u8>, Detail>> {
    let mut walk = Walk {
        set,
        dec: Decoder::new(data),
        remaining: 10_000,
    };
    let mut out = String::new();
    let error = match walk.unmarshal_message(Some(top), &mut out) {
        Ok(()) => match walk.dec.read() {
            Ok(tok) if tok.kind == Kind::Eof => return Some(Ok(out.into_bytes())),
            Ok(tok) => {
                let raw = walk.dec.raw_lossy(&tok);
                WalkError::Syntax {
                    pos: tok.start,
                    msg: format!("unexpected token {raw}"),
                }
            }
            Err(text) => WalkError::Detail(text),
        },
        Err(err) => err,
    };
    let text = match error {
        WalkError::Detail(text) => text,
        WalkError::WithPos { pos, msg } => walk.dec.new_error(pos, msg),
        WalkError::Syntax { pos, msg } => walk.dec.syntax_error(pos, msg),
        WalkError::Unsupported => return None,
    };
    Some(Err(Detail {
        text,
        escapes_replacement: walk.dec.lossy,
    }))
}

enum WalkError {
    /// A detail already carrying its Go head (`syntax error (line…)`, …).
    Detail(String),
    /// A schema error: `(line L:C): message`.
    WithPos { pos: usize, msg: String },
    /// A syntax error: `syntax error (line L:C): message`.
    Syntax { pos: usize, msg: String },
    /// A nested type is not described by the schema set; use the serde path.
    Unsupported,
}

impl WalkError {
    fn detail(text: impl Into<String>) -> Self {
        WalkError::Detail(text.into())
    }
}

/// Borrowed description of a value's type.
#[derive(Clone, Copy)]
struct ValueType<'s> {
    kind: FieldKind,
    json_name: &'static str,
    message: Option<&'static str>,
    enum_name: Option<&'static str>,
    _field: std::marker::PhantomData<&'s FieldSchema>,
}

impl<'s> ValueType<'s> {
    /// A field value or list element: errors name the field's JSON name.
    fn of(field: &'s FieldSchema, kind: FieldKind) -> Self {
        ValueType {
            kind,
            json_name: field.json_name,
            message: field.message,
            enum_name: field.enum_name,
            _field: std::marker::PhantomData,
        }
    }

    /// A map value: Go reports the synthetic value field's JSON name.
    fn map_value(field: &'s FieldSchema, kind: FieldKind) -> Self {
        ValueType {
            kind,
            json_name: "value",
            message: field.message,
            enum_name: field.enum_name,
            _field: std::marker::PhantomData,
        }
    }
}

enum Emitted {
    Yes,
    Discarded,
}

/// Messages with a custom protojson representation (Go's well-known types).
///
/// `Any` is routed to the serde fallback: its schema-driven re-encoding needs
/// the embedded type's bytes, which this JSON-only walker does not produce.
#[derive(Clone, Copy)]
enum WellKnown {
    Timestamp,
    Duration,
    Wrapper,
    Struct,
    ListValue,
    Value,
    FieldMask,
    Empty,
    Any,
}

impl WellKnown {
    fn of(full_name: &str) -> Option<Self> {
        let name = full_name.strip_prefix("google.protobuf.")?;
        Some(match name {
            "Timestamp" => Self::Timestamp,
            "Duration" => Self::Duration,
            "BoolValue" | "Int32Value" | "Int64Value" | "UInt32Value" | "UInt64Value"
            | "FloatValue" | "DoubleValue" | "StringValue" | "BytesValue" => Self::Wrapper,
            "Struct" => Self::Struct,
            "ListValue" => Self::ListValue,
            "Value" => Self::Value,
            "FieldMask" => Self::FieldMask,
            "Empty" => Self::Empty,
            "Any" => Self::Any,
            _ => return None,
        })
    }
}

struct Walk<'s, 'a> {
    set: &'s SchemaSet,
    dec: Decoder<'a>,
    remaining: i32,
}

impl<'s, 'a> Walk<'s, 'a> {
    fn unmarshal_message(
        &mut self,
        schema: Option<&'static MessageSchema>,
        out: &mut String,
    ) -> Result<(), WalkError> {
        self.remaining -= 1;
        if self.remaining < 0 {
            return Err(WalkError::detail("exceeded max recursion depth"));
        }
        let schema = schema.ok_or(WalkError::Unsupported)?;
        // Go dispatches well-known types before reading the token.
        if let Some(wkt) = WellKnown::of(schema.full_name) {
            return self.unmarshal_well_known(wkt, schema, out);
        }
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::ObjectOpen {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        out.push('{');
        let mut seen_fields: Vec<usize> = Vec::new();
        let mut seen_oneofs: Vec<usize> = Vec::new();
        let mut first = true;
        loop {
            let tok = self.dec.read().map_err(WalkError::Detail)?;
            match tok.kind {
                Kind::ObjectClose => {
                    out.push('}');
                    return Ok(());
                }
                Kind::Name => {}
                _ => {
                    let raw = self.dec.raw_lossy(&tok);
                    return Err(WalkError::Syntax {
                        pos: tok.start,
                        msg: format!("unexpected token {raw}"),
                    });
                }
            }
            let field_index = self.resolve_field(schema, &tok)?;
            let Some(index) = field_index else {
                self.skip_json_value()?;
                continue;
            };
            let field = &schema.fields[index];
            if seen_fields.contains(&index) {
                let raw = self.dec.raw_lossy(&tok);
                return Err(WalkError::WithPos {
                    pos: tok.start,
                    msg: format!("duplicate field {raw}"),
                });
            }
            seen_fields.push(index);
            if self.dec.peek().map_err(WalkError::Detail)?.kind == Kind::Null {
                self.dec.read().map_err(WalkError::Detail)?;
                continue;
            }
            if field.map_value.is_some() {
                let mut value = String::new();
                self.unmarshal_map(field, &mut value)?;
                Self::commit(out, &mut first, field.json_name, &value);
                continue;
            }
            if field.repeated {
                let mut value = String::new();
                self.unmarshal_list(field, &mut value)?;
                Self::commit(out, &mut first, field.json_name, &value);
                continue;
            }
            if let Some(oneof) = field.oneof {
                if seen_oneofs.contains(&oneof) {
                    let raw = self.dec.raw_lossy(&tok);
                    return Err(WalkError::WithPos {
                        pos: tok.start,
                        msg: format!(
                            "error parsing {raw}, oneof {} is already set",
                            schema.oneofs[oneof]
                        ),
                    });
                }
                seen_oneofs.push(oneof);
            }
            let mut value = String::new();
            let emitted = self.unmarshal_singular(ValueType::of(field, field.kind), &mut value)?;
            if matches!(emitted, Emitted::Yes) {
                Self::commit(out, &mut first, field.json_name, &value);
            }
        }
    }

    fn resolve_field(
        &mut self,
        schema: &'static MessageSchema,
        tok: &Token,
    ) -> Result<Option<usize>, WalkError> {
        let name = tok.str_value.as_str();
        if name.len() >= 2 && name.starts_with('[') && name.ends_with(']') {
            let inner = &name[1..name.len() - 1];
            let found = if self.set.message(inner).is_some() {
                Some("message")
            } else if self.set.enumeration(inner).is_some() {
                Some("enum")
            } else {
                None
            };
            if let Some(kind) = found {
                let raw = self.dec.raw_lossy(tok);
                return Err(WalkError::WithPos {
                    pos: tok.start,
                    msg: format!(
                        "unable to resolve {raw}: found wrong type: got {kind}, want extension"
                    ),
                });
            }
            return Ok(None);
        }
        Ok(schema
            .fields
            .iter()
            .position(|f| f.json_name == name)
            .or_else(|| schema.fields.iter().position(|f| f.name == name)))
    }

    fn commit(out: &mut String, first: &mut bool, json_name: &str, value: &str) {
        if !*first {
            out.push(',');
        }
        *first = false;
        out.push_str(&serde_json::to_string(json_name).unwrap_or_default());
        out.push(':');
        out.push_str(value);
    }

    fn unmarshal_singular(
        &mut self,
        ty: ValueType<'s>,
        out: &mut String,
    ) -> Result<Emitted, WalkError> {
        if ty.kind == FieldKind::Message {
            let schema = ty
                .message
                .and_then(|name| self.set.message(name))
                .ok_or(WalkError::Unsupported)?;
            self.unmarshal_message(Some(schema), out)?;
            return Ok(Emitted::Yes);
        }
        self.unmarshal_scalar(ty, out)
    }

    fn unmarshal_scalar(
        &mut self,
        ty: ValueType<'s>,
        out: &mut String,
    ) -> Result<Emitted, WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        let raw = self.dec.raw(&tok).to_vec();
        match ty.kind {
            FieldKind::Bool => {
                if tok.kind == Kind::Bool {
                    out.push_str(if tok.bool_value { "true" } else { "false" });
                    return Ok(Emitted::Yes);
                }
            }
            FieldKind::Int32 | FieldKind::Int64 => {
                let bits = if ty.kind == FieldKind::Int32 { 32 } else { 64 };
                if let Some(value) = token_int_flexible(&tok, &raw, bits) {
                    out.push_str(&value.to_string());
                    return Ok(Emitted::Yes);
                }
            }
            FieldKind::Uint32 | FieldKind::Uint64 => {
                let bits = if ty.kind == FieldKind::Uint32 { 32 } else { 64 };
                if let Some(value) = token_uint_flexible(&tok, &raw, bits) {
                    out.push_str(&value.to_string());
                    return Ok(Emitted::Yes);
                }
            }
            FieldKind::Float | FieldKind::Double => {
                if let Some(value) = token_float(&tok, &raw, ty.kind == FieldKind::Float) {
                    out.push_str(&format_float(value));
                    return Ok(Emitted::Yes);
                }
            }
            FieldKind::String => {
                if tok.kind == Kind::String {
                    out.push_str(&serde_json::to_string(&tok.str_value).unwrap_or_default());
                    return Ok(Emitted::Yes);
                }
            }
            FieldKind::Bytes => {
                if tok.kind == Kind::String {
                    if let Some(bytes) = decode_go_base64(&tok.str_value) {
                        let encoded =
                            serde_json::to_string(&encode_go_base64(&bytes)).unwrap_or_default();
                        out.push_str(&encoded);
                        return Ok(Emitted::Yes);
                    }
                }
            }
            FieldKind::Enum => {
                let number = match tok.kind {
                    Kind::String => self.enum_by_name(ty.enum_name, &tok.str_value),
                    Kind::Number => token_int(&tok, &raw, 32).map(|n| n as i32),
                    _ => None,
                };
                if let Some(number) = number {
                    out.push_str(&number.to_string());
                    return Ok(Emitted::Yes);
                }
                // An unknown enum name is discarded under DiscardUnknown; any
                // other rejected token is an error.
                if tok.kind == Kind::String {
                    return Ok(Emitted::Discarded);
                }
            }
            FieldKind::Message => return Err(WalkError::Unsupported),
        }
        Err(self.invalid_value(ty, &tok))
    }

    fn enum_by_name(&self, enum_name: Option<&'static str>, name: &str) -> Option<i32> {
        let schema = self.set.enumeration(enum_name?)?;
        schema
            .values
            .iter()
            .find(|(value_name, _)| *value_name == name)
            .map(|(_, number)| *number)
    }

    fn invalid_value(&mut self, ty: ValueType<'s>, tok: &Token) -> WalkError {
        let raw = self.dec.raw_lossy(tok);
        WalkError::WithPos {
            pos: tok.start,
            msg: format!(
                "invalid value for {} field {}: {}",
                ty.kind.label(),
                ty.json_name,
                raw
            ),
        }
    }

    fn unmarshal_list(
        &mut self,
        field: &'static FieldSchema,
        out: &mut String,
    ) -> Result<(), WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::ArrayOpen {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        out.push('[');
        let mut first = true;
        loop {
            if self.dec.peek().map_err(WalkError::Detail)?.kind == Kind::ArrayClose {
                self.dec.read().map_err(WalkError::Detail)?;
                out.push(']');
                return Ok(());
            }
            let mut value = String::new();
            let emitted = self.unmarshal_singular(ValueType::of(field, field.kind), &mut value)?;
            if matches!(emitted, Emitted::Yes) {
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&value);
            }
        }
    }

    fn unmarshal_map(
        &mut self,
        field: &'static FieldSchema,
        out: &mut String,
    ) -> Result<(), WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::ObjectOpen {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        out.push('{');
        let mut keys: Vec<String> = Vec::new();
        let mut first = true;
        loop {
            let key_tok = self.dec.read().map_err(WalkError::Detail)?;
            match key_tok.kind {
                Kind::ObjectClose => {
                    out.push('}');
                    return Ok(());
                }
                Kind::Name => {}
                _ => {
                    let raw = self.dec.raw_lossy(&key_tok);
                    return Err(WalkError::Syntax {
                        pos: key_tok.start,
                        msg: format!("unexpected token {raw}"),
                    });
                }
            }
            let key = self.map_key(field.map_key, &key_tok)?;
            if keys.contains(&key) {
                let raw = self.dec.raw_lossy(&key_tok);
                return Err(WalkError::WithPos {
                    pos: key_tok.start,
                    msg: format!("duplicate map key {raw}"),
                });
            }
            let value_kind = field.map_value.unwrap_or(FieldKind::String);
            let mut value = String::new();
            let emitted =
                self.unmarshal_singular(ValueType::map_value(field, value_kind), &mut value)?;
            if matches!(emitted, Emitted::Discarded) {
                continue;
            }
            keys.push(key.clone());
            if !first {
                out.push(',');
            }
            first = false;
            out.push_str(&serde_json::to_string(&key).unwrap_or_default());
            out.push(':');
            out.push_str(&value);
        }
    }

    fn map_key(&mut self, kind: FieldKind, tok: &Token) -> Result<String, WalkError> {
        let name = tok.str_value.clone();
        let key = match kind {
            FieldKind::String => Some(name.clone()),
            FieldKind::Bool => match name.as_str() {
                "true" | "false" => Some(name.clone()),
                _ => None,
            },
            FieldKind::Int32 | FieldKind::Int64 => name.parse::<i64>().ok().and_then(|value| {
                if kind == FieldKind::Int32
                    && (value < i64::from(i32::MIN) || value > i64::from(i32::MAX))
                {
                    None
                } else {
                    Some(value.to_string())
                }
            }),
            FieldKind::Uint32 | FieldKind::Uint64 => name.parse::<u64>().ok().and_then(|value| {
                if kind == FieldKind::Uint32 && value > u64::from(u32::MAX) {
                    None
                } else {
                    Some(value.to_string())
                }
            }),
            _ => None,
        };
        key.ok_or_else(|| {
            let raw = self.dec.raw_lossy(tok);
            WalkError::WithPos {
                pos: tok.start,
                msg: format!("invalid value for {} key: {}", kind.label(), raw),
            }
        })
    }

    fn unmarshal_well_known(
        &mut self,
        wkt: WellKnown,
        schema: &'static MessageSchema,
        out: &mut String,
    ) -> Result<(), WalkError> {
        match wkt {
            WellKnown::Timestamp => self.unmarshal_timestamp(out),
            WellKnown::Duration => self.unmarshal_duration(out),
            WellKnown::Wrapper => {
                let field = schema
                    .fields
                    .iter()
                    .find(|field| field.name == "value")
                    .ok_or(WalkError::Unsupported)?;
                let emitted = self.unmarshal_scalar(ValueType::of(field, field.kind), out)?;
                debug_assert!(matches!(emitted, Emitted::Yes));
                Ok(())
            }
            WellKnown::Struct => {
                let field = schema
                    .fields
                    .iter()
                    .find(|field| field.name == "fields")
                    .ok_or(WalkError::Unsupported)?;
                self.unmarshal_map(field, out)
            }
            WellKnown::ListValue => {
                let field = schema
                    .fields
                    .iter()
                    .find(|field| field.name == "values")
                    .ok_or(WalkError::Unsupported)?;
                self.unmarshal_list(field, out)
            }
            WellKnown::Value => self.unmarshal_value(out),
            WellKnown::FieldMask => self.unmarshal_field_mask(out),
            WellKnown::Empty => self.unmarshal_empty(out),
            // Needs the embedded message's binary form; let serde try it.
            WellKnown::Any => Err(WalkError::Unsupported),
        }
    }

    fn unmarshal_timestamp(&mut self, out: &mut String) -> Result<(), WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::String {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        let raw = self.dec.raw_lossy(&tok);
        let Some((secs, nanos)) = parse_go_timestamp(&tok.str_value) else {
            return Err(WalkError::WithPos {
                pos: tok.start,
                msg: format!("invalid google.protobuf.Timestamp value {raw}"),
            });
        };
        if !(MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&secs) {
            return Err(WalkError::WithPos {
                pos: tok.start,
                msg: format!("google.protobuf.Timestamp value out of range: {raw}"),
            });
        }
        // Go rejects a period-prefixed fraction longer than 9 digits after
        // time.Parse (which itself truncates); commas bypass this check.
        let text = tok.str_value.as_str();
        if let (Some(dot), Some(tz)) = (
            text.rfind('.'),
            text.rfind(|c| c == 'Z' || c == '-' || c == '+'),
        ) {
            if tz >= dot && tz - dot > 10 {
                return Err(WalkError::WithPos {
                    pos: tok.start,
                    msg: format!("invalid google.protobuf.Timestamp value {raw}"),
                });
            }
        }
        out.push_str(&serde_json::to_string(&format_go_timestamp(secs, nanos)).unwrap_or_default());
        Ok(())
    }

    fn unmarshal_duration(&mut self, out: &mut String) -> Result<(), WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::String {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        let raw = self.dec.raw_lossy(&tok);
        let Some((secs, nanos)) = parse_go_duration(&tok.str_value) else {
            return Err(WalkError::WithPos {
                pos: tok.start,
                msg: format!("invalid google.protobuf.Duration value {raw}"),
            });
        };
        if !(-MAX_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&secs) {
            return Err(WalkError::WithPos {
                pos: tok.start,
                msg: format!("google.protobuf.Duration value out of range: {raw}"),
            });
        }
        out.push_str(&serde_json::to_string(&format_go_duration(secs, nanos)).unwrap_or_default());
        Ok(())
    }

    fn unmarshal_value(&mut self, out: &mut String) -> Result<(), WalkError> {
        let tok = self.dec.peek().map_err(WalkError::Detail)?;
        match tok.kind {
            Kind::Null => {
                self.dec.read().map_err(WalkError::Detail)?;
                out.push_str("null");
                Ok(())
            }
            Kind::Bool => {
                let tok = self.dec.read().map_err(WalkError::Detail)?;
                out.push_str(if tok.bool_value { "true" } else { "false" });
                Ok(())
            }
            Kind::Number => {
                let tok = self.dec.read().map_err(WalkError::Detail)?;
                let raw = self.dec.raw(&tok).to_vec();
                if token_float(&tok, &raw, false).is_none() {
                    return Err(self.invalid_value_token(&tok));
                }
                out.push_str(&String::from_utf8_lossy(&raw));
                Ok(())
            }
            Kind::String => {
                let tok = self.dec.read().map_err(WalkError::Detail)?;
                out.push_str(&serde_json::to_string(&tok.str_value).unwrap_or_default());
                Ok(())
            }
            Kind::ObjectOpen => {
                let schema = self
                    .set
                    .message("google.protobuf.Struct")
                    .ok_or(WalkError::Unsupported)?;
                self.unmarshal_map(
                    schema
                        .fields
                        .iter()
                        .find(|field| field.name == "fields")
                        .ok_or(WalkError::Unsupported)?,
                    out,
                )
            }
            Kind::ArrayOpen => {
                let schema = self
                    .set
                    .message("google.protobuf.ListValue")
                    .ok_or(WalkError::Unsupported)?;
                self.unmarshal_list(
                    schema
                        .fields
                        .iter()
                        .find(|field| field.name == "values")
                        .ok_or(WalkError::Unsupported)?,
                    out,
                )
            }
            _ => Err(self.invalid_value_token(&tok)),
        }
    }

    fn invalid_value_token(&mut self, tok: &Token) -> WalkError {
        let raw = self.dec.raw_lossy(tok);
        WalkError::WithPos {
            pos: tok.start,
            msg: format!("invalid google.protobuf.Value: {raw}"),
        }
    }

    fn unmarshal_field_mask(&mut self, out: &mut String) -> Result<(), WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::String {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        let text = tok.str_value.trim_matches(go_space);
        if text.is_empty() {
            out.push_str("\"\"");
            return Ok(());
        }
        let mut paths: Vec<String> = Vec::new();
        for part in text.split(',') {
            let snake = json_snake_case(part);
            if part.contains('_') || !is_valid_full_name(&snake) {
                return Err(WalkError::WithPos {
                    pos: tok.start,
                    msg: format!(
                        "google.protobuf.FieldMask.paths contains invalid path: {}",
                        go_quote_bytes(part.as_bytes())
                    ),
                });
            }
            paths.push(snake);
        }
        out.push_str(&serde_json::to_string(&paths.join(",")).unwrap_or_default());
        Ok(())
    }

    fn unmarshal_empty(&mut self, out: &mut String) -> Result<(), WalkError> {
        let tok = self.dec.read().map_err(WalkError::Detail)?;
        if tok.kind != Kind::ObjectOpen {
            let raw = self.dec.raw_lossy(&tok);
            return Err(WalkError::Syntax {
                pos: tok.start,
                msg: format!("unexpected token {raw}"),
            });
        }
        loop {
            let tok = self.dec.read().map_err(WalkError::Detail)?;
            match tok.kind {
                Kind::ObjectClose => {
                    out.push_str("{}");
                    return Ok(());
                }
                Kind::Name => self.skip_json_value()?,
                _ => {
                    let raw = self.dec.raw_lossy(&tok);
                    return Err(WalkError::Syntax {
                        pos: tok.start,
                        msg: format!("unexpected token {raw}"),
                    });
                }
            }
        }
    }

    fn skip_json_value(&mut self) -> Result<(), WalkError> {
        let mut open = 0i64;
        loop {
            let tok = self.dec.read().map_err(WalkError::Detail)?;
            match tok.kind {
                Kind::ObjectClose | Kind::ArrayClose => open -= 1,
                Kind::ObjectOpen | Kind::ArrayOpen => {
                    open += 1;
                    if open > i64::from(self.remaining) {
                        return Err(WalkError::detail("exceeded max recursion depth"));
                    }
                }
                Kind::Eof => return Err(WalkError::detail("unexpected EOF")),
                _ => {}
            }
            if open == 0 {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_FIELDS: &[FieldSchema] = &[
        FieldSchema::scalar("path", "path", FieldKind::String),
        FieldSchema::scalar("depth", "depth", FieldKind::Uint32),
        FieldSchema {
            name: "args",
            json_name: "args",
            kind: FieldKind::String,
            repeated: true,
            map_value: None,
            map_key: FieldKind::String,
            message: None,
            enum_name: None,
            oneof: None,
        },
        FieldSchema {
            name: "envs",
            json_name: "envs",
            kind: FieldKind::String,
            repeated: false,
            map_value: Some(FieldKind::String),
            map_key: FieldKind::String,
            message: None,
            enum_name: None,
            oneof: None,
        },
        FieldSchema {
            name: "signal",
            json_name: "signal",
            kind: FieldKind::Enum,
            repeated: false,
            map_value: None,
            map_key: FieldKind::String,
            message: None,
            enum_name: Some("process.Signal"),
            oneof: None,
        },
        FieldSchema {
            name: "pid",
            json_name: "pid",
            kind: FieldKind::Uint32,
            repeated: false,
            map_value: None,
            map_key: FieldKind::String,
            message: None,
            enum_name: None,
            oneof: Some(0),
        },
        FieldSchema {
            name: "tag",
            json_name: "tag",
            kind: FieldKind::String,
            repeated: false,
            map_value: None,
            map_key: FieldKind::String,
            message: None,
            enum_name: None,
            oneof: Some(0),
        },
    ];
    static MESSAGES: &[MessageSchema] = &[MessageSchema {
        full_name: "test.Request",
        fields: ENV_FIELDS,
        oneofs: &["test.Selector.selector"],
    }];
    static ENUMS: &[EnumSchema] = &[EnumSchema {
        full_name: "process.Signal",
        values: &[("SIGNAL_NONE", 0), ("SIGNAL_KILL", 9)],
    }];
    static SET: SchemaSet = SchemaSet {
        messages: MESSAGES,
        enums: ENUMS,
    };

    fn decode_json(body: &[u8]) -> Result<String, String> {
        decode_with(&SET, &MESSAGES[0], body)
            .expect("schema is complete")
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .map_err(|detail| detail.text)
    }

    #[test]
    fn accepts_and_canonicalises() {
        assert_eq!(
            decode_json(br#"{"path":"a","depth":2}"#).unwrap(),
            r#"{"path":"a","depth":2}"#
        );
        assert_eq!(
            decode_json(br#"{"path":null,"depth":"1e2"}"#).unwrap(),
            r#"{"depth":100}"#
        );
        assert_eq!(decode_json(b"{}").unwrap(), "{}");
        assert_eq!(
            decode_json(b"[]").unwrap_err(),
            "syntax error (line 1:1): unexpected token ["
        );
    }

    #[test]
    fn quoted_numbers_and_leading_zeros() {
        assert_eq!(
            decode_json(br#"{"depth":"05"}"#).unwrap_err(),
            "(line 1:11): invalid value for uint32 field depth: \"05\""
        );
        assert_eq!(
            decode_json(br#"{"depth":"+5"}"#).unwrap_err(),
            "(line 1:11): invalid value for uint32 field depth: \"+5\""
        );
        assert_eq!(decode_json(br#"{"depth":5e}"#).unwrap(), r#"{"depth":5}"#);
    }

    #[test]
    fn schema_errors_match_go() {
        assert_eq!(
            decode_json(br#"{"path":5}"#).unwrap_err(),
            "(line 1:9): invalid value for string field path: 5"
        );
        assert_eq!(
            decode_json(br#"{"depth":-1}"#).unwrap_err(),
            "(line 1:10): invalid value for uint32 field depth: -1"
        );
        assert_eq!(
            decode_json(br#"{"path":"a","path":"b"}"#).unwrap_err(),
            "(line 1:13): duplicate field \"path\""
        );
        assert_eq!(
            decode_json(br#"{"pid":1,"tag":"t"}"#).unwrap_err(),
            "(line 1:11): error parsing \"tag\", oneof test.Selector.selector is already set"
        );
    }

    #[test]
    fn enum_discard_and_alias() {
        assert_eq!(decode_json(br#"{"signal":"SIGNAL_FOO"}"#).unwrap(), "{}");
        assert_eq!(
            decode_json(br#"{"signal":"SIGNAL_KILL"}"#).unwrap(),
            r#"{"signal":9}"#
        );
        assert_eq!(
            decode_json(br#"{"signal":1.5}"#).unwrap_err(),
            "(line 1:11): invalid value for enum field signal: 1.5"
        );
    }

    #[test]
    fn map_rules() {
        assert_eq!(
            decode_json(br#"{"envs":{"A":"1","A":"2"}}"#).unwrap_err(),
            r#"(line 1:21): duplicate map key "A""#
        );
        assert_eq!(
            decode_json(br#"{"envs":{"A":null}}"#).unwrap_err(),
            "(line 1:17): invalid value for string field value: null"
        );
    }

    #[test]
    fn syntax_errors_match_go() {
        assert_eq!(
            decode_json(b"   ").unwrap_err(),
            "syntax error (line 1:4): unexpected token "
        );
        assert_eq!(
            decode_json(br#"{"path":"a" "b"}"#).unwrap_err(),
            "syntax error (line 1:13): unexpected token \"b\""
        );
        assert_eq!(
            decode_json(br#"{"foo":"\q"}"#).unwrap_err(),
            "syntax error (line 1:8): invalid escape code \"\\\\q\" in string"
        );
        assert_eq!(
            decode_json(b"{\"foo\":\"\xff\"}").unwrap_err(),
            "syntax error (line 1:8): invalid UTF-8 in string"
        );
        assert_eq!(
            decode_json(br#"{"foo":"\ud800"}"#).unwrap_err(),
            "unexpected EOF"
        );
    }

    #[test]
    fn skip_depth_boundary() {
        let build = |depth: usize| {
            let mut body = b"{\"x\":".to_vec();
            body.extend(std::iter::repeat_n(b'[', depth));
            body.extend(std::iter::repeat_n(b']', depth));
            body.push(b'}');
            body
        };
        assert_eq!(decode_json(&build(9_999)).unwrap(), "{}");
        assert_eq!(
            decode_json(&build(10_000)).unwrap_err(),
            "exceeded max recursion depth"
        );
    }

    #[test]
    fn extension_names_resolve_to_wrong_type() {
        assert_eq!(
            decode_json(br#"{"[test.Request]":1}"#).unwrap_err(),
            "(line 1:2): unable to resolve \"[test.Request]\": found wrong type: got message, want extension"
        );
        assert_eq!(
            decode_json(br#"{"[process.Signal]":0}"#).unwrap_err(),
            "(line 1:2): unable to resolve \"[process.Signal]\": found wrong type: got enum, want extension"
        );
        assert_eq!(decode_json(br#"{"[foo.bar]":1}"#).unwrap(), "{}");
    }

    #[test]
    fn invalid_utf8_is_flagged_for_escaping() {
        let detail = decode_with(&SET, &MESSAGES[0], b"{\"path\":\"\xff\"}")
            .expect("schema is complete")
            .unwrap_err();
        assert!(detail.escapes_replacement);
        assert!(detail.text.contains("invalid UTF-8 in string"));
    }
}
