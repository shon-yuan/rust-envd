# Vendored hyper fork

This directory is a fork of the crates.io crate **`hyper` 1.12.0**, copied
from the registry and patched in-tree. The published crate declares and ships
**MIT**; see `LICENSE` and `Cargo.toml.orig`.

The workspace selects it with `[patch.crates-io]` in the root `Cargo.toml`:

```toml
[patch.crates-io]
hyper = { path = "vendor/hyper" }
```

The fork exists solely for wire parity with the pinned oracle, official Go
envd 0.10.2 (commit `51083a83cc0903185d992725ea6bb33683814b29`), whose HTTP
server is Go 1.26.10 `net/http`. Hyper 1.12.0 refuses to write HTTP/1.1
response trailers unless the request carried `TE: trailers` **and** the
response announced each trailer name in a `Trailer` header. Go's `net/http`
does neither: a handler that appends trailer fields through `http.TrailerPrefix`
gets them written after the terminal chunk unconditionally. connect-go's gRPC
over HTTP/1.1 path relies on exactly that, so without this patch every unary
gRPC response loses its `grpc-status`/`grpc-message` trailers.

## Changed files

| File | Change |
| --- | --- |
| `src/proto/h1/conn.rs` | `write_trailers` no longer drops trailers when the server did not see `TE: trailers` on the request. |
| `src/proto/h1/role.rs` | A chunked response always installs a trailer encoder; a missing `Trailer` response header becomes an empty declared set instead of `Kind::Chunked(None)`. |
| `src/proto/h1/encode.rs` | An empty declared trailer set means "every valid trailer field", matching Go's unannounced `TrailerPrefix` output. A non-empty set still filters to the declared names. |
| `src/proto/h1/decode.rs` | Chunked request bodies follow Go's `internal/chunked.go` limits: a chunk-size line (excluding CRLF) of 4095 bytes or more is rejected (4094 is Go's last accepted length, because its 4096-byte `bufio` buffer must also hold the CRLF), and the running non-data overhead budget rejects a body once it exceeds 16 KiB after crediting 16 + 2n per chunk. Hyper's flat 16 KiB extension cap is replaced by these rules. |

The chunked limits matter to the response: Go drains an unread request body
before sending its response (`server.go:1393-1435`), and a body its chunked
reader rejects sets `closeAfterReply`, which adds `Connection: close` to the
HTTP/1.1 response. `crates/envd` implements that drain; the decoder must
therefore reject exactly the bodies Go rejects.

## Keeping the fork honest

- The gRPC/HTTP-1.1 trailer corpus lives in the envd parity harness
  (`scripts/tests/envd-parity/fixtures_http1_edges.py`) and compares trailer
  name/value pairs against the pinned oracle.
- To update the upstream base, re-copy the registry crate and re-apply these
  three patches; rerun the HTTP/1 edge gate and the conformance gates before
  freezing a candidate.
- Do not add unrelated features here. Changes that are not oracle-parity
  fixes belong upstream or in `crates/envd`.
