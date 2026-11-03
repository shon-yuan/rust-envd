# Third-Party Notices — rust-envd

This file and the `licenses/` directory beside it constitute the distributable dependency notice bundle for this project.

## Scope

These notices cover every distribution built from this repository: the linked `envd` daemon binaries produced from this crate, and any guest or sandbox image that embeds such a binary. Distributing a covered distribution requires reproducing this file, the `licenses/` tree, and the files called out in [`PACKAGING.md`](PACKAGING.md).

Enumerated from the pinned dependency graph via cargo metadata --locked --offline: 217 packages total including this crate; the 216 third-party crates indexed below carry the remaining linked-license obligations. Regenerate with `python3 scripts/generate_notices.py`; verify a checkout with `python3 scripts/generate_notices.py --check`.

## This project (MIT)

rust-envd (crate `coorworker-envd` 0.1.0) is licensed under the MIT License; see [`LICENSE`](../LICENSE) at the repository root.

Copyright (c) 2026 shon-yuan

## Vendored upstream schemas (Apache-2.0)

`third_party/e2b/` contains e2b `envd` protobuf schemas redistributed byte-for-byte from https://github.com/e2b-dev/runtime (release 0.10.2, commit 51083a83cc0903185d992725ea6bb33683814b29) under the Apache License, 2.0. Any covered distribution must include `third_party/e2b/LICENSE` and `third_party/e2b/PROVENANCE`; generated code built from those schemas carries the same attribution and does not replace those files.

## Go-derived source (BSD-3-Clause)

`src/platform/linux/mmds/go_url.rs` is a Go-derived redirect URL helper carrying The Go Authors' BSD-3-Clause notice ("Copyright 2009 The Go Authors.") in its file header. Binary redistribution of a covered distribution must reproduce that notice; source distributions include the file itself.

## Vendored Rust crate forks

`vendor/connectrpc/` is an in-tree fork of crates.io `connectrpc` 0.9.1 (Apache-2.0), selected with `[patch.crates-io]`. Covered distributions must include this file, the license text(s) under `licenses/connectrpc-0.9.1/`, and `vendor/connectrpc/PATCHES.md`, which records the upstream revision and every local change. The fork's license text is enumerated below with the other third-party crates.

`vendor/hyper/` is an in-tree fork of crates.io `hyper` 1.12.0 (MIT), selected with `[patch.crates-io]`. Covered distributions must include this file, the license text(s) under `licenses/hyper-1.12.0/`, and `vendor/hyper/PATCHES.md`, which records the upstream revision and every local change. The fork's license text is enumerated below with the other third-party crates.

## Third-party crates

Full license texts are copied verbatim under `licenses/<crate>-<version>/`. Copyright lines quoted per crate below are extracted from those files for convenience; where none is embedded in the license file(s), consult the full text.

### Index

| Crate                      | Version                       | License                                             | Files copied |
|----------------------------|-------------------------------|-----------------------------------------------------|---|
| adler2                     | 2.0.1                         | 0BSD OR MIT OR Apache-2.0                           | LICENSE-0BSD, LICENSE-APACHE, LICENSE-MIT |
| anyhow                     | 1.0.104                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| async-compression          | 0.4.51                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| async-trait                | 0.1.92                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| atomic-waker               | 1.1.2                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT, LICENSE-THIRD-PARTY |
| axum                       | 0.8.9                         | MIT                                                 | LICENSE |
| axum-core                  | 0.5.6                         | MIT                                                 | LICENSE |
| base64                     | 0.22.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| bitflags                   | 1.3.2                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| bitflags                   | 2.13.2                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| block-buffer               | 0.10.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| buffa                      | 0.9.1                         | Apache-2.0                                          | LICENSE |
| buffa-codegen              | 0.9.1                         | Apache-2.0                                          | LICENSE |
| buffa-descriptor           | 0.9.1                         | Apache-2.0                                          | LICENSE-APACHE |
| buffa-types                | 0.9.1                         | Apache-2.0                                          | LICENSE |
| bumpalo                    | 3.20.3                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| bytes                      | 1.12.1                        | MIT                                                 | LICENSE |
| cc                         | 1.8.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cfg-if                     | 1.0.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cfg_aliases                | 0.2.2                         | MIT                                                 | LICENSE |
| compression-codecs         | 0.4.46                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| compression-core           | 0.4.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| connectrpc                 | 0.9.1                         | Apache-2.0                                          | LICENSE |
| connectrpc-build           | 0.9.0                         | Apache-2.0                                          | LICENSE |
| connectrpc-codegen         | 0.9.0                         | Apache-2.0                                          | LICENSE |
| core-foundation            | 0.10.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| core-foundation-sys        | 0.8.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| core_detect                | 1.0.0                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| cpufeatures                | 0.2.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| crc32fast                  | 1.5.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| crypto-common              | 0.1.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| defmt                      | 1.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| defmt-macros               | 1.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| defmt-parser               | 1.1.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| digest                     | 0.10.7                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| displaydoc                 | 0.2.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| encoding_rs                | 0.8.42                        | (Apache-2.0 OR MIT) AND BSD-3-Clause                | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT, LICENSE-WHATWG |
| equivalent                 | 1.0.2                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| errno                      | 0.3.14                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| fastrand                   | 2.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| find-msvc-tools            | 0.1.14                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| flate2                     | 1.1.10                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| fnv                        | 1.0.7                         | Apache-2.0 / MIT                                    | LICENSE-APACHE, LICENSE-MIT |
| foldhash                   | 0.1.5                         | Zlib                                                | LICENSE |
| form_urlencoded            | 1.2.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures                    | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-channel            | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-core               | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-executor           | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-io                 | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-macro              | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-sink               | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-task               | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-util               | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| generic-array              | 0.14.7                        | MIT                                                 | LICENSE |
| getrandom                  | 0.2.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| getrandom                  | 0.3.4                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| h2                         | 0.4.20                        | MIT                                                 | LICENSE |
| hashbrown                  | 0.15.5                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hashbrown                  | 0.17.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| heck                       | 0.5.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| http                       | 1.5.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| http-body                  | 1.1.0                         | MIT                                                 | LICENSE |
| http-body-util             | 0.1.5                         | MIT                                                 | LICENSE |
| http-range                 | 0.1.5                         | MIT                                                 | LICENSE |
| httparse                   | 1.10.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| httpdate                   | 1.0.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hyper                      | 1.12.0                        | MIT                                                 | LICENSE |
| hyper-util                 | 0.1.21                        | MIT                                                 | LICENSE |
| icu_collections            | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_locale_core            | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_normalizer             | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_normalizer_data        | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_properties             | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_properties_data        | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_provider               | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| idna                       | 1.1.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| idna_adapter               | 1.2.2                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| indexmap                   | 2.14.2                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| inotify                    | 0.11.5                        | ISC                                                 | LICENSE |
| inotify-sys                | 0.1.8                         | ISC                                                 | LICENSE |
| itoa                       | 1.0.18                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| jiff                       | 0.2.38                        | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| jiff-core                  | 0.1.1                         | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| jiff-static                | 0.2.38                        | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| js-sys                     | 0.3.106                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| libc                       | 0.2.190                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| linux-raw-sys              | 0.12.1                        | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | COPYRIGHT, LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| litemap                    | 0.8.3                         | Unicode-3.0                                         | LICENSE |
| log                        | 0.4.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| mach2                      | 0.6.0                         | BSD-2-Clause OR MIT OR Apache-2.0                   | LICENSE-APACHE, LICENSE-BSD, LICENSE-MIT |
| matchit                    | 0.8.4                         | MIT AND BSD-3-Clause                                | LICENSE, LICENSE.httprouter |
| memchr                     | 2.8.3                         | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| memsec                     | 0.6.3                         | MIT                                                 | LICENSE |
| mime                       | 0.3.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| mime_guess                 | 2.0.5                         | MIT                                                 | LICENSE |
| miniz_oxide                | 0.9.1                         | MIT OR Zlib OR Apache-2.0                           | LICENSE, LICENSE-APACHE.md, LICENSE-MIT.md, LICENSE-ZLIB.md |
| mio                        | 1.2.4                         | MIT                                                 | LICENSE |
| multer                     | 3.1.0                         | MIT                                                 | LICENSE |
| multiversion_no_op         | 1.0.0                         | Apache-2.0 OR MIT                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| nix                        | 0.29.0                        | MIT                                                 | LICENSE |
| once_cell                  | 1.21.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| openssl-probe              | 0.2.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| percent-encoding           | 2.3.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| pin-project                | 1.1.13                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| pin-project-internal       | 1.1.13                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| pin-project-lite           | 0.2.17                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| portable-atomic            | 1.15.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| portable-atomic-util       | 0.2.8                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| potential_utf              | 0.1.6                         | Unicode-3.0                                         | LICENSE |
| prettyplease               | 0.2.37                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| proc-macro2                | 1.0.107                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| quote                      | 1.0.47                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| r-efi                      | 5.3.0                         | MIT OR Apache-2.0 OR LGPL-2.1-or-later              | AUTHORS |
| region                     | 4.0.1                         | MIT                                                 | LICENSE |
| ring                       | 0.17.14                       | Apache-2.0 AND ISC                                  | LICENSE, LICENSE-BoringSSL, LICENSE-other-bits |
| rustix                     | 1.1.5                         | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | COPYRIGHT, LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| rustls                     | 0.23.46                       | Apache-2.0 OR ISC OR MIT                            | LICENSE-APACHE, LICENSE-ISC, LICENSE-MIT |
| rustls-native-certs        | 0.8.5                         | Apache-2.0 OR ISC OR MIT                            | LICENSE, LICENSE-APACHE, LICENSE-ISC, LICENSE-MIT |
| rustls-pki-types           | 1.15.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| rustls-webpki              | 0.103.16                      | ISC                                                 | LICENSE |
| rustversion                | 1.0.23                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ryu                        | 1.0.23                        | Apache-2.0 OR BSL-1.0                               | LICENSE-APACHE, LICENSE-BOOST |
| schannel                   | 0.1.30                        | MIT                                                 | LICENSE.md |
| scopeguard                 | 1.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| seckey                     | 0.12.1                        | MIT                                                 | LICENSE |
| security-framework         | 3.7.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| security-framework-sys     | 2.17.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde                      | 1.0.229                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_core                 | 1.0.229                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_derive               | 1.0.229                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_json                 | 1.0.154                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_path_to_error        | 0.1.20                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_urlencoded           | 0.7.1                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| sha2                       | 0.10.9                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| shlex                      | 2.0.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| signal-hook-registry       | 1.4.8                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| simd-adler32               | 0.3.10                        | MIT                                                 | LICENSE.md |
| simdutf8                   | 0.1.5                         | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| slab                       | 0.4.12                        | MIT                                                 | LICENSE |
| smallvec                   | 1.16.3                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| smoothutf8                 | 0.2.3                         | Apache-2.0                                          | LICENSE |
| socket2                    | 0.6.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| spin                       | 0.9.9                         | MIT                                                 | LICENSE |
| stable_deref_trait         | 1.2.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| subtle                     | 2.6.1                         | BSD-3-Clause                                        | LICENSE |
| syn                        | 2.0.119                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| syn                        | 3.0.9                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| sync_wrapper               | 1.0.2                         | Apache-2.0                                          | LICENSE |
| synstructure               | 0.14.0                        | MIT                                                 | LICENSE |
| tempfile                   | 3.27.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| thiserror                  | 2.0.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| thiserror-impl             | 2.0.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| tinystr                    | 0.8.5                         | Unicode-3.0                                         | LICENSE |
| tokio                      | 1.53.2                        | MIT                                                 | LICENSE |
| tokio-macros               | 2.7.2                         | MIT                                                 | LICENSE |
| tokio-rustls               | 0.26.6                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| tokio-util                 | 0.7.20                        | MIT                                                 | LICENSE |
| tower                      | 0.5.3                         | MIT                                                 | LICENSE |
| tower-http                 | 0.6.11                        | MIT                                                 | LICENSE |
| tower-layer                | 0.3.3                         | MIT                                                 | LICENSE |
| tower-service              | 0.3.3                         | MIT                                                 | LICENSE |
| tracing                    | 0.1.44                        | MIT                                                 | LICENSE |
| tracing-attributes         | 0.1.31                        | MIT                                                 | LICENSE |
| tracing-core               | 0.1.36                        | MIT                                                 | LICENSE |
| try-lock                   | 0.2.5                         | MIT                                                 | LICENSE |
| typenum                    | 1.20.1                        | MIT OR Apache-2.0                                   | LICENSE, LICENSE-APACHE, LICENSE-MIT |
| unicase                    | 2.10.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| unicode-ident              | 1.0.27                        | (MIT OR Apache-2.0) AND Unicode-3.0                 | LICENSE-APACHE, LICENSE-MIT, LICENSE-UNICODE |
| untrusted                  | 0.9.0                         | ISC                                                 | LICENSE.txt |
| url                        | 2.5.8                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| utf8_iter                  | 1.1.0                         | Apache-2.0 OR MIT                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| utf_types                  | 1.0.0                         | Apache-2.0 OR MIT                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| version_check              | 0.9.5                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| want                       | 0.3.2                         | MIT                                                 | LICENSE |
| wasi                       | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| wasip2                     | 1.0.4+wasi-0.2.12             | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| wasm-bindgen               | 0.2.129                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-futures       | 0.4.79                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-macro         | 0.2.129                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-macro-support | 0.2.129                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-shared        | 0.2.129                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| windows-link               | 0.2.1                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-sys                | 0.45.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-sys                | 0.52.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-sys                | 0.61.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-targets            | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-targets            | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_gnullvm    | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_gnullvm    | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_msvc       | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_msvc       | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnu           | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnu           | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnullvm       | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_msvc          | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_msvc          | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnu         | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnu         | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnullvm     | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnullvm     | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_msvc        | 0.42.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_msvc        | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| wit-bindgen                | 0.57.1                        | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| writeable                  | 0.6.4                         | Unicode-3.0                                         | LICENSE |
| xattr                      | 1.6.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| yoke                       | 0.8.4                         | Unicode-3.0                                         | LICENSE |
| yoke-derive                | 0.8.4                         | Unicode-3.0                                         | LICENSE |
| zerofrom                   | 0.1.8                         | Unicode-3.0                                         | LICENSE |
| zerofrom-derive            | 0.1.8                         | Unicode-3.0                                         | LICENSE |
| zeroize                    | 1.9.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| zerotrie                   | 0.2.5                         | Unicode-3.0                                         | LICENSE |
| zerovec                    | 0.11.9                        | Unicode-3.0                                         | LICENSE |
| zerovec-derive             | 0.11.7                        | Unicode-3.0                                         | LICENSE |
| zlib-rs                    | 0.6.8                         | Zlib                                                | LICENSE |
| zmij                       | 1.0.23                        | MIT                                                 | LICENSE-MIT |

### Per-crate notices

#### adler2 2.0.1 — 0BSD OR MIT OR Apache-2.0

- Files: licenses/adler2-2.0.1/LICENSE-0BSD, licenses/adler2-2.0.1/LICENSE-APACHE, licenses/adler2-2.0.1/LICENSE-MIT
- Copyright (LICENSE-0BSD): Copyright (C) Jonas Schievink <jonasschievink@gmail.com>
- Repository: https://github.com/oyvindln/adler2

#### anyhow 1.0.104 — MIT OR Apache-2.0

- Files: licenses/anyhow-1.0.104/LICENSE-APACHE, licenses/anyhow-1.0.104/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/anyhow

#### async-compression 0.4.51 — MIT OR Apache-2.0

- Files: licenses/async-compression-0.4.51/LICENSE-APACHE, licenses/async-compression-0.4.51/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 the rustasync developers
- Repository: https://github.com/Nullus157/async-compression

#### async-trait 0.1.92 — MIT OR Apache-2.0

- Files: licenses/async-trait-0.1.92/LICENSE-APACHE, licenses/async-trait-0.1.92/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/async-trait

#### atomic-waker 1.1.2 — Apache-2.0 OR MIT

- Files: licenses/atomic-waker-1.1.2/LICENSE-APACHE, licenses/atomic-waker-1.1.2/LICENSE-MIT, licenses/atomic-waker-1.1.2/LICENSE-THIRD-PARTY
- Copyright (LICENSE-THIRD-PARTY): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-THIRD-PARTY): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/smol-rs/atomic-waker

#### axum 0.8.9 — MIT

- Files: licenses/axum-0.8.9/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 axum Contributors
- Repository: https://github.com/tokio-rs/axum

#### axum-core 0.5.6 — MIT

- Files: licenses/axum-core-0.5.6/LICENSE
- Copyright (LICENSE): Copyright (c) 2019–2025 axum Contributors
- Repository: https://github.com/tokio-rs/axum

#### base64 0.22.1 — MIT OR Apache-2.0

- Files: licenses/base64-0.22.1/LICENSE-APACHE, licenses/base64-0.22.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Alice Maz
- Repository: https://github.com/marshallpierce/rust-base64

#### bitflags 1.3.2 — MIT/Apache-2.0

- Files: licenses/bitflags-1.3.2/LICENSE-APACHE, licenses/bitflags-1.3.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/bitflags/bitflags

#### bitflags 2.13.2 — MIT OR Apache-2.0

- Files: licenses/bitflags-2.13.2/LICENSE-APACHE, licenses/bitflags-2.13.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/bitflags/bitflags

#### block-buffer 0.10.4 — MIT OR Apache-2.0

- Files: licenses/block-buffer-0.10.4/LICENSE-APACHE, licenses/block-buffer-0.10.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2019 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### buffa 0.9.1 — Apache-2.0

- Files: licenses/buffa-0.9.1/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/anthropics/buffa

#### buffa-codegen 0.9.1 — Apache-2.0

- Files: licenses/buffa-codegen-0.9.1/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/anthropics/buffa

#### buffa-descriptor 0.9.1 — Apache-2.0

- Files: licenses/buffa-descriptor-0.9.1/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from async-compression-0.4.51/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/anthropics/buffa

#### buffa-types 0.9.1 — Apache-2.0

- Files: licenses/buffa-types-0.9.1/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/anthropics/buffa

#### bumpalo 3.20.3 — MIT OR Apache-2.0

- Files: licenses/bumpalo-3.20.3/LICENSE-APACHE, licenses/bumpalo-3.20.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019 Nick Fitzgerald
- Repository: https://github.com/fitzgen/bumpalo

#### bytes 1.12.1 — MIT

- Files: licenses/bytes-1.12.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2018 Carl Lerche
- Repository: https://github.com/tokio-rs/bytes

#### cc 1.8.0 — MIT OR Apache-2.0

- Files: licenses/cc-1.8.0/LICENSE-APACHE, licenses/cc-1.8.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/cc-rs

#### cfg-if 1.0.5 — MIT OR Apache-2.0

- Files: licenses/cfg-if-1.0.5/LICENSE-APACHE, licenses/cfg-if-1.0.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/cfg-if

#### cfg_aliases 0.2.2 — MIT

- Files: licenses/cfg_aliases-0.2.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2020 Katharos Technology
- Repository: https://github.com/katharostech/cfg_aliases

#### compression-codecs 0.4.46 — MIT OR Apache-2.0

- Files: licenses/compression-codecs-0.4.46/LICENSE-APACHE, licenses/compression-codecs-0.4.46/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 the rustasync developers
- Repository: https://github.com/Nullus157/async-compression

#### compression-core 0.4.34 — MIT OR Apache-2.0

- Files: licenses/compression-core-0.4.34/LICENSE-APACHE, licenses/compression-core-0.4.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 the rustasync developers
- Repository: https://github.com/Nullus157/async-compression

#### connectrpc 0.9.1 — Apache-2.0

- Files: licenses/connectrpc-0.9.1/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/connectrpc/connect-rust

#### connectrpc-build 0.9.0 — Apache-2.0

- Files: licenses/connectrpc-build-0.9.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/connectrpc/connect-rust

#### connectrpc-codegen 0.9.0 — Apache-2.0

- Files: licenses/connectrpc-codegen-0.9.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/connectrpc/connect-rust

#### core-foundation 0.10.1 — MIT OR Apache-2.0

- Files: licenses/core-foundation-0.10.1/LICENSE-APACHE, licenses/core-foundation-0.10.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2012-2013 Mozilla Foundation
- Repository: https://github.com/servo/core-foundation-rs

#### core-foundation-sys 0.8.7 — MIT OR Apache-2.0

- Files: licenses/core-foundation-sys-0.8.7/LICENSE-APACHE, licenses/core-foundation-sys-0.8.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2012-2013 Mozilla Foundation
- Repository: https://github.com/servo/core-foundation-rs

#### core_detect 1.0.0 — MIT/Apache-2.0

- Files: licenses/core_detect-1.0.0/LICENSE-APACHE, licenses/core_detect-1.0.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2020 The Rust Project Developers
- Repository: https://github.com/thomcc/core_detect

#### cpufeatures 0.2.17 — MIT OR Apache-2.0

- Files: licenses/cpufeatures-0.2.17/LICENSE-APACHE, licenses/cpufeatures-0.2.17/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020-2025 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### crc32fast 1.5.2 — MIT OR Apache-2.0

- Files: licenses/crc32fast-1.5.2/LICENSE-APACHE, licenses/crc32fast-1.5.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 Sam Rijs, Alex Crichton and contributors
- Repository: https://github.com/srijs/rust-crc32fast

#### crypto-common 0.1.7 — MIT OR Apache-2.0

- Files: licenses/crypto-common-0.1.7/LICENSE-APACHE, licenses/crypto-common-0.1.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2021 RustCrypto Developers
- Repository: https://github.com/RustCrypto/traits

#### defmt 1.2.0 — MIT OR Apache-2.0

- Files: licenses/defmt-1.2.0/LICENSE-APACHE, licenses/defmt-1.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Ferrous Systems
- Repository: https://github.com/knurling-rs/defmt

#### defmt-macros 1.2.0 — MIT OR Apache-2.0

- Files: licenses/defmt-macros-1.2.0/LICENSE-APACHE, licenses/defmt-macros-1.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Ferrous Systems
- Repository: https://github.com/knurling-rs/defmt

#### defmt-parser 1.1.0 — MIT OR Apache-2.0

- Files: licenses/defmt-parser-1.1.0/LICENSE-APACHE, licenses/defmt-parser-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Ferrous Systems
- Repository: https://github.com/knurling-rs/defmt

#### digest 0.10.7 — MIT OR Apache-2.0

- Files: licenses/digest-0.10.7/LICENSE-APACHE, licenses/digest-0.10.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Artyom Pavlov
- Repository: https://github.com/RustCrypto/traits

#### displaydoc 0.2.7 — MIT OR Apache-2.0

- Files: licenses/displaydoc-0.2.7/LICENSE-APACHE, licenses/displaydoc-0.2.7/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/yaahc/displaydoc

#### encoding_rs 0.8.42 — (Apache-2.0 OR MIT) AND BSD-3-Clause

- Files: licenses/encoding_rs-0.8.42/COPYRIGHT, licenses/encoding_rs-0.8.42/LICENSE-APACHE, licenses/encoding_rs-0.8.42/LICENSE-MIT, licenses/encoding_rs-0.8.42/LICENSE-WHATWG
- Copyright (LICENSE-WHATWG): Copyright © WHATWG (Apple, Google, Mozilla, Microsoft).
- Repository: https://github.com/hsivonen/encoding_rs

#### equivalent 1.0.2 — Apache-2.0 OR MIT

- Files: licenses/equivalent-1.0.2/LICENSE-APACHE, licenses/equivalent-1.0.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016--2023
- Repository: https://github.com/indexmap-rs/equivalent

#### errno 0.3.14 — MIT OR Apache-2.0

- Files: licenses/errno-0.3.14/LICENSE-APACHE, licenses/errno-0.3.14/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Chris Wong
- Repository: https://github.com/lambda-fairy/rust-errno

#### fastrand 2.5.0 — Apache-2.0 OR MIT

- Files: licenses/fastrand-2.5.0/LICENSE-APACHE, licenses/fastrand-2.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/fastrand

#### find-msvc-tools 0.1.14 — MIT OR Apache-2.0

- Files: licenses/find-msvc-tools-0.1.14/LICENSE-APACHE, licenses/find-msvc-tools-0.1.14/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/cc-rs

#### flate2 1.1.10 — MIT OR Apache-2.0

- Files: licenses/flate2-1.1.10/LICENSE-APACHE, licenses/flate2-1.1.10/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014-2026 Alex Crichton
- Repository: https://github.com/rust-lang/flate2-rs

#### fnv 1.0.7 — Apache-2.0 / MIT

- Files: licenses/fnv-1.0.7/LICENSE-APACHE, licenses/fnv-1.0.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Contributors
- Repository: https://github.com/servo/rust-fnv

#### foldhash 0.1.5 — Zlib

- Files: licenses/foldhash-0.1.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Orson Peters
- Repository: https://github.com/orlp/foldhash

#### form_urlencoded 1.2.2 — MIT OR Apache-2.0

- Files: licenses/form_urlencoded-1.2.2/LICENSE-APACHE, licenses/form_urlencoded-1.2.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2016 The rust-url developers
- Repository: https://github.com/servo/rust-url

#### futures 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-0.3.34/LICENSE-APACHE, licenses/futures-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-channel 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-channel-0.3.34/LICENSE-APACHE, licenses/futures-channel-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-core 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-core-0.3.34/LICENSE-APACHE, licenses/futures-core-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-executor 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-executor-0.3.34/LICENSE-APACHE, licenses/futures-executor-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-io 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-io-0.3.34/LICENSE-APACHE, licenses/futures-io-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-macro 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-macro-0.3.34/LICENSE-APACHE, licenses/futures-macro-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-sink 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-sink-0.3.34/LICENSE-APACHE, licenses/futures-sink-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-task 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-task-0.3.34/LICENSE-APACHE, licenses/futures-task-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-util 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-util-0.3.34/LICENSE-APACHE, licenses/futures-util-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### generic-array 0.14.7 — MIT

- Files: licenses/generic-array-0.14.7/LICENSE
- Copyright (LICENSE): Copyright (c) 2015 Bartłomiej Kamiński
- Repository: https://github.com/fizyk20/generic-array.git

#### getrandom 0.2.17 — MIT OR Apache-2.0

- Files: licenses/getrandom-0.2.17/LICENSE-APACHE, licenses/getrandom-0.2.17/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2024 The rust-random Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/getrandom

#### getrandom 0.3.4 — MIT OR Apache-2.0

- Files: licenses/getrandom-0.3.4/LICENSE-APACHE, licenses/getrandom-0.3.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2025 The rust-random Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/getrandom

#### h2 0.4.20 — MIT

- Files: licenses/h2-0.4.20/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 h2 authors
- Repository: https://github.com/hyperium/h2

#### hashbrown 0.15.5 — MIT OR Apache-2.0

- Files: licenses/hashbrown-0.15.5/LICENSE-APACHE, licenses/hashbrown-0.15.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Amanieu d'Antras
- Repository: https://github.com/rust-lang/hashbrown

#### hashbrown 0.17.1 — MIT OR Apache-2.0

- Files: licenses/hashbrown-0.17.1/LICENSE-APACHE, licenses/hashbrown-0.17.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Amanieu d'Antras
- Repository: https://github.com/rust-lang/hashbrown

#### heck 0.5.0 — MIT OR Apache-2.0

- Files: licenses/heck-0.5.0/LICENSE-APACHE, licenses/heck-0.5.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 The Rust Project Developers
- Repository: https://github.com/withoutboats/heck

#### http 1.5.0 — MIT OR Apache-2.0

- Files: licenses/http-1.5.0/LICENSE-APACHE, licenses/http-1.5.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 http-rs authors
- Repository: https://github.com/hyperium/http

#### http-body 1.1.0 — MIT

- Files: licenses/http-body-1.1.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2019-2026 Sean McArthur & Hyper Contributors
- Repository: https://github.com/hyperium/http-body

#### http-body-util 0.1.5 — MIT

- Files: licenses/http-body-util-0.1.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2019-2026 Sean McArthur & Hyper Contributors
- Repository: https://github.com/hyperium/http-body

#### http-range 0.1.5 — MIT

- Files: licenses/http-range-0.1.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 Luka Zakrajšek
- Repository: https://github.com/bancek/rust-http-range.git

#### httparse 1.10.1 — MIT OR Apache-2.0

- Files: licenses/httparse-1.10.1/LICENSE-APACHE, licenses/httparse-1.10.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2025 Sean McArthur
- Repository: https://github.com/seanmonstar/httparse

#### httpdate 1.0.3 — MIT OR Apache-2.0

- Files: licenses/httpdate-1.0.3/LICENSE-APACHE, licenses/httpdate-1.0.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Pyfisch
- Repository: https://github.com/pyfisch/httpdate

#### hyper 1.12.0 — MIT

- Files: licenses/hyper-1.12.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2014-2026 Sean McArthur
- Repository: https://github.com/hyperium/hyper

#### hyper-util 0.1.21 — MIT

- Files: licenses/hyper-util-0.1.21/LICENSE
- Copyright (LICENSE): Copyright (c) 2023-2025 Sean McArthur
- Repository: https://github.com/hyperium/hyper-util

#### icu_collections 2.4.0 — Unicode-3.0

- Files: licenses/icu_collections-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### icu_locale_core 2.4.0 — Unicode-3.0

- Files: licenses/icu_locale_core-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### icu_normalizer 2.4.0 — Unicode-3.0

- Files: licenses/icu_normalizer-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### icu_normalizer_data 2.4.0 — Unicode-3.0

- Files: licenses/icu_normalizer_data-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### icu_properties 2.4.0 — Unicode-3.0

- Files: licenses/icu_properties-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### icu_properties_data 2.4.0 — Unicode-3.0

- Files: licenses/icu_properties_data-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### icu_provider 2.4.0 — Unicode-3.0

- Files: licenses/icu_provider-2.4.0/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### idna 1.1.0 — MIT OR Apache-2.0

- Files: licenses/idna-1.1.0/LICENSE-APACHE, licenses/idna-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2025 The rust-url developers
- Repository: https://github.com/servo/rust-url/

#### idna_adapter 1.2.2 — Apache-2.0 OR MIT

- Files: licenses/idna_adapter-1.2.2/LICENSE-APACHE, licenses/idna_adapter-1.2.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) The rust-url developers
- Repository: https://github.com/hsivonen/idna_adapter

#### indexmap 2.14.2 — Apache-2.0 OR MIT

- Files: licenses/indexmap-2.14.2/LICENSE-APACHE, licenses/indexmap-2.14.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016--2017
- Repository: https://github.com/indexmap-rs/indexmap

#### inotify 0.11.5 — ISC

- Files: licenses/inotify-0.11.5/LICENSE
- Copyright (LICENSE): Copyright (c) Hanno Braun and contributors
- Repository: https://github.com/hannobraun/inotify-rs

#### inotify-sys 0.1.8 — ISC

- Files: licenses/inotify-sys-0.1.8/LICENSE
- Copyright (LICENSE): Copyright (c) Hanno Braun and contributors
- Repository: https://github.com/hannobraun/inotify-sys

#### itoa 1.0.18 — MIT OR Apache-2.0

- Files: licenses/itoa-1.0.18/LICENSE-APACHE, licenses/itoa-1.0.18/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/itoa

#### jiff 0.2.38 — Unlicense OR MIT

- Files: licenses/jiff-0.2.38/COPYING, licenses/jiff-0.2.38/LICENSE-MIT, licenses/jiff-0.2.38/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/jiff

#### jiff-core 0.1.1 — Unlicense OR MIT

- Files: licenses/jiff-core-0.1.1/COPYING, licenses/jiff-core-0.1.1/LICENSE-MIT, licenses/jiff-core-0.1.1/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/jiff

#### jiff-static 0.2.38 — Unlicense OR MIT

- Files: licenses/jiff-static-0.2.38/COPYING, licenses/jiff-static-0.2.38/LICENSE-MIT, licenses/jiff-static-0.2.38/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/jiff

#### js-sys 0.3.106 — MIT OR Apache-2.0

- Files: licenses/js-sys-0.3.106/LICENSE-APACHE, licenses/js-sys-0.3.106/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys

#### libc 0.2.190 — MIT OR Apache-2.0

- Files: licenses/libc-0.2.190/LICENSE-APACHE, licenses/libc-0.2.190/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) The Rust Project Developers
- Repository: https://github.com/rust-lang/libc

#### linux-raw-sys 0.12.1 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/linux-raw-sys-0.12.1/COPYRIGHT, licenses/linux-raw-sys-0.12.1/LICENSE-APACHE, licenses/linux-raw-sys-0.12.1/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/linux-raw-sys-0.12.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/sunfishcode/linux-raw-sys

#### litemap 0.8.3 — Unicode-3.0

- Files: licenses/litemap-0.8.3/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### log 0.4.34 — MIT OR Apache-2.0

- Files: licenses/log-0.4.34/LICENSE-APACHE, licenses/log-0.4.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-lang/log

#### mach2 0.6.0 — BSD-2-Clause OR MIT OR Apache-2.0

- Files: licenses/mach2-0.6.0/LICENSE-APACHE, licenses/mach2-0.6.0/LICENSE-BSD, licenses/mach2-0.6.0/LICENSE-MIT
- Copyright (LICENSE-BSD): Copyright (c) 2019 Nick Fitzgerald, 2021 Yuki Okushi
- Copyright (LICENSE-MIT): Copyright (c) 2019 Nick Fitzgerald, 2021 Yuki Okushi
- Repository: https://github.com/JohnTitor/mach2

#### matchit 0.8.4 — MIT AND BSD-3-Clause

- Files: licenses/matchit-0.8.4/LICENSE, licenses/matchit-0.8.4/LICENSE.httprouter
- Copyright (LICENSE): Copyright (c) 2022 Ibraheem Ahmed
- Copyright (LICENSE.httprouter): Copyright (c) 2013, Julien Schmidt
- Repository: https://github.com/ibraheemdev/matchit

#### memchr 2.8.3 — Unlicense OR MIT

- Files: licenses/memchr-2.8.3/COPYING, licenses/memchr-2.8.3/LICENSE-MIT, licenses/memchr-2.8.3/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/memchr

#### memsec 0.6.3 — MIT

- Files: licenses/memsec-0.6.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 quininer@live.com
- Repository: https://github.com/quininer/memsec

#### mime 0.3.17 — MIT OR Apache-2.0

- Files: licenses/mime-0.3.17/LICENSE-APACHE, licenses/mime-0.3.17/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Sean McArthur
- Repository: https://github.com/hyperium/mime

#### mime_guess 2.0.5 — MIT

- Files: licenses/mime_guess-2.0.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2015 Austin Bonander
- Repository: https://github.com/abonander/mime_guess

#### miniz_oxide 0.9.1 — MIT OR Zlib OR Apache-2.0

- Files: licenses/miniz_oxide-0.9.1/LICENSE, licenses/miniz_oxide-0.9.1/LICENSE-APACHE.md, licenses/miniz_oxide-0.9.1/LICENSE-MIT.md, licenses/miniz_oxide-0.9.1/LICENSE-ZLIB.md
- Copyright (LICENSE): Copyright 2013-2014 RAD Game Tools and Valve Software
- Copyright (LICENSE): Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC
- Copyright (LICENSE): Copyright (c) 2017 Frommi
- ... (+1 more copyright line(s) in LICENSE; see full text)
- Copyright (LICENSE-MIT.md): Copyright 2013-2014 RAD Game Tools and Valve Software
- Copyright (LICENSE-MIT.md): Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC
- Copyright (LICENSE-MIT.md): Copyright (c) 2017 Frommi
- ... (+1 more copyright line(s) in LICENSE-MIT.md; see full text)
- Repository: https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide

#### mio 1.2.4 — MIT

- Files: licenses/mio-1.2.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2014 Carl Lerche and other MIO contributors
- Repository: https://github.com/tokio-rs/mio

#### multer 3.1.0 — MIT

- Files: licenses/multer-3.1.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2020 Rousan Ali
- Repository: https://github.com/rwf2/multer

#### multiversion_no_op 1.0.0 — Apache-2.0 OR MIT

- Files: licenses/multiversion_no_op-1.0.0/COPYRIGHT, licenses/multiversion_no_op-1.0.0/LICENSE-APACHE, licenses/multiversion_no_op-1.0.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/hsivonen/multiversion_no_op

#### nix 0.29.0 — MIT

- Files: licenses/nix-0.29.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2015 Carl Lerche + nix-rust Authors
- Repository: https://github.com/nix-rust/nix

#### once_cell 1.21.4 — MIT OR Apache-2.0

- Files: licenses/once_cell-1.21.4/LICENSE-APACHE, licenses/once_cell-1.21.4/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/matklad/once_cell

#### openssl-probe 0.2.2 — MIT OR Apache-2.0

- Files: licenses/openssl-probe-0.2.2/LICENSE-APACHE, licenses/openssl-probe-0.2.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rustls/openssl-probe

#### percent-encoding 2.3.2 — MIT OR Apache-2.0

- Files: licenses/percent-encoding-2.3.2/LICENSE-APACHE, licenses/percent-encoding-2.3.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2025 The rust-url developers
- Repository: https://github.com/servo/rust-url/

#### pin-project 1.1.13 — Apache-2.0 OR MIT

- Files: licenses/pin-project-1.1.13/LICENSE-APACHE, licenses/pin-project-1.1.13/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/taiki-e/pin-project

#### pin-project-internal 1.1.13 — Apache-2.0 OR MIT

- Files: licenses/pin-project-internal-1.1.13/LICENSE-APACHE, licenses/pin-project-internal-1.1.13/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/taiki-e/pin-project

#### pin-project-lite 0.2.17 — Apache-2.0 OR MIT

- Files: licenses/pin-project-lite-0.2.17/LICENSE-APACHE, licenses/pin-project-lite-0.2.17/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/taiki-e/pin-project-lite

#### portable-atomic 1.15.0 — Apache-2.0 OR MIT

- Files: licenses/portable-atomic-1.15.0/LICENSE-APACHE, licenses/portable-atomic-1.15.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/taiki-e/portable-atomic

#### portable-atomic-util 0.2.8 — Apache-2.0 OR MIT

- Files: licenses/portable-atomic-util-0.2.8/LICENSE-APACHE, licenses/portable-atomic-util-0.2.8/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/taiki-e/portable-atomic-util

#### potential_utf 0.1.6 — Unicode-3.0

- Files: licenses/potential_utf-0.1.6/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### prettyplease 0.2.37 — MIT OR Apache-2.0

- Files: licenses/prettyplease-0.2.37/LICENSE-APACHE, licenses/prettyplease-0.2.37/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/prettyplease

#### proc-macro2 1.0.107 — MIT OR Apache-2.0

- Files: licenses/proc-macro2-1.0.107/LICENSE-APACHE, licenses/proc-macro2-1.0.107/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/proc-macro2

#### quote 1.0.47 — MIT OR Apache-2.0

- Files: licenses/quote-1.0.47/LICENSE-APACHE, licenses/quote-1.0.47/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/quote

#### r-efi 5.3.0 — MIT OR Apache-2.0 OR LGPL-2.1-or-later

- Files: licenses/r-efi-5.3.0/AUTHORS
- Copyright (AUTHORS): Copyright (C) 2017-2023 Red Hat, Inc.
- Copyright (AUTHORS): Copyright (C) 2019-2023 Microsoft Corporation
- Copyright (AUTHORS): Copyright (C) 2022-2023 David Rheinsberg
- Repository: https://github.com/r-efi/r-efi

#### region 4.0.1 — MIT

- Files: licenses/region-4.0.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 Elliott Linder
- Repository: https://github.com/darfink/region-rs

#### ring 0.17.14 — Apache-2.0 AND ISC

- Files: licenses/ring-0.17.14/LICENSE, licenses/ring-0.17.14/LICENSE-BoringSSL, licenses/ring-0.17.14/LICENSE-other-bits
- Copyright (LICENSE-other-bits): Copyright 2015-2025 Brian Smith.
- Repository: https://github.com/briansmith/ring

#### rustix 1.1.5 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/rustix-1.1.5/COPYRIGHT, licenses/rustix-1.1.5/LICENSE-APACHE, licenses/rustix-1.1.5/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/rustix-1.1.5/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/rustix

#### rustls 0.23.46 — Apache-2.0 OR ISC OR MIT

- Files: licenses/rustls-0.23.46/LICENSE-APACHE, licenses/rustls-0.23.46/LICENSE-ISC, licenses/rustls-0.23.46/LICENSE-MIT
- Copyright (LICENSE-ISC): Copyright (c) 2016, Joseph Birr-Pixton <jpixton@gmail.com>
- Copyright (LICENSE-MIT): Copyright (c) 2016 Joseph Birr-Pixton <jpixton@gmail.com>
- Repository: https://github.com/rustls/rustls

#### rustls-native-certs 0.8.5 — Apache-2.0 OR ISC OR MIT

- Files: licenses/rustls-native-certs-0.8.5/LICENSE, licenses/rustls-native-certs-0.8.5/LICENSE-APACHE, licenses/rustls-native-certs-0.8.5/LICENSE-ISC, licenses/rustls-native-certs-0.8.5/LICENSE-MIT
- Copyright (LICENSE-ISC): Copyright (c) 2016, Joseph Birr-Pixton <jpixton@gmail.com>
- Copyright (LICENSE-MIT): Copyright (c) 2016 Joseph Birr-Pixton <jpixton@gmail.com>
- Repository: https://github.com/rustls/rustls-native-certs

#### rustls-pki-types 1.15.1 — MIT OR Apache-2.0

- Files: licenses/rustls-pki-types-1.15.1/LICENSE-APACHE, licenses/rustls-pki-types-1.15.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2023 Dirkjan Ochtman <dirkjan@ochtman.nl>
- Repository: https://github.com/rustls/pki-types

#### rustls-webpki 0.103.16 — ISC

- Files: licenses/rustls-webpki-0.103.16/LICENSE
- Copyright (LICENSE): Copyright 2015 Brian Smith.
- Repository: https://github.com/rustls/webpki

#### rustversion 1.0.23 — MIT OR Apache-2.0

- Files: licenses/rustversion-1.0.23/LICENSE-APACHE, licenses/rustversion-1.0.23/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/rustversion

#### ryu 1.0.23 — Apache-2.0 OR BSL-1.0

- Files: licenses/ryu-1.0.23/LICENSE-APACHE, licenses/ryu-1.0.23/LICENSE-BOOST
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/ryu

#### schannel 0.1.30 — MIT

- Files: licenses/schannel-0.1.30/LICENSE.md
- Copyright (LICENSE.md): Copyright (c) 2015 steffengy
- Repository: https://github.com/steffengy/schannel-rs

#### scopeguard 1.2.0 — MIT OR Apache-2.0

- Files: licenses/scopeguard-1.2.0/LICENSE-APACHE, licenses/scopeguard-1.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016-2019 Ulrik Sverdrup "bluss" and scopeguard developers
- Repository: https://github.com/bluss/scopeguard

#### seckey 0.12.1 — MIT

- Files: licenses/seckey-0.12.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 quininer@live.com
- Repository: https://github.com/quininer/seckey

#### security-framework 3.7.0 — MIT OR Apache-2.0

- Files: licenses/security-framework-3.7.0/LICENSE-APACHE, licenses/security-framework-3.7.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Fackler
- Repository: https://github.com/kornelski/rust-security-framework

#### security-framework-sys 2.17.0 — MIT OR Apache-2.0

- Files: licenses/security-framework-sys-2.17.0/LICENSE-APACHE, licenses/security-framework-sys-2.17.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Fackler
- Repository: https://github.com/kornelski/rust-security-framework

#### serde 1.0.229 — MIT OR Apache-2.0

- Files: licenses/serde-1.0.229/LICENSE-APACHE, licenses/serde-1.0.229/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/serde-rs/serde

#### serde_core 1.0.229 — MIT OR Apache-2.0

- Files: licenses/serde_core-1.0.229/LICENSE-APACHE, licenses/serde_core-1.0.229/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/serde-rs/serde

#### serde_derive 1.0.229 — MIT OR Apache-2.0

- Files: licenses/serde_derive-1.0.229/LICENSE-APACHE, licenses/serde_derive-1.0.229/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/serde-rs/serde

#### serde_json 1.0.154 — MIT OR Apache-2.0

- Files: licenses/serde_json-1.0.154/LICENSE-APACHE, licenses/serde_json-1.0.154/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/serde-rs/json

#### serde_path_to_error 0.1.20 — MIT OR Apache-2.0

- Files: licenses/serde_path_to_error-0.1.20/LICENSE-APACHE, licenses/serde_path_to_error-0.1.20/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/path-to-error

#### serde_urlencoded 0.7.1 — MIT/Apache-2.0

- Files: licenses/serde_urlencoded-0.7.1/LICENSE-APACHE, licenses/serde_urlencoded-0.7.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Anthony Ramine
- Repository: https://github.com/nox/serde_urlencoded

#### sha2 0.10.9 — MIT OR Apache-2.0

- Files: licenses/sha2-0.10.9/LICENSE-APACHE, licenses/sha2-0.10.9/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2006-2009 Graydon Hoare
- Copyright (LICENSE-MIT): Copyright (c) 2009-2013 Mozilla Foundation
- Copyright (LICENSE-MIT): Copyright (c) 2016 Artyom Pavlov
- Repository: https://github.com/RustCrypto/hashes

#### shlex 2.0.1 — MIT OR Apache-2.0

- Files: licenses/shlex-2.0.1/LICENSE-APACHE, licenses/shlex-2.0.1/LICENSE-MIT
- Copyright (LICENSE-APACHE): Copyright 2015 Nicholas Allegra (comex).
- Copyright (LICENSE-MIT): Copyright (c) 2015 Nicholas Allegra (comex).
- Repository: https://github.com/comex/rust-shlex

#### signal-hook-registry 1.4.8 — MIT OR Apache-2.0

- Files: licenses/signal-hook-registry-1.4.8/LICENSE-APACHE, licenses/signal-hook-registry-1.4.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 tokio-jsonrpc developers
- Repository: https://github.com/vorner/signal-hook

#### simd-adler32 0.3.10 — MIT

- Files: licenses/simd-adler32-0.3.10/LICENSE.md
- Copyright (LICENSE.md): Copyright (c) [2021] [Marvin Countryman]
- Repository: https://github.com/mcountryman/simd-adler32

#### simdutf8 0.1.5 — MIT OR Apache-2.0

- Files: licenses/simdutf8-0.1.5/LICENSE-Apache, licenses/simdutf8-0.1.5/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/rusticstuff/simdutf8

#### slab 0.4.12 — MIT

- Files: licenses/slab-0.4.12/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Carl Lerche
- Repository: https://github.com/tokio-rs/slab

#### smallvec 1.16.3 — MIT OR Apache-2.0

- Files: licenses/smallvec-1.16.3/LICENSE-APACHE, licenses/smallvec-1.16.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 The Servo Project Developers
- Repository: https://github.com/servo/rust-smallvec

#### smoothutf8 0.2.3 — Apache-2.0

- Files: licenses/smoothutf8-0.2.3/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/iainmcgin/smooth-utf8

#### socket2 0.6.5 — MIT OR Apache-2.0

- Files: licenses/socket2-0.6.5/LICENSE-APACHE, licenses/socket2-0.6.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/socket2

#### spin 0.9.9 — MIT

- Files: licenses/spin-0.9.9/LICENSE
- Copyright (LICENSE): Copyright (c) 2014 Mathijs van de Nes
- Repository: https://github.com/mvdnes/spin-rs.git

#### stable_deref_trait 1.2.1 — MIT OR Apache-2.0

- Files: licenses/stable_deref_trait-1.2.1/LICENSE-APACHE, licenses/stable_deref_trait-1.2.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Robert Grosse
- Repository: https://github.com/storyyeller/stable_deref_trait

#### subtle 2.6.1 — BSD-3-Clause

- Files: licenses/subtle-2.6.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2016-2017 Isis Agora Lovecruft, Henry de Valence. All rights reserved.
- Copyright (LICENSE): Copyright (c) 2016-2024 Isis Agora Lovecruft. All rights reserved.
- Repository: https://github.com/dalek-cryptography/subtle

#### syn 2.0.119 — MIT OR Apache-2.0

- Files: licenses/syn-2.0.119/LICENSE-APACHE, licenses/syn-2.0.119/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/syn

#### syn 3.0.9 — MIT OR Apache-2.0

- Files: licenses/syn-3.0.9/LICENSE-APACHE, licenses/syn-3.0.9/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/syn

#### sync_wrapper 1.0.2 — Apache-2.0

- Files: licenses/sync_wrapper-1.0.2/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/Actyx/sync_wrapper

#### synstructure 0.14.0 — MIT

- Files: licenses/synstructure-0.14.0/LICENSE
- Copyright (LICENSE): Copyright 2016 Nika Layzell
- Repository: https://github.com/mystor/synstructure

#### tempfile 3.27.0 — MIT OR Apache-2.0

- Files: licenses/tempfile-3.27.0/LICENSE-APACHE, licenses/tempfile-3.27.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Allen
- Repository: https://github.com/Stebalien/tempfile

#### thiserror 2.0.21 — MIT OR Apache-2.0

- Files: licenses/thiserror-2.0.21/LICENSE-APACHE, licenses/thiserror-2.0.21/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/thiserror

#### thiserror-impl 2.0.21 — MIT OR Apache-2.0

- Files: licenses/thiserror-impl-2.0.21/LICENSE-APACHE, licenses/thiserror-impl-2.0.21/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/thiserror

#### tinystr 0.8.5 — Unicode-3.0

- Files: licenses/tinystr-0.8.5/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### tokio 1.53.2 — MIT

- Files: licenses/tokio-1.53.2/LICENSE
- Copyright (LICENSE): Copyright (c) Tokio Contributors
- Repository: https://github.com/tokio-rs/tokio

#### tokio-macros 2.7.2 — MIT

- Files: licenses/tokio-macros-2.7.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Yoshua Wuyts
- Copyright (LICENSE): Copyright (c) Tokio Contributors
- Repository: https://github.com/tokio-rs/tokio

#### tokio-rustls 0.26.6 — MIT OR Apache-2.0

- Files: licenses/tokio-rustls-0.26.6/LICENSE-APACHE, licenses/tokio-rustls-0.26.6/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 quininer kel
- Repository: https://github.com/rustls/tokio-rustls

#### tokio-util 0.7.20 — MIT

- Files: licenses/tokio-util-0.7.20/LICENSE
- Copyright (LICENSE): Copyright (c) Tokio Contributors
- Repository: https://github.com/tokio-rs/tokio

#### tower 0.5.3 — MIT

- Files: licenses/tower-0.5.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tower Contributors
- Repository: https://github.com/tower-rs/tower

#### tower-http 0.6.11 — MIT

- Files: licenses/tower-http-0.6.11/LICENSE
- Copyright (LICENSE): Copyright (c) 2019-2021 Tower Contributors
- Repository: https://github.com/tower-rs/tower-http

#### tower-layer 0.3.3 — MIT

- Files: licenses/tower-layer-0.3.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tower Contributors
- Repository: https://github.com/tower-rs/tower

#### tower-service 0.3.3 — MIT

- Files: licenses/tower-service-0.3.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tower Contributors
- Repository: https://github.com/tower-rs/tower

#### tracing 0.1.44 — MIT

- Files: licenses/tracing-0.1.44/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tokio Contributors
- Repository: https://github.com/tokio-rs/tracing

#### tracing-attributes 0.1.31 — MIT

- Files: licenses/tracing-attributes-0.1.31/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tokio Contributors
- Repository: https://github.com/tokio-rs/tracing

#### tracing-core 0.1.36 — MIT

- Files: licenses/tracing-core-0.1.36/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tokio Contributors
- Repository: https://github.com/tokio-rs/tracing

#### try-lock 0.2.5 — MIT

- Files: licenses/try-lock-0.2.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2018-2023 Sean McArthur
- Copyright (LICENSE): Copyright (c) 2016 Alex Crichton
- Repository: https://github.com/seanmonstar/try-lock

#### typenum 1.20.1 — MIT OR Apache-2.0

- Files: licenses/typenum-1.20.1/LICENSE, licenses/typenum-1.20.1/LICENSE-APACHE, licenses/typenum-1.20.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Paho Lurie-Gregg
- Repository: https://github.com/paholg/typenum

#### unicase 2.10.0 — MIT OR Apache-2.0

- Files: licenses/unicase-2.10.0/LICENSE-APACHE, licenses/unicase-2.10.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014-2026 Sean McArthur
- Repository: https://github.com/seanmonstar/unicase

#### unicode-ident 1.0.27 — (MIT OR Apache-2.0) AND Unicode-3.0

- Files: licenses/unicode-ident-1.0.27/LICENSE-APACHE, licenses/unicode-ident-1.0.27/LICENSE-MIT, licenses/unicode-ident-1.0.27/LICENSE-UNICODE
- Copyright (LICENSE-UNICODE): Copyright © 1991-2023 Unicode, Inc.
- Repository: https://github.com/dtolnay/unicode-ident

#### untrusted 0.9.0 — ISC

- Files: licenses/untrusted-0.9.0/LICENSE.txt
- Copyright (LICENSE.txt): // Copyright 2015-2016 Brian Smith.
- Repository: https://github.com/briansmith/untrusted

#### url 2.5.8 — MIT OR Apache-2.0

- Files: licenses/url-2.5.8/LICENSE-APACHE, licenses/url-2.5.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2025 The rust-url developers
- Repository: https://github.com/servo/rust-url

#### utf8_iter 1.1.0 — Apache-2.0 OR MIT

- Files: licenses/utf8_iter-1.1.0/COPYRIGHT, licenses/utf8_iter-1.1.0/LICENSE-APACHE, licenses/utf8_iter-1.1.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/hsivonen/utf8_iter

#### utf_types 1.0.0 — Apache-2.0 OR MIT

- Files: licenses/utf_types-1.0.0/COPYRIGHT, licenses/utf_types-1.0.0/LICENSE-APACHE, licenses/utf_types-1.0.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/hsivonen/utf_types

#### version_check 0.9.5 — MIT/Apache-2.0

- Files: licenses/version_check-0.9.5/LICENSE-APACHE, licenses/version_check-0.9.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2018 Sergio Benitez
- Repository: https://github.com/SergioBenitez/version_check

#### want 0.3.2 — MIT

- Files: licenses/want-0.3.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2018-2019 Sean McArthur
- Repository: https://github.com/seanmonstar/want

#### wasi 0.11.1+wasi-snapshot-preview1 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/wasi-0.11.1+wasi-snapshot-preview1/LICENSE-APACHE, licenses/wasi-0.11.1+wasi-snapshot-preview1/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/wasi-0.11.1+wasi-snapshot-preview1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/wasi

#### wasip2 1.0.4+wasi-0.2.12 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/wasip2-1.0.4+wasi-0.2.12/LICENSE-APACHE, licenses/wasip2-1.0.4+wasi-0.2.12/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/wasip2-1.0.4+wasi-0.2.12/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/wasi-rs

#### wasm-bindgen 0.2.129 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-0.2.129/LICENSE-APACHE, licenses/wasm-bindgen-0.2.129/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen

#### wasm-bindgen-futures 0.4.79 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-futures-0.4.79/LICENSE-APACHE, licenses/wasm-bindgen-futures-0.4.79/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/futures

#### wasm-bindgen-macro 0.2.129 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-macro-0.2.129/LICENSE-APACHE, licenses/wasm-bindgen-macro-0.2.129/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro

#### wasm-bindgen-macro-support 0.2.129 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-macro-support-0.2.129/LICENSE-APACHE, licenses/wasm-bindgen-macro-support-0.2.129/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support

#### wasm-bindgen-shared 0.2.129 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-shared-0.2.129/LICENSE-APACHE, licenses/wasm-bindgen-shared-0.2.129/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared

#### windows-link 0.2.1 — MIT OR Apache-2.0

- Files: licenses/windows-link-0.2.1/license-apache-2.0, licenses/windows-link-0.2.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-sys 0.45.0 — MIT OR Apache-2.0

- Files: licenses/windows-sys-0.45.0/license-apache-2.0, licenses/windows-sys-0.45.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-sys 0.52.0 — MIT OR Apache-2.0

- Files: licenses/windows-sys-0.52.0/license-apache-2.0, licenses/windows-sys-0.52.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-sys 0.61.2 — MIT OR Apache-2.0

- Files: licenses/windows-sys-0.61.2/license-apache-2.0, licenses/windows-sys-0.61.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-targets 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows-targets-0.42.2/license-apache-2.0, licenses/windows-targets-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-targets 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows-targets-0.52.6/license-apache-2.0, licenses/windows-targets-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_gnullvm 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_gnullvm-0.42.2/license-apache-2.0, licenses/windows_aarch64_gnullvm-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_gnullvm 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_gnullvm-0.52.6/license-apache-2.0, licenses/windows_aarch64_gnullvm-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_msvc 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_msvc-0.42.2/license-apache-2.0, licenses/windows_aarch64_msvc-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_msvc 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_msvc-0.52.6/license-apache-2.0, licenses/windows_aarch64_msvc-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnu 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnu-0.42.2/license-apache-2.0, licenses/windows_i686_gnu-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnu 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnu-0.52.6/license-apache-2.0, licenses/windows_i686_gnu-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnullvm 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnullvm-0.52.6/license-apache-2.0, licenses/windows_i686_gnullvm-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_msvc 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_i686_msvc-0.42.2/license-apache-2.0, licenses/windows_i686_msvc-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_msvc 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_i686_msvc-0.52.6/license-apache-2.0, licenses/windows_i686_msvc-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnu 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnu-0.42.2/license-apache-2.0, licenses/windows_x86_64_gnu-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnu 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnu-0.52.6/license-apache-2.0, licenses/windows_x86_64_gnu-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnullvm 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnullvm-0.42.2/license-apache-2.0, licenses/windows_x86_64_gnullvm-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnullvm 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnullvm-0.52.6/license-apache-2.0, licenses/windows_x86_64_gnullvm-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_msvc 0.42.2 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_msvc-0.42.2/license-apache-2.0, licenses/windows_x86_64_msvc-0.42.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_msvc 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_msvc-0.52.6/license-apache-2.0, licenses/windows_x86_64_msvc-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### wit-bindgen 0.57.1 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/wit-bindgen-0.57.1/LICENSE-APACHE, licenses/wit-bindgen-0.57.1/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/wit-bindgen-0.57.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/wit-bindgen

#### writeable 0.6.4 — Unicode-3.0

- Files: licenses/writeable-0.6.4/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### xattr 1.6.1 — MIT OR Apache-2.0

- Files: licenses/xattr-1.6.1/LICENSE-APACHE, licenses/xattr-1.6.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Allen
- Repository: https://github.com/Stebalien/xattr

#### yoke 0.8.4 — Unicode-3.0

- Files: licenses/yoke-0.8.4/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### yoke-derive 0.8.4 — Unicode-3.0

- Files: licenses/yoke-derive-0.8.4/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zerofrom 0.1.8 — Unicode-3.0

- Files: licenses/zerofrom-0.1.8/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zerofrom-derive 0.1.8 — Unicode-3.0

- Files: licenses/zerofrom-derive-0.1.8/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zeroize 1.9.1 — Apache-2.0 OR MIT

- Files: licenses/zeroize-1.9.1/LICENSE-APACHE, licenses/zeroize-1.9.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### zerotrie 0.2.5 — Unicode-3.0

- Files: licenses/zerotrie-0.2.5/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zerovec 0.11.9 — Unicode-3.0

- Files: licenses/zerovec-0.11.9/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zerovec-derive 0.11.7 — Unicode-3.0

- Files: licenses/zerovec-derive-0.11.7/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zlib-rs 0.6.8 — Zlib

- Files: licenses/zlib-rs-0.6.8/LICENSE
- Copyright (LICENSE): (C) 2024 Trifecta Tech Foundation
- Repository: https://github.com/trifectatechfoundation/zlib-rs

#### zmij 1.0.23 — MIT

- Files: licenses/zmij-1.0.23/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/zmij

---

Bundle generated by `scripts/generate_notices.py` from the locked dependency graph. Do not edit by hand; regenerate instead.
