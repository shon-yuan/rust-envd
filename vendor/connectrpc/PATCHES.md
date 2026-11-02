# Vendored connectrpc fork

This directory is a fork of the crates.io crate **`connectrpc` 0.9.1**
(upstream repository `connectrpc/connect-rust`, crate VCS revision
`7fd4a8248df90a6f5403c7066df0e44d4164d67f`), copied from the registry and
patched in-tree. The published crate declares and ships **Apache-2.0**; see
`LICENSE` and `Cargo.toml.orig` (whose `license.workspace = true` resolves to
the published expression above).

The workspace selects it with `[patch.crates-io]` in the root `Cargo.toml`:

```toml
[patch.crates-io]
connectrpc = { path = "vendor/connectrpc" }
```

Only `crates/envd` uses the crate (server side). The fork exists solely for
wire-parity with the pinned oracle, official Go envd 0.10.2 (commit
`51083a83cc0903185d992725ea6bb33683814b29`), whose RPC layer is connect-go
1.19.2. Every change below makes a malformed-request error text or frame
byte-identical to that oracle; no API surface used by callers changed beyond
trait bounds.

## Changed files

| File | Change |
| --- | --- |
| `src/lib.rs` | Declares `mod go_json` under the `json` feature. |
| `src/go_json.rs` | **New.** Go `protojson` syntax-error scanner: reports `proto: unexpected EOF`, `proto: syntax error (line L:C): invalid value <token>`, or `proto: unexpected token <token>` with Go's token regexp (`^([-+._a-zA-Z0-9]{1,32}|.)`), rune-based columns and lossy-replacement formatting. A token decoded from an invalid UTF-8 byte is flagged so the encoder can reproduce Go's escaping. Unit tests pin the oracle corpus texts. |
| `src/codec.rs` | `decode_json` now takes `M: DeserializeOwned + MessageName` and produces connect-go's texts: empty body → `unmarshal message: zero-length payload is not a valid JSON object`; syntax failures → `unmarshal message: unmarshal into *<package>.<Type>: <protojson detail>` (nested names joined with `_`). A token decoded from invalid UTF-8 marks the error so its U+FFFD is JSON-escaped as in Go (see `src/error.rs`). Syntactically valid but schema-invalid JSON keeps serde's text (outside the campaign corpus). |
| `src/error.rs` | `ConnectError` gains a private `escapes_replacement` flag (skipped by serde) and `to_json` rewrites each raw U+FFFD in the body as Go's six-character JSON escape. Go's `encoding/json` coerces strings from invalid UTF-8 this way; serde_json writes the raw bytes. Only the protojson malformed-token path sets the flag. |
| `src/envelope.rs` | Adds `incomplete_envelope_error`: short prefix → `protocol error: incomplete envelope: unexpected EOF`; otherwise `protocol error: promised N bytes in enveloped message, got M bytes`. Unsupported compression → `protocol error: sent compressed message without compression support`. `decode_eof` uses the helper. |
| `src/service.rs` | Connect and gRPC unary paths use connect-go's cardinality and framing texts: `unary request has zero messages`, `unary request has multiple messages`, the envelope helper, and the compression text. |
| `src/handler.rs`, `src/router.rs`, `src/payload.rs`, `src/dispatcher.rs` | Propagate the added `buffa::MessageName` bound through the `JsonDeserialize` request types and streaming dispatcher. No behavior change. |

## Keeping the fork honest

- The regression corpus lives in `crates/envd/tests/malformed_rpc.rs` and is
  captured from the pinned oracle by
  `scripts/tests/envd-parity/malformed_vm.py`.
- To update the upstream base, re-copy the registry crate and re-apply these
  patches; re-run the malformed gate before freezing a candidate.
- Do not add unrelated features here. Changes that are not oracle-parity
  fixes belong upstream or in `crates/envd`.
