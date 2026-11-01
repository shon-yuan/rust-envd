//! The x509 acceptance gate pinned to the pinned Go crypto/x509 by the probe
//! battery in /tmp/go-cacerts-probe (cryptobyte envelope rules, integer
//! minimality, Name charset checks, UTCTime/GeneralizedTime formats with
//! canonical round-trip, BIT STRING padding, extension envelope shapes, and
//! the exact error strings). Documented residuals versus Go's full parser:
//! on-curve verification of EC points ("P256 point not on curve"), deep
//! GeneralName/value parsing beyond the basicConstraints/subjectAltName
//! envelope checks, and RSA/EC key integer depth are length/shape checks
//! here, and public-key key material is never reconstructed.

/// Validate one DER certificate exactly where the probe battery pins Go.
/// The returned detail is the bare Go error text (e.g. "x509: malformed
/// certificate"); callers add their own prefix.
pub fn validate_certificate(der: &[u8]) -> Result<(), String> {
    let malformed = || "x509: malformed certificate".to_string();
    let mut top = Reader::new(der);
    let (tag, certificate) = top.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err(malformed());
    }
    if !top.at_end() {
        return Err("x509: trailing data".to_string());
    }

    let mut outer = Reader::new(certificate);
    let (tag, tbs) = outer.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err(malformed());
    }
    let (tag, inner_alg) = outer.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err(malformed());
    }
    let (tag, signature) = outer.read_element().map_err(|_| malformed())?;
    if tag != 0x03 {
        return Err(malformed());
    }
    validate_bit_string(signature).map_err(|_| "x509: malformed signature".to_string())?;
    if !outer.at_end() {
        return Err("x509: trailing data".to_string());
    }

    parse_tbs(tbs, inner_alg)
}

fn parse_tbs(tbs: &[u8], outer_alg: &[u8]) -> Result<(), String> {
    let malformed = || "x509: malformed certificate".to_string();
    let mut r = Reader::new(tbs);

    // Optional [0] EXPLICIT version (encoded value is version - 1).
    let mut version = 0i64;
    if matches!(r.peek_tag(), Some(0xa0)) {
        let (_, explicit) = r.read_element().map_err(|_| malformed())?;
        let mut inner = Reader::new(explicit);
        let (tag, int_bytes) = inner
            .read_element()
            .map_err(|_| "x509: malformed version")?;
        if tag != 0x02 {
            return Err("x509: malformed version".to_string());
        }
        version = read_minimal_i64(int_bytes).ok_or("x509: malformed version")?;
        if version < 0 {
            return Err("x509: malformed version".to_string());
        }
        if version.checked_add(1).filter(|v| *v <= 3).is_none() {
            return Err("x509: invalid version".to_string());
        }
        version += 1;
    }

    // Serial: minimal, non-negative INTEGER of any length.
    let (tag, serial) = r
        .read_element()
        .map_err(|_| "x509: malformed serial number")?;
    if tag != 0x02 || !minimal_integer(serial) {
        return Err("x509: malformed serial number".to_string());
    }
    if serial.first().is_some_and(|b| b & 0x80 != 0) {
        return Err("x509: negative serial number".to_string());
    }

    // Inner signature algorithm identifier.
    let (tag, inner_alg) = r.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err(malformed());
    }
    check_algorithm_identifier(inner_alg).map_err(|_| malformed())?;

    // Issuer Name.
    let (tag, issuer) = r.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err("x509: malformed issuer".to_string());
    }
    parse_name(issuer)?;

    // Validity.
    let (tag, validity) = r.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err(malformed());
    }
    parse_validity(validity)?;

    // Subject Name.
    let (tag, subject) = r.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err("x509: malformed subject".to_string());
    }
    parse_name(subject)?;

    // Subject public key info: SEQUENCE { AlgorithmIdentifier, BIT STRING }.
    let (tag, spki) = r.read_element().map_err(|_| malformed())?;
    if tag != 0x30 {
        return Err("x509: malformed subjectPublicKey".to_string());
    }
    let mut spki_reader = Reader::new(spki);
    let (tag, algorithm) = spki_reader
        .read_element()
        .map_err(|_| "x509: malformed subjectPublicKey".to_string())?;
    if tag != 0x30 {
        return Err("x509: malformed subjectPublicKey".to_string());
    }
    check_algorithm_identifier(algorithm).map_err(|_| "x509: malformed subjectPublicKey")?;
    let (tag, key_bits) = spki_reader
        .read_element()
        .map_err(|_| "x509: malformed subjectPublicKey".to_string())?;
    if tag != 0x03 {
        return Err("x509: malformed subjectPublicKey".to_string());
    }
    validate_bit_string(key_bits).map_err(|_| "x509: malformed subjectPublicKey".to_string())?;

    // [1]/[2] unique identifiers are skipped regardless of version.
    while matches!(r.peek_tag(), Some(0x81 | 0x82)) {
        r.read_element().map_err(|_| malformed())?;
    }

    // [3] extensions are read only for v3; older versions tolerate them.
    if matches!(r.peek_tag(), Some(0xa3)) {
        if version == 3 {
            let (_, extensions) = r.read_element().map_err(|_| malformed())?;
            parse_extensions(extensions)?;
        } else {
            r.read_element().map_err(|_| malformed())?;
        }
    }
    if !r.at_end() {
        return Err("x509: trailing data".to_string());
    }

    // The two signature algorithm identifiers must match byte-for-byte.
    if inner_alg != outer_alg {
        return Err(
            "x509: inner and outer signature algorithm identifiers don't match".to_string(),
        );
    }
    Ok(())
}

/// parseAlgorithmIdentifier: a SEQUENCE holding an OID, parameters optional.
fn check_algorithm_identifier(content: &[u8]) -> Result<(), ()> {
    let mut r = Reader::new(content);
    let (tag, oid) = r.read_element()?;
    if tag != 0x06 || parse_oid(oid).is_err() {
        return Err(());
    }
    Ok(())
}

/// parseName: SEQUENCE OF SET OF SEQUENCE { OID, value } with per-tag charset
/// checks for the string types Go validates.
fn parse_name(name: &[u8]) -> Result<(), String> {
    let invalid_rdn = || "x509: invalid RDNSequence".to_string();
    let mut r = Reader::new(name);
    while !r.at_end() {
        let (tag, rdn) = r.read_element().map_err(|_| invalid_rdn())?;
        if tag != 0x31 {
            return Err(invalid_rdn());
        }
        let mut set = Reader::new(rdn);
        while !set.at_end() {
            let (tag, atav) = set.read_element().map_err(|_| invalid_rdn())?;
            if tag != 0x30 {
                return Err(invalid_rdn());
            }
            let mut attribute = Reader::new(atav);
            let (tag, oid) = attribute.read_element().map_err(|_| invalid_rdn())?;
            if tag != 0x06 {
                return Err("x509: invalid RDNSequence: invalid attribute type".to_string());
            }
            if parse_oid(oid).is_err() {
                return Err("x509: invalid RDNSequence: invalid attribute type".to_string());
            }
            let (tag, value) = attribute.read_element().map_err(|_| invalid_rdn())?;
            let invalid_value =
                |what: &str| format!("x509: invalid RDNSequence: invalid attribute value: {what}");
            match tag {
                0x13 => {
                    if !value
                        .iter()
                        .all(|b| b.is_ascii_alphanumeric() || b" '()+,-./:=?".contains(b))
                    {
                        return Err(invalid_value("invalid PrintableString"));
                    }
                }
                0x0c => {
                    if std::str::from_utf8(value).is_err() {
                        return Err(invalid_value("invalid UTF-8 string"));
                    }
                }
                0x12 => {
                    if !value.iter().all(|b| b.is_ascii_digit() || *b == b' ') {
                        return Err(invalid_value("invalid NumericString"));
                    }
                }
                0x16 => {
                    if !value.iter().all(|b| *b <= 0x7f) {
                        return Err(invalid_value("invalid IA5String"));
                    }
                }
                0x1e if value.len() % 2 != 0 => {
                    return Err(invalid_value("invalid BMPString"));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// parseValidity: exactly two time elements.
fn parse_validity(validity: &[u8]) -> Result<(), String> {
    let mut r = Reader::new(validity);
    let (tag, time) = r
        .read_element()
        .map_err(|_| "x509: unsupported time format")?;
    parse_time(tag, time)?;
    let (tag, time) = r
        .read_element()
        .map_err(|_| "x509: unsupported time format")?;
    parse_time(tag, time)?;
    if !r.at_end() {
        return Err("x509: trailing data".to_string());
    }
    Ok(())
}

/// readASN1Time: UTCTime "060102150405Z0700" (minute fallback) and
/// GeneralizedTime "20060102150405Z0700". Ranges are validated, and an
/// explicit offset of ±0000 is rejected because it does not round-trip
/// canonically (Go re-encodes and compares).
fn parse_time(tag: u8, bytes: &[u8]) -> Result<(), String> {
    let malformed = |what: &str| format!("x509: malformed {what}");
    match tag {
        0x17 => {
            let digits_end = match bytes.len() {
                11 | 15 => 10,
                13 | 17 => 12,
                _ => return Err(malformed("UTCTime")),
            };
            if !all_digits(&bytes[..digits_end]) {
                return Err(malformed("UTCTime"));
            }
            let two =
                |i: usize| -> u32 { (bytes[i] - b'0') as u32 * 10 + (bytes[i + 1] - b'0') as u32 };
            let year = {
                let yy = two(0);
                if yy < 50 {
                    2000 + yy as i64
                } else {
                    1900 + yy as i64
                }
            };
            check_civil(
                year,
                two(2),
                two(4),
                two(6),
                two(8),
                if digits_end == 12 {
                    Some(two(10))
                } else {
                    None
                },
            )
            .map_err(|_| malformed("UTCTime"))?;
            check_zone(&bytes[digits_end..]).map_err(|_| malformed("UTCTime"))
        }
        0x18 => {
            if bytes.len() != 15 && bytes.len() != 19 {
                return Err(malformed("GeneralizedTime"));
            }
            if !all_digits(&bytes[..14]) {
                return Err(malformed("GeneralizedTime"));
            }
            let four = |i: usize| -> i64 {
                bytes[i..i + 4]
                    .iter()
                    .fold(0i64, |acc, b| acc * 10 + (b - b'0') as i64)
            };
            let two =
                |i: usize| -> u32 { (bytes[i] - b'0') as u32 * 10 + (bytes[i + 1] - b'0') as u32 };
            check_civil(four(0), two(4), two(6), two(8), two(10), Some(two(12)))
                .map_err(|_| malformed("GeneralizedTime"))?;
            check_zone(&bytes[14..]).map_err(|_| malformed("GeneralizedTime"))
        }
        _ => Err("x509: unsupported time format".to_string()),
    }
}

fn all_digits(bytes: &[u8]) -> bool {
    bytes.iter().all(|b| b.is_ascii_digit())
}

fn check_civil(
    year: i64,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: Option<u32>,
) -> Result<(), ()> {
    if !(1..=12).contains(&month) {
        return Err(());
    }
    if day < 1 || day > days_in_month(year, month) {
        return Err(());
    }
    if hour > 23 || minute > 59 || second.is_some_and(|s| s > 59) {
        return Err(());
    }
    Ok(())
}

/// Zone: 'Z', or ±hhmm with hh <= 24 and mm <= 59; ±0000 never round-trips
/// (Go would re-encode it as Z).
fn check_zone(zone: &[u8]) -> Result<(), ()> {
    match zone {
        [b'Z'] => Ok(()),
        [sign @ (b'+' | b'-'), h1, h2, m1, m2] => {
            if !(h1.is_ascii_digit()
                && h2.is_ascii_digit()
                && m1.is_ascii_digit()
                && m2.is_ascii_digit())
            {
                return Err(());
            }
            let hour = (h1 - b'0') as u32 * 10 + (h2 - b'0') as u32;
            let minute = (m1 - b'0') as u32 * 10 + (m2 - b'0') as u32;
            if hour > 24 || minute > 59 {
                return Err(());
            }
            if *sign == b'+' && hour == 0 && minute == 0 {
                return Err(());
            }
            if *sign == b'-' && hour == 0 && minute == 0 {
                return Err(());
            }
            Ok(())
        }
        _ => Err(()),
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// parseExtensions: [3] wraps exactly one SEQUENCE of
/// SEQUENCE { OID, [BOOLEAN], OCTET STRING }.
fn parse_extensions(extensions: &[u8]) -> Result<(), String> {
    let malformed_extensions = || "x509: malformed extensions".to_string();
    let mut r = Reader::new(extensions);
    let (tag, sequence) = r.read_element().map_err(|_| malformed_extensions())?;
    if tag != 0x30 || !r.at_end() {
        return Err(malformed_extensions());
    }
    let mut r = Reader::new(sequence);
    let mut seen: Vec<String> = Vec::new();
    while !r.at_end() {
        let (tag, entry) = r.read_element().map_err(|_| malformed_extensions())?;
        if tag != 0x30 {
            return Err(malformed_extensions());
        }
        let mut ext = Reader::new(entry);
        let (tag, oid) = ext
            .read_element()
            .map_err(|_| "x509: malformed extension".to_string())?;
        if tag != 0x06 {
            return Err("x509: malformed extension".to_string());
        }
        let oid = parse_oid(oid).map_err(|_| "x509: malformed extension".to_string())?;
        if matches!(ext.peek_tag(), Some(0x01)) {
            let (_, critical) = ext
                .read_element()
                .map_err(|_| "x509: malformed extension critical field".to_string())?;
            if critical.len() != 1 || (critical[0] != 0x00 && critical[0] != 0xff) {
                return Err("x509: malformed extension critical field".to_string());
            }
        }
        let (tag, value) = ext
            .read_element()
            .map_err(|_| "x509: malformed extension value field".to_string())?;
        if tag != 0x04 || !ext.at_end() {
            return Err("x509: malformed extension value field".to_string());
        }
        if seen.contains(&oid) {
            return Err(format!(
                "x509: certificate contains duplicate extension with OID \"{oid}\""
            ));
        }
        if value_seen_basic_constraints(&oid) {
            let mut inner = Reader::new(value);
            let (tag, _) = inner
                .read_element()
                .map_err(|_| "x509: invalid basic constraints".to_string())?;
            if tag != 0x30 {
                return Err("x509: invalid basic constraints".to_string());
            }
        } else if oid == "2.5.29.17" {
            let mut inner = Reader::new(value);
            let (tag, sequence) = inner
                .read_element()
                .map_err(|_| "x509: invalid subject alternative names".to_string())?;
            if tag != 0x30 {
                return Err("x509: invalid subject alternative names".to_string());
            }
            let mut names = Reader::new(sequence);
            while !names.at_end() {
                names
                    .read_element()
                    .map_err(|_| "x509: invalid subject alternative names".to_string())?;
            }
        }
        seen.push(oid);
    }
    Ok(())
}

fn value_seen_basic_constraints(oid: &str) -> bool {
    oid == "2.5.29.19"
}

/// cryptobyte BIT STRING rules: at least the pad byte, pad <= 7, and the
/// padding bits of the final byte must be zero.
fn validate_bit_string(content: &[u8]) -> Result<(), ()> {
    let Some((&pad, data)) = content.split_first() else {
        return Err(());
    };
    if pad > 7 {
        return Err(());
    }
    if pad > 0 {
        match data.last() {
            Some(last) if last & ((1u16 << pad) - 1) as u8 != 0 => return Err(()),
            None => return Err(()),
            _ => {}
        }
    }
    Ok(())
}

/// cryptobyte checkASN1Integer shape rules (no value reconstruction).
fn minimal_integer(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return false;
    }
    if bytes.len() > 1 {
        let (first, second) = (bytes[0], bytes[1]);
        if (first == 0x00 && second & 0x80 == 0) || (first == 0xff && second & 0x80 != 0) {
            return false;
        }
    }
    true
}

/// The version integer must fit a minimal int64 (Go readASN1Integer into i64).
fn read_minimal_i64(bytes: &[u8]) -> Option<i64> {
    if !minimal_integer(bytes) || bytes.len() > 8 {
        return None;
    }
    let negative = bytes[0] & 0x80 != 0;
    let mut value: i64 = if negative { -1 } else { 0 };
    for byte in bytes {
        value = (value << 8) | *byte as i64;
    }
    Some(value)
}

/// parseObjectIdentifier: base-128 subidentifiers (the first encodes the
/// first two arcs), minimality enforced; returns the dotted string (used in
/// duplicate-OID error messages).
fn parse_oid(bytes: &[u8]) -> Result<String, ()> {
    let mut parts: Vec<u64> = Vec::new();
    let mut rest = bytes;
    let mut first = true;
    while !rest.is_empty() {
        // One base-128 subidentifier: continuation bits high, last byte clear,
        // and a leading 0x80 is a non-minimal encoding.
        let mut value: u64 = 0;
        let mut index = 0;
        loop {
            let byte = *rest.get(index).ok_or(())?;
            if index == 0 && byte == 0x80 {
                return Err(());
            }
            value = value
                .checked_mul(0x80)
                .and_then(|v| v.checked_add((byte & 0x7f) as u64))
                .ok_or(())?;
            index += 1;
            if byte & 0x80 == 0 {
                break;
            }
        }
        rest = &rest[index..];
        if first {
            first = false;
            let (arc1, arc2) = if value < 40 {
                (0, value)
            } else if value < 80 {
                (1, value - 40)
            } else {
                (2, value - 80)
            };
            parts.push(arc1);
            parts.push(arc2);
        } else {
            parts.push(value);
        }
    }
    if first {
        return Err(());
    }
    Ok(parts
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join("."))
}

/// DER TLV reader with cryptobyte's envelope rules: high-tag-number form is
/// rejected, indefinite length is rejected, long-form lengths are minimal and
/// at most four bytes.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn peek_tag(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// Returns the identifier octet and the element content, advancing past it.
    fn read_element(&mut self) -> Result<(u8, &'a [u8]), ()> {
        let tag = *self.buf.get(self.pos).ok_or(())?;
        if tag & 0x1f == 0x1f {
            return Err(());
        }
        let length_byte = *self.buf.get(self.pos + 1).ok_or(())?;
        let (length, header) = match length_byte {
            n if n < 0x80 => (n as usize, 2),
            0x80 => return Err(()),
            n => {
                let len_len = (n - 0x80) as usize;
                if len_len > 4 || self.buf.len() < self.pos + 2 + len_len {
                    return Err(());
                }
                let digits = &self.buf[self.pos + 2..self.pos + 2 + len_len];
                if digits[0] == 0 {
                    return Err(());
                }
                let mut length = 0usize;
                for digit in digits {
                    length = length.checked_mul(0x100).ok_or(())? + *digit as usize;
                }
                (length, 2 + len_len)
            }
        };
        let content_start = self.pos + header;
        let content_end = content_start.checked_add(length).ok_or(())?;
        if content_end > self.buf.len() {
            return Err(());
        }
        let content = &self.buf[content_start..content_end];
        self.pos = content_end;
        Ok((tag, content))
    }
}
