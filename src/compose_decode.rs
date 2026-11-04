//! Incremental encoding/json scanner for compose's single Decoder.Decode.
pub(crate) use crate::goquote::quote;
#[derive(Clone, Copy, Default)]
enum ScanState {
    #[default]
    Begin,
    ArrayBegin,
    ObjectBegin,
    ObjectKey,
    End,
    String,
    Escape,
    Hex(u8),
    Minus,
    Zero,
    Digits,
    Dot,
    Fraction,
    Exponent,
    ExponentSign,
    ExponentDigits,
    Literal(&'static [u8], usize),
}
#[derive(Clone, Copy)]
enum Container {
    ObjectKey,
    ObjectValue,
    Array,
}
#[derive(Default)]
pub(crate) struct Scanner {
    state: ScanState,
    stack: Vec<Container>,
}
pub(crate) fn space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\n' | b'\r' | b'\t')
}
fn syntax(byte: u8, context: &str) -> String {
    let quoted = if byte == b'\'' {
        "\\'".into()
    } else if byte == b'"' {
        "\"".into()
    } else {
        let quoted = quote(&char::from(byte).to_string());
        quoted[1..quoted.len() - 1].to_owned()
    };
    format!("invalid character '{quoted}' {context}")
}
impl Scanner {
    fn push(&mut self, byte: u8, container: Container, state: ScanState) -> Result<bool, String> {
        if self.stack.len() == 10_000 {
            return Err(syntax(byte, "exceeded max depth"));
        }
        self.stack.push(container);
        self.state = state;
        Ok(false)
    }
    fn finish_container(&mut self) -> bool {
        self.stack.pop();
        self.state = ScanState::End;
        self.stack.is_empty()
    }
    pub(crate) fn eof(&self) -> bool {
        self.stack.is_empty()
            && matches!(
                self.state,
                ScanState::End
                    | ScanState::Zero
                    | ScanState::Digits
                    | ScanState::Fraction
                    | ScanState::ExponentDigits
            )
    }
    pub(crate) fn step(&mut self, byte: u8) -> Result<bool, String> {
        use ScanState::*;
        match self.state {
            Begin => self.begin(byte),
            ArrayBegin if byte == b']' => Ok(self.finish_container()),
            ArrayBegin if !space(byte) => self.begin(byte),
            ObjectBegin if byte == b'}' => Ok(self.finish_container()),
            ObjectBegin | ObjectKey if !space(byte) => {
                if byte != b'"' {
                    return Err(syntax(byte, "looking for beginning of object key string"));
                }
                self.state = String;
                Ok(false)
            }
            ArrayBegin | ObjectBegin | ObjectKey => Ok(false),
            End => self.end(byte),
            String => {
                self.state = match byte {
                    b'"' => End,
                    b'\\' => Escape,
                    0..=31 => return Err(syntax(byte, "in string literal")),
                    _ => String,
                };
                Ok(false)
            }
            Escape => {
                self.state = match byte {
                    b'b' | b'f' | b'n' | b'r' | b't' | b'\\' | b'/' | b'"' => String,
                    b'u' => Hex(4),
                    _ => return Err(syntax(byte, "in string escape code")),
                };
                Ok(false)
            }
            Hex(left) => {
                if !byte.is_ascii_hexdigit() {
                    return Err(syntax(byte, "in \\u hexadecimal character escape"));
                }
                self.state = if left == 1 { String } else { Hex(left - 1) };
                Ok(false)
            }
            Literal(literal, index) => {
                if byte != literal[index] {
                    let name = std::str::from_utf8(literal).unwrap_or_default();
                    return Err(syntax(
                        byte,
                        &format!(
                            "in literal {name} (expecting '{}')",
                            char::from(literal[index])
                        ),
                    ));
                }
                self.state = if index + 1 == literal.len() {
                    End
                } else {
                    Literal(literal, index + 1)
                };
                Ok(false)
            }
            Minus => match byte {
                b'0' => {
                    self.state = Zero;
                    Ok(false)
                }
                b'1'..=b'9' => {
                    self.state = Digits;
                    Ok(false)
                }
                _ => Err(syntax(byte, "in numeric literal")),
            },
            Digits | Fraction | ExponentDigits if byte.is_ascii_digit() => Ok(false),
            Zero | Digits | Fraction if matches!(byte, b'e' | b'E') => {
                self.state = Exponent;
                Ok(false)
            }
            Zero | Digits if byte == b'.' => {
                self.state = Dot;
                Ok(false)
            }
            Zero | Digits | Fraction | ExponentDigits => self.end(byte),
            Dot => {
                if !byte.is_ascii_digit() {
                    return Err(syntax(byte, "after decimal point in numeric literal"));
                }
                self.state = Fraction;
                Ok(false)
            }
            Exponent if matches!(byte, b'+' | b'-') => {
                self.state = ExponentSign;
                Ok(false)
            }
            Exponent | ExponentSign => {
                if !byte.is_ascii_digit() {
                    return Err(syntax(byte, "in exponent of numeric literal"));
                }
                self.state = ExponentDigits;
                Ok(false)
            }
        }
    }
    fn begin(&mut self, byte: u8) -> Result<bool, String> {
        use ScanState::*;
        self.state = match byte {
            b'{' => return self.push(byte, Container::ObjectKey, ObjectBegin),
            b'[' => return self.push(byte, Container::Array, ArrayBegin),
            b'"' => String,
            b'-' => Minus,
            b'0' => Zero,
            b'1'..=b'9' => Digits,
            b't' => Literal(b"true", 1),
            b'f' => Literal(b"false", 1),
            b'n' => Literal(b"null", 1),
            byte if space(byte) => return Ok(false),
            _ => return Err(syntax(byte, "looking for beginning of value")),
        };
        Ok(false)
    }
    fn end(&mut self, byte: u8) -> Result<bool, String> {
        self.state = ScanState::End;
        let Some(container) = self.stack.last_mut() else {
            return Ok(true);
        };
        if space(byte) {
            return Ok(false);
        }
        match *container {
            Container::ObjectKey if byte == b':' => {
                *container = Container::ObjectValue;
                self.state = ScanState::Begin;
            }
            Container::ObjectValue if byte == b',' => {
                *container = Container::ObjectKey;
                self.state = ScanState::ObjectKey;
            }
            Container::Array if byte == b',' => self.state = ScanState::Begin,
            Container::ObjectValue if byte == b'}' => return Ok(self.finish_container()),
            Container::Array if byte == b']' => return Ok(self.finish_container()),
            Container::ObjectKey => return Err(syntax(byte, "after object key")),
            Container::ObjectValue => return Err(syntax(byte, "after object key:value pair")),
            Container::Array => return Err(syntax(byte, "after array element")),
        }
        Ok(false)
    }
}

/// ASCII case-insensitive key fold with Go's special runs: `ſ` (U+017F) folds
/// to `S` and the Kelvin sign (U+212A) folds to `K`, as `encoding/json` does
/// when matching struct fields.
pub(crate) fn folded_field(key: &str) -> String {
    key.chars()
        .map(|ch| match ch {
            'ſ' => 'S',
            'K' => 'K',
            ch => ch.to_ascii_uppercase(),
        })
        .collect()
}
