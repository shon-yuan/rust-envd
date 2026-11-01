//! CA bundle slice: pinned cabundle parse/reconcile, the x509 gate, and the
//! CaCertInstaller state machine. Contract: pinned internal/cabundle/bundle.go,
//! internal/host/cacerts.go (state.json 0600 intent-first, aggregate append,
//! e2b-<fingerprint>.crt sources, 1s reconcile, legacy e2b-ca.crt, semaphore
//! serialization), init.go:474-478 (SetData arm; failures fail the /init) and
//! init.go:730-747 writeInitError (ErrCAInstallInProgress -> 503). PEM and DER
//! acceptance pinned against the pinned Go toolchain by
//! /tmp/go-cacerts-probe (battery rows inline below). All filesystem effects
//! run under injected tempdir roots; the host never touches real trust stores.

#[path = "cacerts/fixtures.rs"]
mod fixtures;

use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use coorworker_envd::cacerts::{
    bundle_equal, make_cert_name, parse, reconcile, x509, CaCertInstaller, CaError,
};
use fixtures::{
    CERT_A_DER_HEX, CERT_A_FP, CERT_A_PEM, CERT_B_DER_HEX, CERT_B_FP, CERT_B_PEM, CERT_C_DER_HEX,
    CERT_C_FP, CERT_C_PEM,
};
use std::time::Duration;
use tower::ServiceExt;

fn der_hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

fn cert_a_der() -> Vec<u8> {
    der_hex_to_bytes(CERT_A_DER_HEX)
}

fn cert_b_der() -> Vec<u8> {
    der_hex_to_bytes(CERT_B_DER_HEX)
}

fn assert_fingerprints(certs: &[coorworker_envd::cacerts::Certificate], fps: &[&str]) {
    let got: Vec<&str> = certs.iter().map(|c| c.fingerprint.as_str()).collect();
    assert_eq!(got, fps);
}

// --- engine: pem gate, parse, equal, reconcile ---

/// Row shape of the probe's PEM battery: (name, bundle, expected).
type ParseRow = (
    &'static str,
    String,
    Result<Vec<&'static str>, &'static str>,
);

#[test]
fn parse_matches_the_probed_go_acceptance() {
    let malformed = "-----BEGIN CERTIFICATE-----\n!!! invalid !!!\n-----END CERTIFICATE-----\n";
    let rows: &[ParseRow] = &[
        ("valid single", CERT_A_PEM.to_owned(), Ok(vec![CERT_A_FP])),
        (
            "valid pair",
            format!("{CERT_A_PEM}{CERT_B_PEM}"),
            Ok(vec![CERT_A_FP, CERT_B_FP]),
        ),
        (
            "duplicate keeps the first occurrence",
            format!("{CERT_A_PEM}{CERT_A_PEM}"),
            Ok(vec![CERT_A_FP]),
        ),
        ("empty", String::new(), Err("no certificates")),
        ("\n\n whitespace only", "\n\n".to_owned(), Err("no certificates")),
        (
            "leading blank lines",
            format!("\r\n\r\n{CERT_A_PEM}"),
            Ok(vec![CERT_A_FP]),
        ),
        (
            "crlf line endings",
            CERT_A_PEM.replace('\n', "\r\n"),
            Ok(vec![CERT_A_FP]),
        ),
        (
            "headers on the delivered cert are dropped",
            format!(
                "-----BEGIN CERTIFICATE-----\nComment: hello\n{}",
                CERT_A_PEM.split_once("-----\n").unwrap().1
            ),
            Ok(vec![CERT_A_FP]),
        ),
        (
            // Probe /tmp/go-cacerts-probe/out.txt row "not a certificate":
            // pem.Decode rejects the 3-char final group "A==" (padding '=' at
            // position 1) so Parse never reaches x509. The oracle's own
            // RejectsInvalidBundle only pins require.Error, not the message.
            "not a certificate (bad padding)",
            "-----BEGIN CERTIFICATE-----\nMIIBcTCCARegAwIBAgIUTestCertA==\n-----END CERTIFICATE-----\n"
                .to_owned(),
            Err("malformed certificate PEM"),
        ),
        (
            // Go StdEncoding requires canonical padding: an unpadded final
            // quantum is CorruptInputError (base64.go:316-321, probe-confirmed
            // in /tmp/go-fxround-probe: "MQ"/"MQU" -> illegal base64 data).
            "unpadded one-byte tail",
            "-----BEGIN CERTIFICATE-----\nMQ\n-----END CERTIFICATE-----\n".to_owned(),
            Err("malformed certificate PEM"),
        ),
        (
            "unpadded two-byte tail",
            "-----BEGIN CERTIFICATE-----\nMQU\n-----END CERTIFICATE-----\n".to_owned(),
            Err("malformed certificate PEM"),
        ),
        (
            // Two data morsels followed by a single '=': decodeQuantum's
            // case 2 needs the second '=' and hits end of input
            // ("not enough padding", probe-confirmed).
            "truncated padding (QQ=)",
            "-----BEGIN CERTIFICATE-----\nQQ=\n-----END CERTIFICATE-----\n".to_owned(),
            Err("malformed certificate PEM"),
        ),
        (
            "malformed block first",
            format!("{malformed}{CERT_A_PEM}"),
            Err("malformed certificate PEM"),
        ),
        (
            "malformed block between",
            format!("{CERT_A_PEM}{malformed}{CERT_A_PEM}"),
            Err("malformed certificate PEM"),
        ),
        (
            "unterminated block",
            format!("-----BEGIN CERTIFICATE-----\nMIIB\n{CERT_A_PEM}"),
            Err("malformed certificate PEM"),
        ),
        (
            "text between blocks",
            format!("{CERT_A_PEM}junk\n{CERT_A_PEM}"),
            Err("malformed certificate PEM"),
        ),
        (
            "truncated tail",
            format!("{}{}", CERT_A_PEM, &CERT_A_PEM[..CERT_A_PEM.len() / 2]),
            Err("malformed certificate PEM"),
        ),
        (
            "begin marker must start a line",
            format!(
                "x-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n",
                base64_of(&cert_a_der())
            ),
            Err("malformed certificate PEM"),
        ),
        (
            "only an end marker",
            "-----END CERTIFICATE-----\n".to_owned(),
            Err("malformed certificate PEM"),
        ),
        (
            "lowercase begin marker",
            CERT_A_PEM.replacen("-----BEGIN", "-----begin", 1),
            Err("malformed certificate PEM"),
        ),
        (
            "type trailing junk is a type mismatch",
            format!(
                "-----BEGIN CERTIFICATE extra-----\n{}\n-----END CERTIFICATE extra-----\n",
                base64_of(&cert_a_der())
            ),
            Err("PEM block is \"CERTIFICATE extra\", not a certificate"),
        ),
        (
            // Go %q byte escapes: quote, backslash, tab, CR and a control
            // byte in the type line all render with escapes; a literal
            // newline cannot appear in a getLine line, which is why the \n
            // arm is unreachable from parsed input.
            "type escapes follow go %q",
            {
                let typ = "CERT\"X\\Y\tZ\rQ\u{1}W";
                format!(
                    "-----BEGIN {typ}-----\n{}\n-----END {typ}-----\n",
                    base64_of(&cert_a_der())
                )
            },
            Err("PEM block is \"CERT\\\"X\\\\Y\\tZ\\rQ\\x01W\", not a certificate"),
        ),
        (
            "spaces inside base64 break the begin line",
            CERT_A_PEM.replacen('\n', " ", 2),
            Err("malformed certificate PEM"),
        ),
        (
            "non-colon line before the next block",
            format!(
                "{CERT_A_PEM}\nno colon line\n{CERT_A_PEM}"
            ),
            Err("malformed certificate PEM"),
        ),
    ];
    for (name, bundle, expected) in rows {
        let parsed = parse(bundle);
        match expected {
            Ok(fps) => {
                let certs = parsed.unwrap_or_else(|e| panic!("{name}: unexpected error {e}"));
                assert_fingerprints(&certs, fps);
                // Normalized PEM: canonical 64-column re-encode, headers gone.
                let normalized = certs
                    .iter()
                    .map(|c| String::from_utf8(c.pem.clone()).unwrap())
                    .collect::<Vec<_>>()
                    .join("");
                let canonical = |fingerprint: &str| {
                    if fingerprint == CERT_A_FP {
                        canonical_pem(&cert_a_der())
                    } else if fingerprint == CERT_B_FP {
                        canonical_pem(&der_hex_to_bytes(CERT_B_DER_HEX))
                    } else {
                        canonical_pem(&der_hex_to_bytes(CERT_C_DER_HEX))
                    }
                };
                let expected_pem: String = fps.iter().map(|fp| canonical(fp)).collect();
                assert_eq!(normalized, expected_pem, "{name}");
            }
            Err(message) => {
                let error = parsed
                    .err()
                    .unwrap_or_else(|| panic!("{name}: expected error"));
                assert_eq!(error.to_string(), *message, "{name}");
            }
        }
    }
}

fn base64_of(der: &[u8]) -> String {
    use base64::Engine as _;
    // Same 64-column wrapping Go's pem.EncodeToMemory produces.
    let encoded = base64::engine::general_purpose::STANDARD.encode(der);
    let mut out = String::new();
    for chunk in encoded.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap());
        out.push('\n');
    }
    // The probe passes raw base64 with explicit \n terminators on both sides.
    out
}

fn canonical_pem(der: &[u8]) -> String {
    format!(
        "-----BEGIN CERTIFICATE-----\n{}-----END CERTIFICATE-----\n",
        base64_of(der)
    )
}

#[test]
fn bundle_equal_ignores_order_and_formatting() {
    let single = parse(CERT_A_PEM).unwrap();
    let pair = parse(&format!("{CERT_A_PEM}{CERT_B_PEM}")).unwrap();
    let reordered = parse(&format!("{CERT_B_PEM}{CERT_A_PEM}")).unwrap();
    let duplicated = parse(&format!("{CERT_A_PEM}{CERT_A_PEM}")).unwrap();
    assert!(bundle_equal(&single, &single));
    assert!(bundle_equal(&pair, &reordered));
    assert!(!bundle_equal(&single, &pair));
    // Same length but different identities is not equal.
    assert!(!bundle_equal(&duplicated, &pair));
    let empty: Vec<coorworker_envd::cacerts::Certificate> = Vec::new();
    assert!(bundle_equal(&empty, &empty));
    assert!(!bundle_equal(&empty, &single));
}

#[test]
fn reconcile_strips_owned_identities_and_appends_desired() {
    // Aggregate: system roots (C) plus a previously-installed A with headers.
    let mut preexisting = String::new();
    preexisting.push_str("# comment kept\n");
    preexisting.push_str(CERT_C_PEM);
    preexisting.push_str(&format!(
        "-----BEGIN CERTIFICATE-----\nComment: test certificate\n{}",
        CERT_A_PEM.split_once("-----\n").unwrap().1
    ));
    let owned_fp = CERT_A_FP.to_owned();
    let desired = parse(CERT_B_PEM).unwrap();
    let content = reconcile(preexisting.as_bytes(), &[owned_fp], &desired);
    let text = String::from_utf8(content).unwrap();
    // Desired is appended; owned A is gone; unrelated C keeps its PEM bytes
    // (comment line is dropped: pem.Decode skips it silently, as pinned).
    assert!(text.ends_with(&canonical_pem(&cert_b_der())), "text={text}");
    assert!(!text.contains(&canonical_pem(&cert_a_der())[28..56]));
    assert!(text.contains(
        CERT_C_PEM
            .trim_end_matches('\n')
            .split('\n')
            .nth(1)
            .unwrap()
    ));
    // Managed blocks re-encode canonically; the header on C is preserved only
    // if C carried one (it does not here) - A's dropped header must be gone.
    assert!(!text.contains("Comment:"));
}

#[test]
fn reconcile_reencodes_blocks_with_sorted_headers() {
    // A non-managed block with headers is re-encoded through pem.Encode:
    // Proc-Type first (RFC 1421 4.6.1.1), remaining keys sorted, then the
    // blank separator line, then the body; the desired set is appended
    // (probed against the pinned Go oracle).
    let body = CERT_A_PEM.split_once("-----\n").unwrap().1;
    let with_headers =
        "-----BEGIN CERTIFICATE-----\nZeta: 1\nProc-Type: 4,ENCRYPTED\nDEK-Info: DES\nAlpha: 2\n"
            .to_owned()
            + body;
    let desired = parse(CERT_B_PEM).unwrap();
    let content = reconcile(with_headers.as_bytes(), &[], &desired);
    let expected = format!(
        "-----BEGIN CERTIFICATE-----\nProc-Type: 4,ENCRYPTED\nAlpha: 2\nDEK-Info: DES\nZeta: 1\n\n{body}{}",
        String::from_utf8_lossy(&desired[0].pem)
    );
    assert_eq!(String::from_utf8_lossy(&content), expected);
}

#[test]
fn make_cert_name_uses_the_sha256_fingerprint() {
    assert_eq!(make_cert_name(CERT_A_FP), format!("e2b-{CERT_A_FP}.crt"));
    assert_eq!(CERT_A_FP.len(), 64);
    assert!(CERT_A_FP
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}

// --- engine: the x509 gate (pinned DER battery) ---

// Minimal DER builder mirroring the Go probe (/tmp/go-cacerts-probe/der.go).
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x100 {
        out.extend_from_slice(&[0x81, n as u8]);
    } else {
        out.extend_from_slice(&[0x82, (n >> 8) as u8, n as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn cat(items: &[&[u8]]) -> Vec<u8> {
    items.iter().flat_map(|i| i.iter().copied()).collect()
}

fn seq(items: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &cat(items))
}

fn set(items: &[&[u8]]) -> Vec<u8> {
    tlv(0x31, &cat(items))
}

fn integer(bytes: &[u8]) -> Vec<u8> {
    tlv(0x02, bytes)
}

fn oid(bytes: &[u8]) -> Vec<u8> {
    tlv(0x06, bytes)
}

fn bit_string(bytes: &[u8]) -> Vec<u8> {
    tlv(0x03, bytes)
}

fn octet_string(bytes: &[u8]) -> Vec<u8> {
    tlv(0x04, bytes)
}

fn utf8_string(s: &str) -> Vec<u8> {
    tlv(0x0c, s.as_bytes())
}

fn raw_string(tag: u8, bytes: &[u8]) -> Vec<u8> {
    tlv(tag, bytes)
}

fn bool_der(b: u8) -> Vec<u8> {
    tlv(0x01, &[b])
}

fn utc_time(s: &str) -> Vec<u8> {
    tlv(0x17, s.as_bytes())
}

fn gen_time(s: &str) -> Vec<u8> {
    tlv(0x18, s.as_bytes())
}

fn name_cn(s: &[u8]) -> Vec<u8> {
    // Name ::= SEQUENCE OF SET OF (SEQUENCE { OID 2.5.4.3, value })
    let atav = seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x13, s)]);
    let rdn = set(&[&atav]);
    seq(&[&rdn])
}

const OID_SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_EC_PUB: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_BC: &[u8] = &[0x55, 0x1d, 0x13];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];

fn alg_sha() -> Vec<u8> {
    seq(&[&oid(OID_SHA256_RSA), &tlv(0x05, &[])])
}

fn alg_ec() -> Vec<u8> {
    seq(&[&oid(OID_EC_PUB), &oid(OID_P256)])
}

/// The probe's synthetic v3 certificate: real fixture EC point, fake signature.
// Test transcription of the Go probe's builder signature; a builder struct
// would be churn for a one-to-one probe mirror.
#[allow(clippy::too_many_arguments)]
fn build_cert(
    version: Option<&[u8]>,
    serial: &[u8],
    inner_alg: Option<&[u8]>,
    outer_alg: Option<&[u8]>,
    issuer: Option<&[u8]>,
    subject: Option<&[u8]>,
    not_before: &[u8],
    not_after: &[u8],
    spki_bits: Option<&[u8]>,
    unique_ids: bool,
    extensions: Option<&[u8]>,
    sig_bits: Option<&[u8]>,
) -> Vec<u8> {
    let point = cert_a_der();
    // SPKI BIT STRING content: 00 pad + the 65-byte uncompressed point, which
    // sits after the fixture's `03 42 00` prefix.
    let point_start = point
        .windows(3)
        .position(|w| w == [0x03, 0x42, 0x00])
        .unwrap();
    let point = &point[point_start + 3..point_start + 3 + 65];
    let default_spki = bit_string(&cat(&[&[0x00], point]));
    let sha = alg_sha();
    let tbs_inner_alg = inner_alg.unwrap_or(&sha);
    let outer_alg_value = outer_alg.unwrap_or(&sha);
    let default_issuer = name_cn(b"probe issuer");
    let default_subject = name_cn(b"probe subject");
    let issuer = issuer.unwrap_or(&default_issuer);
    let subject = subject.unwrap_or(&default_subject);
    let spki = spki_bits.unwrap_or(&default_spki);
    let mut tbs_items: Vec<Vec<u8>> = Vec::new();
    if let Some(version) = version {
        tbs_items.push(version.to_vec());
    }
    tbs_items.push(serial.to_vec());
    tbs_items.push(tbs_inner_alg.to_vec());
    tbs_items.push(issuer.to_vec());
    tbs_items.push(seq(&[not_before, not_after]));
    tbs_items.push(subject.to_vec());
    tbs_items.push(seq(&[&alg_ec(), spki]));
    if unique_ids {
        tbs_items.push(tlv(0x81, &[0x01]));
        tbs_items.push(tlv(0x82, &[0x01]));
    }
    if let Some(extensions) = extensions {
        tbs_items.push(extensions.to_vec());
    }
    let refs: Vec<&[u8]> = tbs_items.iter().map(|v| v.as_slice()).collect();
    let tbs = seq(&refs);
    let default_sig = bit_string(&[0x00, 0xde, 0xad]);
    let sig = sig_bits.unwrap_or(&default_sig);
    seq(&[&tbs, outer_alg_value, sig])
}

fn v3() -> Vec<u8> {
    tlv(0xa0, &integer(&[0x02]))
}

fn serial1() -> Vec<u8> {
    integer(&[0x01])
}

fn ext_entry(oid_bytes: &[u8], value: &[u8]) -> Vec<u8> {
    seq(&[&oid(oid_bytes), &octet_string(value)])
}

fn extensions_of(entries: &[&[u8]]) -> Vec<u8> {
    tlv(0xa3, &seq(entries))
}

#[test]
fn x509_gate_matches_the_probed_go_der_battery() {
    let rows: Vec<(String, Vec<u8>, Option<&str>)> = vec![
        ("control v3".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("v1 no version".into(), build_cert(None, &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("v2 encoded 1".into(), build_cert(Some(&tlv(0xa0, &integer(&[0x01]))), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("v4 encoded 3".into(), build_cert(Some(&tlv(0xa0, &integer(&[0x03]))), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid version")),
        ("version negative".into(), build_cert(Some(&tlv(0xa0, &integer(&[0xff]))), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed version")),
        ("version empty int".into(), build_cert(Some(&tlv(0xa0, &integer(&[]))), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed version")),
        ("version nonminimal".into(), build_cert(Some(&tlv(0xa0, &integer(&[0x00, 0x02]))), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed version")),
        ("version huge".into(), build_cert(Some(&tlv(0xa0, &integer(&[0; 9]))), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed version")),
        ("serial empty".into(), build_cert(Some(&v3()), &integer(&[]), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed serial number")),
        ("serial negative".into(), build_cert(Some(&v3()), &integer(&[0x81]), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: negative serial number")),
        ("serial nonminimal".into(), build_cert(Some(&v3()), &integer(&[0x00, 0x01]), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed serial number")),
        ("serial 24 bytes".into(), build_cert(Some(&v3()), &integer(&cat(&[&[0x01], &[0u8; 23]])), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        (
            "sigai mismatch".into(),
            {
                let inner = alg_ec();
                build_cert(Some(&v3()), &serial1(), Some(&inner), None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None)
            },
            Some("x509: inner and outer signature algorithm identifiers don't match"),
        ),
        (
            "sigai no params".into(),
            build_cert(Some(&v3()), &serial1(), Some(&seq(&[&oid(OID_SHA256_RSA)])), None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None),
            Some("x509: inner and outer signature algorithm identifiers don't match"),
        ),
        ("name empty seq".into(), build_cert(Some(&v3()), &serial1(), None, None, Some(&seq(&[])), None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("name not a seq".into(), build_cert(Some(&v3()), &serial1(), None, None, Some(&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x13, b"x")])])), None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed issuer")),
        ("name rdn not set".into(), build_cert(Some(&v3()), &serial1(), None, None, Some(&seq(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x13, b"x")])])), None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid RDNSequence")),
        ("name atav bad oid".into(), build_cert(Some(&v3()), &serial1(), None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0xff]), &raw_string(0x13, b"x")])])])), None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid RDNSequence: invalid attribute type")),
        ("name utf8 ok".into(), build_cert(Some(&v3()), &serial1(), None, None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &utf8_string("probe subject")])])])), &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("name utf8 invalid".into(), build_cert(Some(&v3()), &serial1(), None, None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x0c, b"bad\xc3")])])])), &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid RDNSequence: invalid attribute value: invalid UTF-8 string")),
        ("name bmp even".into(), build_cert(Some(&v3()), &serial1(), None, None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x1e, &[0x00, 0x41])])])])), &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("name bmp odd".into(), build_cert(Some(&v3()), &serial1(), None, None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x1e, &[0x00])])])])), &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid RDNSequence: invalid attribute value: invalid BMPString")),
        ("name numeric ok".into(), build_cert(Some(&v3()), &serial1(), None, None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x12, b"12 34")])])])), &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("name numeric bad".into(), build_cert(Some(&v3()), &serial1(), None, None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x12, b"12a")])])])), &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid RDNSequence: invalid attribute value: invalid NumericString")),
        ("name printable bad char".into(), build_cert(Some(&v3()), &serial1(), None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &raw_string(0x13, b"bad\x01name")])])])), None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: invalid RDNSequence: invalid attribute value: invalid PrintableString")),
        ("name atav bool".into(), build_cert(Some(&v3()), &serial1(), None, None, Some(&seq(&[&set(&[&seq(&[&oid(&[0x55, 0x04, 0x03]), &bool_der(0xff)])])])), None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc second precision".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc minute precision".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("2501010000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc offset -0700".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000-0700"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc offset +2400".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000+2400"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc offset +2401".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000+2401"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc offset +0059".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000+0059"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc offset +0000 canonicalizes to Z".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000+0000"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc offset -0000 canonicalizes to Z".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000-0000"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc offset hour 25".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000+2500"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc offset minute 60".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000+2460"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc month 13".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("251301000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc feb 30".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250230000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc leap feb29 2024".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("240229000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("utc feb29 2025".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250229000000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc hour 24".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101240000Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc sec 60".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000060Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed UTCTime")),
        ("utc year 50 boundary".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("500101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("gen z form".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &gen_time("20250101000000Z"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("gen offset -0700".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &gen_time("20250101000000-0700"), &utc_time("260101000000Z"), None, false, None, None), None),
        ("gen fraction".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &gen_time("20250101000000.5Z"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed GeneralizedTime")),
        ("gen offset +0000".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &gen_time("20250101000000+0000"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed GeneralizedTime")),
        ("gen no zone".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &gen_time("20250101000000"), &utc_time("260101000000Z"), None, false, None, None), Some("x509: malformed GeneralizedTime")),
        ("validity wrong tag".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &octet_string(&[0x01]), &utc_time("260101000000Z"), None, false, None, None), Some("x509: unsupported time format")),
        ("spki bitstring pad8".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), Some(&tlv(0x03, &[0x08, 0x04, 0x01])), false, None, None), Some("x509: malformed subjectPublicKey")),
        ("spki bitstring nonzero pad".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), Some(&bit_string(&[0x01, 0xff, 0xff])), false, None, None), Some("x509: malformed subjectPublicKey")),
        ("sig bitstring empty".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, Some(&bit_string(&[0x00]))), None),
        ("sig bitstring pad8".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, None, Some(&tlv(0x03, &[0x08, 0xff]))), Some("x509: malformed signature")),
        ("v3 unique ids".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, true, None, None), None),
        ("v1 unique ids tolerated".into(), build_cert(None, &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, true, None, None), None),
        (
            "v3 bc extension ok".into(),
            build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&ext_entry(OID_BC, &seq(&[&bool_der(0xff)]))])), None),
            None,
        ),
        (
            "v3 bc extension not a sequence".into(),
            build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&ext_entry(OID_BC, &bool_der(0xff))])), None),
            Some("parse certificate: x509: invalid basic constraints"),
        ),
        (
            "v3 duplicate extension oid".into(),
            build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&ext_entry(OID_BC, &seq(&[])), &ext_entry(OID_BC, &seq(&[]))])), None),
            Some("parse certificate: x509: certificate contains duplicate extension with OID \"2.5.29.19\""),
        ),
        (
            "v1 with extensions tolerated".into(),
            build_cert(None, &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&ext_entry(OID_BC, &seq(&[]))])), None),
            None,
        ),
        ("ext critical bool 0x01".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&seq(&[&oid(OID_BC), &bool_der(0x01), &octet_string(&[])])])), None), Some("x509: malformed extension critical field")),
        ("ext value not octet".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&seq(&[&oid(OID_BC), &bool_der(0xff), &raw_string(0x13, b"x")])])), None), Some("x509: malformed extension value field")),
        ("ext a3 inner not seq".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&tlv(0xa3, &ext_entry(OID_BC, &seq(&[])))), None), Some("x509: malformed extensions")),
        ("ext san garbage value".into(), build_cert(Some(&v3()), &serial1(), None, None, None, None, &utc_time("250101000000Z"), &utc_time("260101000000Z"), None, false, Some(&extensions_of(&[&ext_entry(OID_SAN, &[0x01, 0x02, 0x03])])), None), Some("parse certificate: x509: invalid subject alternative names")),
    ];
    let mut mismatches = Vec::new();
    // The battery pins the detail text itself; the envd cabundle wrapper's
    // "parse certificate: " prefix (pinned separately by the parse battery)
    // may appear on either side.
    fn detail(text: &str) -> &str {
        text.strip_prefix("parse certificate: ").unwrap_or(text)
    }
    for (name, der, expected) in &rows {
        let outcome = x509::validate_certificate(der).err();
        let matches = match (&outcome, expected) {
            (None, None) => true,
            (Some(actual), Some(expected)) => detail(actual) == detail(expected),
            _ => false,
        };
        if !matches {
            mismatches.push(format!("{name}: got {outcome:?}, want {expected:?}"));
        }
    }
    assert!(
        mismatches.is_empty(),
        "DER battery mismatches:\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn x509_gate_rejects_structural_corruption() {
    let base = build_cert(
        Some(&v3()),
        &serial1(),
        None,
        None,
        None,
        None,
        &utc_time("250101000000Z"),
        &utc_time("260101000000Z"),
        None,
        false,
        None,
        None,
    );
    let corrupted: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "trailing byte",
            cat(&[&base, &[0xff]]),
            "x509: trailing data",
        ),
        (
            "truncated mid",
            base[..base.len() - 3].to_vec(),
            "x509: malformed certificate",
        ),
        (
            "wrong outer tag",
            cat(&[&[0x31], &base[1..]]),
            "x509: malformed certificate",
        ),
        (
            "indefinite length",
            cat(&[&[0x30, 0x80], &base[2..]]),
            "x509: malformed certificate",
        ),
        (
            "nonminimal length",
            cat(&[&[0x30, 0x82, 0x00, (base.len() - 4) as u8], &base[4..]]),
            "x509: malformed certificate",
        ),
        (
            "five byte length",
            cat(&[
                &[
                    0x30,
                    0x85,
                    0x00,
                    0x00,
                    0x00,
                    ((base.len() - 6) >> 8) as u8,
                    (base.len() - 6) as u8,
                ],
                &base[6..],
            ]),
            "x509: malformed certificate",
        ),
        (
            "high tag number",
            cat(&[&[0x1f, 0x1e], &base[2..]]),
            "x509: malformed certificate",
        ),
    ];
    for (name, der, expected) in corrupted {
        let error = x509::validate_certificate(&der)
            .err()
            .unwrap_or_else(|| panic!("{name}: accepted"));
        assert!(
            error.ends_with(expected) || error == expected,
            "{name}: {error}"
        );
    }
}

// --- x509 gate: the remaining structural branches ---

/// The fixture certificate's uncompressed P-256 point (the bytes after the
/// SPKI's `03 42 00` prefix), reused to build a structurally valid SPKI for
/// the manual-TBS rows below.
fn ec_public_key_bits() -> Vec<u8> {
    let point = cert_a_der();
    let start = point
        .windows(3)
        .position(|window| window == [0x03, 0x42, 0x00])
        .unwrap();
    point[start + 3..start + 3 + 65].to_vec()
}

fn validity_ok() -> Vec<u8> {
    seq(&[&utc_time("250101000000Z"), &utc_time("260101000000Z")])
}

fn spki_ok() -> Vec<u8> {
    let point = ec_public_key_bits();
    seq(&[&alg_ec(), &bit_string(&cat(&[&[0x00], &point]))])
}

fn base_tbs_parts() -> Vec<Vec<u8>> {
    vec![
        v3(),
        serial1(),
        alg_sha(),
        name_cn(b"probe issuer"),
        validity_ok(),
        name_cn(b"probe subject"),
        spki_ok(),
    ]
}

fn cert_of_parts(parts: &[Vec<u8>], signature: &[u8]) -> Vec<u8> {
    let refs: Vec<&[u8]> = parts.iter().map(|part| part.as_slice()).collect();
    let tbs = seq(&refs);
    let alg = alg_sha();
    seq(&[&tbs, &alg, signature])
}

fn default_sig() -> Vec<u8> {
    bit_string(&[0x00, 0xde, 0xad])
}

/// Every remaining reachable arm of the DER gate, one mutation per row. The
/// oracle is Go's crypto/x509 parser as pinned by the probe battery; the
/// expected detail is the bare error text `validate_certificate` returns.
#[test]
fn x509_gate_rejects_every_remaining_structural_branch() {
    let malformed = Some("x509: malformed certificate");
    let mut rows: Vec<(&str, Vec<u8>, Option<&str>)> = vec![
        (
            "outer tbs not a sequence",
            seq(&[&set(&[]), &alg_sha(), &default_sig()]),
            malformed,
        ),
        (
            "outer inner-alg not a sequence",
            seq(&[&seq(&[]), &set(&[]), &default_sig()]),
            malformed,
        ),
        (
            "outer signature not a bit string",
            seq(&[&seq(&[]), &alg_sha(), &octet_string(&[])]),
            malformed,
        ),
        (
            "outer trailing element",
            seq(&[&seq(&[]), &alg_sha(), &default_sig(), &integer(&[0x01])]),
            Some("x509: trailing data"),
        ),
    ];
    let push = |name: &'static str,
                parts: Vec<Vec<u8>>,
                signature: Option<Vec<u8>>,
                expected: Option<&'static str>,
                rows: &mut Vec<(&'static str, Vec<u8>, Option<&'static str>)>| {
        let signature = signature.unwrap_or_else(default_sig);
        rows.push((name, cert_of_parts(&parts, &signature), expected));
    };

    let mut parts = base_tbs_parts();
    parts[0] = tlv(0xa0, &octet_string(&[]));
    push(
        "version value not an integer",
        parts,
        None,
        Some("x509: malformed version"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[2] = raw_string(0x04, b"x");
    push(
        "tbs algorithm not a sequence",
        parts,
        None,
        malformed,
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[4] = raw_string(0x04, b"x");
    push("validity not a sequence", parts, None, malformed, &mut rows);

    let mut parts = base_tbs_parts();
    parts[5] = raw_string(0x04, b"x");
    push(
        "subject not a sequence",
        parts,
        None,
        Some("x509: malformed subject"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[6] = octet_string(&[]);
    push(
        "spki not a sequence",
        parts,
        None,
        Some("x509: malformed subjectPublicKey"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[6] = seq(&[&octet_string(&[]), &bit_string(&[0x00])]);
    push(
        "spki algorithm not a sequence",
        parts,
        None,
        Some("x509: malformed subjectPublicKey"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[6] = seq(&[&alg_ec(), &octet_string(&[])]);
    push(
        "spki key not a bit string",
        parts,
        None,
        Some("x509: malformed subjectPublicKey"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts.push(integer(&[0x01]));
    push(
        "tbs trailing element",
        parts,
        None,
        Some("x509: trailing data"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[4] = seq(&[
        &utc_time("250101000000Z"),
        &utc_time("260101000000Z"),
        &utc_time("270101000000Z"),
    ]);
    push(
        "validity trailing element",
        parts,
        None,
        Some("x509: trailing data"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[4] = seq(&[&utc_time("2501"), &utc_time("260101000000Z")]);
    push(
        "utc wrong length",
        parts,
        None,
        Some("x509: malformed UTCTime"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[4] = seq(&[&utc_time("25a101000000Z"), &utc_time("260101000000Z")]);
    push(
        "utc non-digit",
        parts,
        None,
        Some("x509: malformed UTCTime"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[4] = seq(&[&gen_time("2025a101000000Z"), &utc_time("260101000000Z")]);
    push(
        "generalized time non-digit",
        parts,
        None,
        Some("x509: malformed GeneralizedTime"),
        &mut rows,
    );

    for (name, zone) in [
        ("utc zone non-digit", "250101000000+0a00"),
        ("utc zone wrong shape", "250101000000+07"),
        ("utc 31st of a 30-day month", "250431000000Z"),
    ] {
        let mut parts = base_tbs_parts();
        parts[4] = seq(&[&utc_time(zone), &utc_time("260101000000Z")]);
        push(
            name,
            parts,
            None,
            Some("x509: malformed UTCTime"),
            &mut rows,
        );
    }

    let mut parts = base_tbs_parts();
    parts[2] = seq(&[&octet_string(&[]), &tlv(0x05, &[])]);
    push(
        "algorithm identifier type not an oid",
        parts,
        None,
        malformed,
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[3] = seq(&[&set(&[&octet_string(&[])])]);
    push(
        "rdn set child not a sequence",
        parts,
        None,
        Some("x509: invalid RDNSequence"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[3] = seq(&[&set(&[&seq(&[&octet_string(&[])])])]);
    push(
        "attribute type not an oid",
        parts,
        None,
        Some("x509: invalid RDNSequence: invalid attribute type"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[5] = seq(&[&set(&[&seq(&[
        &oid(&[0x55, 0x04, 0x03]),
        &raw_string(0x16, &[0xff]),
    ])])]);
    push(
        "ia5 string out of range",
        parts,
        None,
        Some("x509: invalid RDNSequence: invalid attribute value: invalid IA5String"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts.push(tlv(
        0xa3,
        &cat(&[&seq(&[&ext_entry(OID_BC, &seq(&[]))]), &integer(&[0x01])]),
    ));
    push(
        "extensions wrapper trailing element",
        parts,
        None,
        Some("x509: malformed extensions"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts.push(extensions_of(&[&seq(&[
        &octet_string(&[]),
        &octet_string(&[]),
    ])]));
    push(
        "extension oid field not an oid",
        parts,
        None,
        Some("x509: malformed extension"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts.push(extensions_of(&[&ext_entry(OID_SAN, &octet_string(&[]))]));
    push(
        "san value not a sequence",
        parts,
        None,
        Some("x509: invalid subject alternative names"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts.push(extensions_of(&[&ext_entry(
        OID_SAN,
        &seq(&[&[0x02, 0x05, 0x01]]),
    )]));
    push(
        "san entry truncated",
        parts,
        None,
        Some("x509: invalid subject alternative names"),
        &mut rows,
    );

    // Signature BIT STRING envelope arms.
    let parts = base_tbs_parts();
    rows.push((
        "signature bit string empty",
        cert_of_parts(&parts, &bit_string(&[])),
        Some("x509: malformed signature"),
    ));
    rows.push((
        "signature pad with no data",
        cert_of_parts(&parts, &bit_string(&[0x01])),
        Some("x509: malformed signature"),
    ));
    rows.push((
        "signature pad with zeroed padding bits",
        cert_of_parts(&parts, &bit_string(&[0x01, 0x02])),
        None,
    ));

    // OID arms: a non-minimal leading 0x80, a first arc below 40, and an
    // empty body.
    let mut parts = base_tbs_parts();
    parts[2] = seq(&[&oid(&[0x80, 0x2a]), &tlv(0x05, &[])]);
    push("oid non-minimal lead", parts, None, malformed, &mut rows);

    let mut parts = base_tbs_parts();
    parts[2] = seq(&[&oid(&[0x05])]);
    push(
        "oid first arc below 40",
        parts,
        None,
        Some("x509: inner and outer signature algorithm identifiers don't match"),
        &mut rows,
    );

    let mut parts = base_tbs_parts();
    parts[2] = seq(&[&oid(&[])]);
    push("oid empty", parts, None, malformed, &mut rows);

    let mut mismatches = Vec::new();
    for (name, der, expected) in &rows {
        let outcome = x509::validate_certificate(der).err();
        match (outcome.as_deref(), *expected) {
            (None, None) => {}
            (Some(actual), Some(want)) if actual == want => {}
            (actual, want) => {
                mismatches.push(format!("{name}: got {actual:?}, want {want:?}"));
            }
        }
    }
    assert!(
        mismatches.is_empty(),
        "structural branch mismatches:\n{}",
        mismatches.join("\n")
    );
}

// --- installer (injected tempdir roots) ---

struct Trust {
    #[allow(dead_code)]
    root: tempfile::TempDir,
    bundle_path: std::path::PathBuf,
    cert_dir: std::path::PathBuf,
    state_path: std::path::PathBuf,
}

fn new_trust() -> Trust {
    let root = tempfile::tempdir().unwrap();
    let bundle_path = root.path().join("certs/ca-certificates.crt");
    let cert_dir = root.path().join("local");
    let state_path = root.path().join("certs/state.json");
    std::fs::create_dir_all(bundle_path.parent().unwrap()).unwrap();
    std::fs::create_dir_all(&cert_dir).unwrap();
    // System bundle: one unrelated root with a leading comment line.
    let mut system = String::from("# comment kept\n");
    system.push_str(CERT_C_PEM);
    std::fs::write(&bundle_path, &system).unwrap();
    std::fs::write(cert_dir.join("user.crt"), CERT_A_PEM).unwrap();
    Trust {
        root,
        bundle_path,
        cert_dir,
        state_path,
    }
}

fn installer_for(trust: &Trust) -> CaCertInstaller {
    CaCertInstaller::builder()
        .state_path(trust.state_path.clone())
        .bundle_path(trust.bundle_path.clone())
        .cert_dir(trust.cert_dir.clone())
        .tick(Duration::from_millis(10))
        .build()
}

async fn install(trust: &Trust, pems: &[&str]) {
    // The oracle's helper joins the set and runs one update (one Parse, one
    // updateBundle), then persist synchronously.
    let installer = installer_for(trust);
    installer.install(&pems.concat()).await.unwrap();
    installer.persist().await.unwrap();
}

fn bundle_text(trust: &Trust) -> String {
    std::fs::read_to_string(&trust.bundle_path).unwrap()
}

fn mode_of(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test]
async fn install_appends_publishes_and_persists_intent_first() {
    let trust = new_trust();
    let before = bundle_text(&trust);
    let installer = installer_for(&trust);
    installer.install(CERT_A_PEM).await.unwrap();
    // Intent-first: the state file exists with the exact 0600 JSON shape
    // before /init could have been told anything else.
    let state_text = std::fs::read_to_string(&trust.state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_text).unwrap();
    let desired_pem = state["desiredPEM"].as_str().unwrap();
    assert_eq!(desired_pem, CERT_A_PEM);
    assert_eq!(state["owned"], serde_json::json!([CERT_A_FP]));
    assert_eq!(mode_of(&trust.state_path), 0o600);
    // Foreground publish: the aggregate is usable before persist runs.
    let after_install = bundle_text(&trust);
    assert_eq!(after_install, format!("{before}\n{CERT_A_PEM}"));
    installer.persist().await.unwrap();
    // Reconcile converges: the non-PEM comment prefix is dropped by the
    // pem.Decode walk (probed against the pinned Go oracle), owned A is
    // re-appended canonically after the untouched system root.
    let text = bundle_text(&trust);
    assert_eq!(text, format!("{CERT_C_PEM}{CERT_A_PEM}"));
    // Source files: e2b-<fp>.crt at 0644; the user's own file untouched.
    let source = trust.cert_dir.join(format!("e2b-{CERT_A_FP}.crt"));
    assert_eq!(std::fs::read_to_string(&source).unwrap(), CERT_A_PEM);
    assert_eq!(mode_of(&source), 0o644);
    assert_eq!(
        std::fs::read_to_string(trust.cert_dir.join("user.crt")).unwrap(),
        CERT_A_PEM
    );
    // State ownership survives; no temp residue beside managed files.
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&trust.state_path).unwrap()).unwrap();
    assert_eq!(state["owned"], serde_json::json!([CERT_A_FP]));
    let residues: Vec<_> = std::fs::read_dir(trust.bundle_path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(residues.is_empty(), "{residues:?}");
}

#[tokio::test]
async fn unchanged_bundle_skips_io() {
    let trust = new_trust();
    // One installer, like the single envd process: the no-op gate is
    // `installed && Equal`, which only holds within a warm instance.
    let installer = installer_for(&trust);
    installer.install(CERT_A_PEM).await.unwrap();
    installer.persist().await.unwrap();
    let before = std::fs::metadata(&trust.bundle_path).unwrap().ino();
    // A reformatted copy of the same identity is a no-op.
    let reformatted = format!("\r\n\r\n{}", CERT_A_PEM.replace('\n', "\r\n"));
    installer.install(&reformatted).await.unwrap();
    installer.persist().await.unwrap();
    let after = std::fs::metadata(&trust.bundle_path).unwrap().ino();
    assert_eq!(before, after, "bundle was replaced");
}

#[tokio::test]
async fn overlapping_sets_follow_the_oracle_lifecycle() {
    let trust = new_trust();
    // worker -> +r0 -> same set reordered -> retire r0 -> replace, each step
    // asserting the aggregate holds exactly the desired set plus unrelated
    // system trust and the source directory tracks only managed desire.
    let known: &[(&str, &str)] = &[(CERT_A_PEM, CERT_A_FP), (CERT_B_PEM, CERT_B_FP)];
    let steps: Vec<Vec<&str>> = vec![
        vec![CERT_A_PEM],
        vec![CERT_A_PEM, CERT_B_PEM],
        vec![CERT_B_PEM, CERT_A_PEM],
        vec![CERT_B_PEM],
        vec![CERT_A_PEM],
    ];
    for desired in &steps {
        install(&trust, desired).await;
        let text = bundle_text(&trust);
        for pem in desired {
            assert!(text.contains(pem.trim_end()), "missing {pem}");
        }
        for (pem, fp) in known {
            let wanted = desired.contains(pem);
            let source = trust.cert_dir.join(format!("e2b-{fp}.crt"));
            assert_eq!(
                source.exists(),
                wanted,
                "source file for {fp} wrong after {desired:?}"
            );
        }
        // C is unrelated system trust: never installed as a source file.
        assert!(!trust.cert_dir.join(format!("e2b-{CERT_C_FP}.crt")).exists());
    }
    // Final state: owned exactly the desired fingerprints.
    let state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&trust.state_path).unwrap()).unwrap();
    assert_eq!(state["owned"], serde_json::json!([CERT_A_FP]));
}

#[tokio::test]
async fn install_repairs_interrupted_appends() {
    for restart in [false, true] {
        let trust = new_trust();
        install(&trust, &[CERT_A_PEM]).await;
        let before = bundle_text(&trust);
        // Simulate an interrupted older append: bundle file with a partial B.
        std::fs::remove_file(&trust.bundle_path).unwrap();
        let broken_pem = &CERT_B_PEM[..CERT_B_PEM.len() / 2];
        std::fs::write(&trust.bundle_path, format!("{before}{broken_pem}")).unwrap();
        let installer = installer_for(&trust);
        installer.install(CERT_B_PEM).await.unwrap();
        installer.persist().await.unwrap();
        if restart {
            // A fresh process converges purely from the persisted intent.
            let fresh = installer_for(&trust);
            fresh.persist().await.unwrap();
        }
        // No background cleanup needed to make init's trust usable: the
        // repair path already reconciled (owned A retired, B published).
        assert_eq!(
            bundle_text(&trust),
            format!("{CERT_C_PEM}{CERT_B_PEM}"),
            "restart={restart}"
        );
    }
}

#[tokio::test]
async fn restart_finishes_persisted_intent_without_init() {
    for boundary in ["before-append", "partial-append", "after-cleanup"] {
        let trust = new_trust();
        install(&trust, &[CERT_A_PEM]).await;
        let before = bundle_text(&trust);
        let pre_state = std::fs::read_to_string(&trust.state_path).unwrap();
        let installer = installer_for(&trust);
        installer.install(CERT_B_PEM).await.unwrap();
        let state_after_install = std::fs::read_to_string(&trust.state_path).unwrap();
        match boundary {
            "before-append" => std::fs::write(&trust.bundle_path, &before).unwrap(),
            "partial-append" => std::fs::write(
                &trust.bundle_path,
                format!("{before}{}", &CERT_B_PEM[..CERT_B_PEM.len() / 2]),
            )
            .unwrap(),
            "after-cleanup" => {
                installer.persist().await.unwrap();
                std::fs::write(&trust.state_path, &state_after_install).unwrap();
            }
            _ => unreachable!(),
        }
        let _ = pre_state;
        let fresh = installer_for(&trust);
        fresh.persist().await.unwrap();
        let text = bundle_text(&trust);
        assert!(text.contains(CERT_B_PEM.trim_end()), "{boundary}");
        assert!(text.contains(CERT_C_PEM.trim_end()), "{boundary}");
        // A was installed before the crash: owned, so retired by B in every
        // boundary, including the post-cleanup replay.
        assert!(
            !text.contains(&CERT_A_PEM[..CERT_A_PEM.len() / 3]),
            "{boundary}"
        );
        let state: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&trust.state_path).unwrap()).unwrap();
        assert_eq!(state["owned"], serde_json::json!([CERT_B_FP]), "{boundary}");
    }
}

#[tokio::test]
async fn persistence_failure_retains_ownership_until_cleanup_succeeds() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    // Break the cert dir: persist must fail AFTER the trust publish.
    std::fs::remove_dir_all(&trust.cert_dir).unwrap();
    std::os::unix::fs::symlink(trust.cert_dir.with_extension("missing"), &trust.cert_dir).unwrap();
    let installer = installer_for(&trust);
    installer.install(CERT_B_PEM).await.unwrap();
    // Intent-first: install has recorded desired B with both fingerprints
    // still owned; the failed persist must leave that intent exactly as
    // written (no partial retirement of ownership).
    let intent = std::fs::read_to_string(&trust.state_path).unwrap();
    let state: serde_json::Value = serde_json::from_str(&intent).unwrap();
    assert_eq!(state["desiredPEM"], serde_json::json!(CERT_B_PEM));
    assert_eq!(state["owned"], serde_json::json!([CERT_A_FP, CERT_B_FP]));
    assert!(installer.persist().await.is_err());
    // Trust is live (r0 published) and ownership retained.
    assert!(bundle_text(&trust).contains(CERT_B_PEM.trim_end()));
    assert_eq!(
        std::fs::read_to_string(&trust.state_path).unwrap(),
        intent,
        "retain ownership until source cleanup succeeds"
    );
    // Heal and let a restart converge.
    std::fs::remove_file(&trust.cert_dir).unwrap();
    std::fs::create_dir_all(&trust.cert_dir).unwrap();
    let fresh = installer_for(&trust);
    fresh.persist().await.unwrap();
    let text = bundle_text(&trust);
    assert!(text.contains(CERT_B_PEM.trim_end()));
    assert!(!trust.cert_dir.join(format!("e2b-{CERT_A_FP}.crt")).exists());
    assert!(trust.cert_dir.join(format!("e2b-{CERT_B_FP}.crt")).exists());
}

#[tokio::test]
async fn ownership_failure_does_not_publish() {
    let trust = new_trust();
    let before = bundle_text(&trust);
    // state.json is a directory: reading it fails inside loadState.
    std::fs::create_dir(&trust.state_path).unwrap();
    let installer = installer_for(&trust);
    let error = installer.install(CERT_A_PEM).await.unwrap_err();
    assert!(error.to_string().contains("read CA state"), "{error}");
    assert_eq!(bundle_text(&trust), before);
}

#[tokio::test]
async fn legacy_cert_removed_only_when_not_requested() {
    let trust = new_trust();
    std::fs::write(trust.cert_dir.join("e2b-ca.crt"), CERT_A_PEM).unwrap();
    // Hash link chain resolving through the legacy source filename.
    let pem_link = trust.cert_dir.join("e2b-ca.pem");
    std::os::unix::fs::symlink(trust.cert_dir.join("e2b-ca.crt"), &pem_link).unwrap();
    let hash_link = trust.cert_dir.join("subject-hash.0");
    std::os::unix::fs::symlink("e2b-ca.pem", &hash_link).unwrap();

    install(&trust, &[CERT_A_PEM, CERT_B_PEM]).await;
    // Legacy file equals a requested identity: kept so links still resolve.
    assert_eq!(
        std::fs::read(&hash_link).unwrap(),
        CERT_A_PEM.as_bytes(),
        "existing OpenSSL hash links still resolve through the legacy name"
    );
    assert!(trust.cert_dir.join("e2b-ca.crt").exists());
    // Once A retires the legacy alias goes with it.
    install(&trust, &[CERT_B_PEM]).await;
    assert!(!trust.cert_dir.join("e2b-ca.crt").exists());
}

#[tokio::test]
async fn legacy_alias_with_unpadded_body_is_not_kept() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    let legacy = trust.cert_dir.join("e2b-ca.crt");
    std::fs::write(
        &legacy,
        "-----BEGIN CERTIFICATE-----\nMQU\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    // removeLegacyCert parses before Acquire: an unparseable alias is never
    // "the latest request", so the reconcile removes it.
    installer_for(&trust).persist().await.unwrap();
    assert!(!legacy.exists());
}

#[tokio::test]
async fn state_owned_null_round_trips_like_go_json_nil() {
    for seed in [
        r#"{"desiredPEM":"","owned":null}"#,
        r#"{"desiredPEM":"","owned":[]}"#,
    ] {
        let trust = new_trust();
        std::fs::write(&trust.state_path, seed).unwrap();
        installer_for(&trust).persist().await.unwrap();
        let text = std::fs::read_to_string(&trust.state_path).unwrap();
        // Go json.Marshal: persist's nextOwned starts nil (cacerts.go:189-193),
        // so even a crafted [] normalizes to null on the rewrite.
        assert_eq!(text, r#"{"desiredPEM":"","owned":null}"#, "seed {seed}");
    }
}

#[tokio::test]
async fn state_with_unpadded_desired_pem_fails_the_parse() {
    let trust = new_trust();
    std::fs::write(
        &trust.state_path,
        r#"{"desiredPEM":"-----BEGIN CERTIFICATE-----\nMQU\n-----END CERTIFICATE-----\n","owned":[]}"#,
    )
    .unwrap();
    // loadState re-parses the recorded intent with the same PEM gate
    // (cacerts.go:255-258): the unpadded body is a decode error there too.
    let error = installer_for(&trust).install(CERT_A_PEM).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "parse CA state: malformed certificate PEM"
    );
}

#[tokio::test]
async fn managed_dir_entries_are_pruned_and_foreign_ones_kept() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    // Foreign names stay; a stale managed name goes.
    // stale managed name (64-hex fingerprint, lowercase, matching the oracle
    // regex ^e2b-([0-9a-f]{64})\.crt$) goes.
    let stale = trust.cert_dir.join(format!("e2b-{}c.crt", "0".repeat(63)));
    std::fs::write(&stale, "stale").unwrap();
    install(&trust, &[CERT_B_PEM]).await;
    assert!(trust.cert_dir.join("user.crt").exists());
    assert!(!stale.exists());
    assert!(!trust.cert_dir.join(format!("e2b-{CERT_A_FP}.crt")).exists());
    assert!(trust.cert_dir.join(format!("e2b-{CERT_B_FP}.crt")).exists());
}

#[tokio::test]
async fn cancelled_context_fails_even_when_the_slot_is_free() {
    let trust = new_trust();
    let installer = installer_for(&trust);
    installer.install(CERT_A_PEM).await.unwrap();
    // Hold the installer's semaphore, like a wedged reconcile would.
    let guard = installer.hold_lock_for_tests().await;
    let cancelled: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(std::future::ready(()));
    let error = installer
        .install_cancellable(CERT_B_PEM, cancelled)
        .await
        .unwrap_err();
    assert!(matches!(error, CaError::InProgress), "{error:?}");
    assert_eq!(
        error.to_string(),
        "CA install already in progress: context canceled"
    );
    assert_eq!(error.init_status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(guard);
    // x/sync semaphore.go:66-73: Acquire checks ctx.Err() before the fast
    // path, so a done context fails even with the slot immediately free.
    let cancelled: std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> =
        Box::pin(std::future::ready(()));
    let error = installer
        .install_cancellable(CERT_B_PEM, cancelled)
        .await
        .unwrap_err();
    assert!(matches!(error, CaError::InProgress), "{error:?}");
    assert_eq!(
        CaError::Failed("read CA state: boom".into()).init_status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test(start_paused = true)]
async fn run_loop_finishes_persisted_intent_without_init() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    let before = bundle_text(&trust);
    let installer = installer_for(&trust);
    installer.install(CERT_B_PEM).await.unwrap();
    // Crash between ownership and the bundle write: the aggregate still holds
    // only A while intent says B.
    std::fs::write(&trust.bundle_path, before).unwrap();
    let worker = installer_for(&trust);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move { worker.run(stop_rx).await });
    for _ in 0..50 {
        tokio::time::advance(Duration::from_millis(50)).await;
        if bundle_text(&trust).contains(CERT_B_PEM.trim_end()) {
            break;
        }
    }
    stop_tx.send(true).unwrap();
    task.await.unwrap();
    let text = bundle_text(&trust);
    assert!(text.contains(CERT_B_PEM.trim_end()));
    assert!(text.contains(CERT_C_PEM.trim_end()));
    assert!(!text.contains(&CERT_A_PEM[..CERT_A_PEM.len() / 3]));
}

// --- installer error paths ---

#[tokio::test]
async fn empty_bundle_is_a_noop_and_a_wedged_slot_honours_cancellation() {
    let trust = new_trust();
    let installer = installer_for(&trust);
    let before = bundle_text(&trust);
    installer.install("").await.unwrap();
    assert!(!trust.state_path.exists());
    assert_eq!(bundle_text(&trust), before);
    // A cancellation that fires while the slot is held must win the select
    // even though the try-lock fast path already failed.
    let guard = installer.hold_lock_for_tests().await;
    let cancel = tokio::time::sleep(Duration::from_millis(5));
    let error = installer
        .install_cancellable(CERT_A_PEM, cancel)
        .await
        .unwrap_err();
    assert!(matches!(error, CaError::InProgress), "{error:?}");
    drop(guard);
}

#[tokio::test]
async fn state_publish_rename_failure_reports_the_go_path_error() {
    let trust = new_trust();
    let installer = installer_for(&trust);
    installer.install(CERT_A_PEM).await.unwrap();
    // Replace the published state with a directory: the staging file itself
    // is written, but the rename cannot replace a directory (EISDIR).
    std::fs::remove_file(&trust.state_path).unwrap();
    std::fs::create_dir(&trust.state_path).unwrap();
    let error = installer.install(CERT_B_PEM).await.unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("publish CA state: rename "), "{text}");
    assert!(text.ends_with("state.json: is a directory"), "{text}");
    // The failed staging file was removed; no .tmp residue is left behind.
    let residues: Vec<_> = std::fs::read_dir(trust.state_path.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".tmp"))
        .collect();
    assert!(residues.is_empty(), "{residues:?}");
}

#[tokio::test]
async fn bundle_cleanup_read_failure_is_reported_with_the_go_path_error() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    // The persisted intent makes a fresh reconcile pending; reading the
    // aggregate for cleanup then fails on the directory.
    std::fs::remove_file(&trust.bundle_path).unwrap();
    std::fs::create_dir(&trust.bundle_path).unwrap();
    let error = installer_for(&trust).persist().await.unwrap_err();
    let text = error.to_string();
    assert!(
        text.starts_with("read CA bundle for cleanup: open "),
        "{text}"
    );
    assert!(text.contains("is a directory"), "{text}");
}

#[tokio::test]
async fn a_stale_managed_entry_that_cannot_be_unlinked_fails_the_reconcile() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    // A managed name (64 lowercase hex) that is a non-empty directory: the
    // removal is a real error, not the NotFound a concurrent prune produces.
    let stale = trust.cert_dir.join(format!("e2b-{}d.crt", "0".repeat(63)));
    std::fs::create_dir(&stale).unwrap();
    std::fs::write(stale.join("child"), b"x").unwrap();
    let error = installer_for(&trust).persist().await.unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("remove stale CA cert: remove "), "{text}");
    assert!(text.contains("is a directory"), "{text}");
    std::fs::remove_dir_all(&stale).unwrap();
}

#[tokio::test]
async fn legacy_alias_read_failure_is_reported_with_the_go_path_error() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    let legacy = trust.cert_dir.join("e2b-ca.crt");
    std::fs::create_dir(&legacy).unwrap();
    std::fs::write(legacy.join("child"), b"x").unwrap();
    let error = installer_for(&trust).persist().await.unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("read legacy CA cert: open "), "{text}");
    assert!(text.contains("is a directory"), "{text}");
    std::fs::remove_dir_all(&legacy).unwrap();
}

#[tokio::test]
async fn legacy_alias_unlink_failure_is_reported_with_the_go_path_error() {
    let trust = new_trust();
    // Empty intent: writeCertFiles writes nothing, so the read-only cert dir
    // only bites the legacy unlink.
    std::fs::write(&trust.state_path, r#"{"desiredPEM":"","owned":[]}"#).unwrap();
    std::fs::write(trust.cert_dir.join("e2b-ca.crt"), CERT_A_PEM).unwrap();
    std::fs::set_permissions(&trust.cert_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    let error = installer_for(&trust).persist().await.unwrap_err();
    std::fs::set_permissions(&trust.cert_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let text = error.to_string();
    assert!(text.starts_with("remove legacy CA cert: remove "), "{text}");
    assert!(text.ends_with("permission denied"), "{text}");
}

#[tokio::test]
async fn cert_source_rename_failure_is_reported_as_a_persist_error() {
    let trust = new_trust();
    install(&trust, &[CERT_A_PEM]).await;
    let source = trust.cert_dir.join(format!("e2b-{CERT_A_FP}.crt"));
    std::fs::remove_file(&source).unwrap();
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("child"), b"x").unwrap();
    let installer = installer_for(&trust);
    installer.install(CERT_A_PEM).await.unwrap();
    let error = installer.persist().await.unwrap_err();
    let text = error.to_string();
    assert!(text.starts_with("persist CA cert: rename "), "{text}");
    assert!(text.contains("is a directory"), "{text}");
    std::fs::remove_dir_all(&source).unwrap();
}

#[tokio::test]
async fn run_loop_logs_its_failure_and_keeps_polling_until_stopped() {
    let trust = new_trust();
    // A directory where the state file belongs makes every persist fail.
    std::fs::create_dir(&trust.state_path).unwrap();
    let installer = installer_for(&trust);
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let task = tokio::spawn(async move { installer.run(stop_rx).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    // changed() with a false value keeps the loop alive; true returns.
    stop_tx.send(false).unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    stop_tx.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("run loop did not stop")
        .unwrap();
}

// --- daemon integration ---

use coorworker_envd::daemon::{app, Daemon};

async fn init_response(router: axum::Router, body: &str) -> axum::http::Response<Body> {
    router
        .oneshot(
            Request::post("/init")
                .body(Body::from(body.to_owned()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body_text(response: axum::http::Response<Body>) -> String {
    String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec()).unwrap()
}

fn ca_daemon(trust: &Trust) -> Arc<Daemon> {
    Arc::new(Daemon::new().with_ca_certinstaller(Arc::new(installer_for(trust))))
}

async fn init_on(daemon: &Arc<Daemon>, body: &str) -> axum::http::Response<Body> {
    init_response(app(daemon.clone()), body).await
}

#[tokio::test]
async fn init_with_cabundle_installs_into_injected_roots() {
    let trust = new_trust();
    let daemon = ca_daemon(&trust);
    let body = format!(
        r#"{{"caBundle":{}}}"#,
        serde_json::to_string(CERT_A_PEM).unwrap()
    );
    let response = init_on(&daemon, &body).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(bundle_text(&trust).contains(CERT_A_PEM.trim_end()));
    assert!(trust.state_path.exists());
    // Source files land in persist (the run loop's job in Go's daemon); a
    // fresh installer converges from the persisted intent like a restart.
    installer_for(&trust).persist().await.unwrap();
    assert!(trust.cert_dir.join(format!("e2b-{CERT_A_FP}.crt")).exists());
    // A second identical init on the same daemon is a no-op that still
    // answers 204.
    let response = init_on(&daemon, &body).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn malformed_cabundle_fails_init_without_touching_trust() {
    let trust = new_trust();
    let before = bundle_text(&trust);
    let daemon = ca_daemon(&trust);
    let response = init_on(&daemon, r#"{"caBundle":"junk"}"#).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // init.go:746: the body is the bare err.Error() text, no JSON envelope.
    assert_eq!(
        body_text(response).await,
        "parse CA bundle: malformed certificate PEM"
    );
    assert_eq!(bundle_text(&trust), before);
    assert!(!trust.state_path.exists());
    assert!(trust.cert_dir.join("user.crt").exists());
}

#[tokio::test]
async fn unpadded_cabundle_body_fails_init_with_the_bare_parse_error() {
    let trust = new_trust();
    let daemon = ca_daemon(&trust);
    let response = init_on(
        &daemon,
        r#"{"caBundle":"-----BEGIN CERTIFICATE-----\nMQU\n-----END CERTIFICATE-----\n"}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_text(response).await,
        "parse CA bundle: malformed certificate PEM"
    );
    assert!(!trust.state_path.exists());
}

#[tokio::test]
async fn missing_bundle_file_surfaces_the_go_path_error() {
    let trust = new_trust();
    std::fs::remove_file(&trust.bundle_path).unwrap();
    let daemon = ca_daemon(&trust);
    let body = format!(
        r#"{{"caBundle":{}}}"#,
        serde_json::to_string(CERT_A_PEM).unwrap()
    );
    let response = init_on(&daemon, &body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // Go updateBundle:125-127 wraps the os.ReadFile PathError verbatim:
    // "open <path>: no such file or directory" (probe-cased).
    assert_eq!(
        body_text(response).await,
        format!(
            "read CA bundle: open {}: no such file or directory",
            trust.bundle_path.display()
        )
    );
}

#[tokio::test]
async fn empty_null_and_absent_cabundle_are_noops() {
    for body in [r#"{"caBundle":""}"#, r#"{"caBundle":null}"#, "{}"] {
        let trust = new_trust();
        let daemon = ca_daemon(&trust);
        let response = init_on(&daemon, body).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT, "{body}");
        assert!(!trust.state_path.exists(), "{body}");
        assert!(trust.cert_dir.join("user.crt").exists(), "{body}");
    }
}

#[tokio::test]
async fn non_string_cabundle_rejects_the_init() {
    let trust = new_trust();
    let daemon = ca_daemon(&trust);
    let response = init_on(&daemon, r#"{"caBundle":42}"#).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn stale_timestamp_skips_the_cabundle_install() {
    let trust = new_trust();
    let daemon = ca_daemon(&trust);
    // Install A under timestamp T1.
    let now = coorworker_envd::clock::real_now_nanos();
    let body = format!(
        r#"{{"timestamp":{},"caBundle":{}}}"#,
        serde_json::to_string(&rfc3339(now)).unwrap(),
        serde_json::to_string(CERT_A_PEM).unwrap()
    );
    assert_eq!(
        init_on(&daemon, &body).await.status(),
        StatusCode::NO_CONTENT
    );
    // A replay with an older timestamp is gated before SetData: no install.
    let body = format!(
        r#"{{"timestamp":{},"caBundle":{}}}"#,
        serde_json::to_string(&rfc3339(now - 60_000_000_000)).unwrap(),
        serde_json::to_string(CERT_B_PEM).unwrap()
    );
    assert_eq!(
        init_on(&daemon, &body).await.status(),
        StatusCode::NO_CONTENT
    );
    let text = bundle_text(&trust);
    assert!(text.contains(CERT_A_PEM.trim_end()));
    assert!(!text.contains(CERT_B_PEM.trim_end()));
    assert!(!trust.cert_dir.join(format!("e2b-{CERT_B_FP}.crt")).exists());
}

fn rfc3339(nanos: i128) -> String {
    let secs = nanos.div_euclid(1_000_000_000);
    let sub = nanos.rem_euclid(1_000_000_000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days as i64);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{sub:09}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[tokio::test]
async fn ca_error_keeps_the_correction_header() {
    let trust = new_trust();
    let daemon = ca_daemon(&trust);
    let body = format!(
        r#"{{"timestamp":{},"caBundle":"junk"}}"#,
        serde_json::to_string(&rfc3339(coorworker_envd::clock::real_now_nanos())).unwrap()
    );
    let response = init_on(&daemon, &body).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    // Go sets the clock header before the gate; writeInitError does not clear
    // it, so the failed init still answers it. (One request: a replay would
    // be gated by the newest-wins timestamp and skip SetData entirely.)
    assert!(
        response
            .headers()
            .get("x-envd-init-clock-correction")
            .is_some(),
        "the clock header set before the failure survives writeInitError"
    );
    // init.go:746: bare err.Error() body.
    assert_eq!(
        body_text(response).await,
        "parse CA bundle: malformed certificate PEM"
    );
}
