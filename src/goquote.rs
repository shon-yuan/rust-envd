//! Go strconv.Quote — the `%q` rendering the oracle's error strings carry
//! (go/src/strconv/quote.go). One shared port for every quoted path: the
//! compose scanner's "source path %q" refusals and the filesystem runtime's
//! "cyclic symlink ... at %q". Escapes \a \b \f \n \r \t \v byte-exactly,
//! other controls as \xNN, non-printable runes as \uXXXX/\UXXXXXXXX per the
//! Go IsPrint tables in [`printable`], and passes printable non-ASCII through
//! verbatim.

#[path = "daemon/compose_quote.rs"]
mod printable;
use printable::is_print;

pub fn quote(value: &str) -> String {
    let mut quoted = String::from("\"");
    for rune in value.chars() {
        match rune {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{7}' => quoted.push_str("\\a"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{b}' => quoted.push_str("\\v"),
            rune if rune <= '\u{1f}' || rune == '\u{7f}' => {
                quoted.push_str(&format!("\\x{:02x}", rune as u32))
            }
            rune if !is_print(rune) => {
                if rune <= '\u{ffff}' {
                    quoted.push_str(&format!("\\u{:04x}", rune as u32))
                } else {
                    quoted.push_str(&format!("\\U{:08x}", rune as u32))
                }
            }
            rune => quoted.push(rune),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_match_go_strconv_quote_byte_for_byte() {
        // Go: strconv.Quote("\a\b\v\f\n\t\r\x00\x7f\"\\ é\u{85}")
        // = "\"\\a\\b\\v\\f\\n\\t\\r\\x00\\x7f\\\"\\\\ é\\u0085\"".
        // \a\b\v\f are single-letter Go escapes (not \x07); U+0085 is
        // non-printable per IsPrint so it becomes \u0085; é (U+00e9) is
        // printable and passes through.
        let quoted = quote("\u{7}\u{8}\u{b}\u{c}\n\t\r\u{0}\u{7f}\"\\ é\u{85}");
        assert_eq!(
            quoted,
            "\"\\a\\b\\v\\f\\n\\t\\r\\x00\\x7f\\\"\\\\ é\\u0085\""
        );
        // Beyond BMP: printable astral runes pass through — including the
        // variation selectors above U+20000, which IsPrint treats as
        // printable Mn without consulting the not-print table — while an
        // unassigned hole (U+1D4AD, in NOT_PRINT32 as 0xd4ad) gets the
        // \UXXXXXXXX form.
        assert_eq!(quote("\u{e000}"), "\"\\ue000\"");
        assert_eq!(quote("\u{1f600}"), "\"\u{1f600}\"");
        assert_eq!(quote("\u{e01ef}"), "\"\u{e01ef}\"");
        assert_eq!(quote("\u{1d4ad}"), "\"\\U0001d4ad\"");
    }
}
