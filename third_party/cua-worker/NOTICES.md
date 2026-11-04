# Third-Party Notices — coorworker-cua

This file and the `licenses/` directory beside it constitute the distributable dependency notice bundle for this project.

## Scope

These notices cover the `coorworker-cua` worker binary built from `cua-worker/`, a separate package with its own lockfile. The daemon can supervise the standalone worker or link its library with `cua-desktop`. Distributing the worker requires reproducing this file and the `licenses/` tree beside it; the daemon's bundle in `third_party/` separately covers its embedded variant. Packaging steps are specified in [`../PACKAGING.md`](../PACKAGING.md).

Enumerated from the pinned dependency graph via cargo metadata --locked --offline --all-features: 502 packages total including this crate; the 501 third-party crates indexed below carry the remaining linked-license obligations. Regenerate with `python3 scripts/generate_notices.py`; verify a checkout with `python3 scripts/generate_notices.py --check`.

## This project (MIT)

coorworker-cua (crate `coorworker-cua` 0.1.0) is licensed under the MIT License; see [`LICENSE`](../../LICENSE) at the repository root.

Copyright (c) 2026 shon-yuan

## Vendored Cua Driver crates

`vendor/cua-driver/` carries the MIT-licensed Cua Driver crates used by the standalone `coorworker-cua` worker and the daemon's optional `cua-desktop` feature. The pinned upstream commit, local changes, vendored list, and deliberately excluded AGPL/FSL components are recorded in `vendor/cua-driver/PROVENANCE.md`. Those excluded components must not be shipped or linked. Covered distributions must include that file and the license texts enumerated below.

`vendor/cua-driver/rust/crates/cua-cursor-motion/` is a vendored upstream crate `cua-cursor-motion` 0.38.0 (MIT). Its license text is under `licenses/cua-cursor-motion-0.38.0/` and is also indexed below.

`vendor/cua-driver/rust/crates/cua-driver-contract/` is a vendored upstream crate `cua-driver-contract` 0.38.0 (MIT). Its license text is under `licenses/cua-driver-contract-0.38.0/` and is also indexed below.

`vendor/cua-driver/rust/crates/cua-driver-core/` is a vendored upstream crate `cua-driver-core` 0.38.0 (MIT). Its license text is under `licenses/cua-driver-core-0.38.0/` and is also indexed below.

`vendor/cua-driver/rust/crates/cursor-overlay/` is a vendored upstream crate `cursor-overlay` 0.38.0 (MIT). Its license text is under `licenses/cursor-overlay-0.38.0/` and is also indexed below.

`vendor/cua-driver/rust/crates/pip-preview/` is a vendored upstream crate `pip-preview` 0.38.0 (MIT). Its license text is under `licenses/pip-preview-0.38.0/` and is also indexed below.

`vendor/cua-driver/rust/crates/platform-linux/` is a vendored upstream crate `platform-linux` 0.38.0 (MIT). Its license text is under `licenses/platform-linux-0.38.0/` and is also indexed below.

`vendor/cua-driver/rust/crates/platform-windows/` is a vendored upstream crate `platform-windows` 0.38.0 (MIT). Its license text is under `licenses/platform-windows-0.38.0/` and is also indexed below.

## Third-party crates

Full license texts are copied verbatim under `licenses/<crate>-<version>/`. Copyright lines quoted per crate below are extracted from those files for convenience; where none is embedded in the license file(s), consult the full text.

### Index

| Crate                             | Version                       | License                                             | Files copied |
|-----------------------------------|-------------------------------|-----------------------------------------------------|---|
| adler2                            | 2.0.1                         | 0BSD OR MIT OR Apache-2.0                           | LICENSE-0BSD, LICENSE-APACHE, LICENSE-MIT |
| aead                              | 0.6.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| aes                               | 0.9.4                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ahash                             | 0.8.12                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| aho-corasick                      | 1.1.5                         | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| allocator-api2                    | 0.2.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| android_system_properties         | 0.1.6                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| anyhow                            | 1.0.104                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| apple-native-keyring-store        | 1.0.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| arrayref                          | 0.3.9                         | BSD-2-Clause                                        | LICENSE |
| arrayvec                          | 0.7.8                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ashpd                             | 0.13.13                       | MIT                                                 | LICENSE |
| askama                            | 0.14.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| askama_derive                     | 0.14.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| askama_parser                     | 0.14.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| async-broadcast                   | 0.7.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| async-channel                     | 2.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-executor                    | 1.14.1                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-io                          | 2.6.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-lock                        | 3.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-process                     | 2.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-recursion                   | 1.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| async-signal                      | 0.2.14                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-task                        | 4.7.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| async-trait                       | 0.1.92                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| atomic-polyfill                   | 1.0.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| atomic-waker                      | 1.1.2                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT, LICENSE-THIRD-PARTY |
| atspi                             | 0.30.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE2.txt, LICENSE-MIT.txt |
| atspi-common                      | 0.14.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE2.txt, LICENSE-MIT.txt |
| atspi-connection                  | 0.14.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE2.txt, LICENSE-MIT.txt |
| atspi-proxies                     | 0.14.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE2.txt, LICENSE-MIT.txt |
| autocfg                           | 1.5.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| axum                              | 0.8.9                         | MIT                                                 | LICENSE |
| axum-core                         | 0.5.6                         | MIT                                                 | LICENSE |
| base64                            | 0.22.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| basic-toml                        | 0.1.10                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| bit-set                           | 0.8.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| bit-vec                           | 0.8.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| bitflags                          | 1.3.2                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| bitflags                          | 2.13.2                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| bitvec                            | 1.1.1                         | MIT                                                 | LICENSE.txt |
| block-buffer                      | 0.10.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| block-buffer                      | 0.12.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| block-padding                     | 0.4.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| block2                            | 0.6.2                         | MIT                                                 | LICENSE-MIT |
| blocking                          | 1.7.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| borrow-or-share                   | 0.2.4                         | MIT-0                                               | LICENSE |
| bstr                              | 1.13.1                        | MIT OR Apache-2.0                                   | COPYING, LICENSE-APACHE, LICENSE-MIT |
| bumpalo                           | 3.20.3                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| bytecount                         | 0.6.9                         | Apache-2.0/MIT                                      | LICENSE.Apache2, LICENSE.MIT |
| bytemuck                          | 1.25.2                        | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE, LICENSE-MIT, LICENSE-ZLIB |
| byteorder                         | 1.5.0                         | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| byteorder-lite                    | 0.1.0                         | Unlicense OR MIT                                    | LICENSE-MIT, UNLICENSE |
| bytes                             | 1.12.1                        | MIT                                                 | LICENSE |
| camino                            | 1.2.6                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cargo-platform                    | 0.1.9                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cargo_metadata                    | 0.19.2                        | MIT                                                 | LICENSE-MIT |
| cbc                               | 0.2.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cc                                | 1.11.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cfg-if                            | 1.0.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| chacha20                          | 0.10.2                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| chacha20poly1305                  | 0.11.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| chrono                            | 0.4.45                        | MIT OR Apache-2.0                                   | LICENSE.txt |
| chrono-tz                         | 0.10.4                        | MIT OR Apache-2.0                                   | LICENSE |
| ciborium                          | 0.2.2                         | Apache-2.0                                          | LICENSE |
| ciborium-io                       | 0.2.2                         | Apache-2.0                                          | LICENSE |
| ciborium-ll                       | 0.2.2                         | Apache-2.0                                          | LICENSE |
| cipher                            | 0.5.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| clipboard-rs                      | 0.3.5                         | MIT                                                 | LICENSE |
| clipboard-win                     | 5.4.3                         | BSL-1.0                                             | LICENSE-BSL |
| cmov                              | 0.5.4                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| cobs                              | 0.3.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| concurrent-queue                  | 2.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| const-oid                         | 0.10.2                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| core-foundation                   | 0.10.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| core-foundation-sys               | 0.8.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| core_maths                        | 0.1.1                         | MIT                                                 | LICENSE |
| coset                             | 0.4.2                         | Apache-2.0                                          | AUTHORS, LICENSE |
| cpubits                           | 0.1.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cpufeatures                       | 0.2.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| cpufeatures                       | 0.3.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| crc32fast                         | 1.5.4                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| critical-section                  | 1.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| crossbeam-channel                 | 0.5.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT, LICENSE-THIRD-PARTY |
| crossbeam-utils                   | 0.8.23                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| crunchy                           | 0.2.4                         | MIT                                                 | LICENSE |
| crypto-common                     | 0.1.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| crypto-common                     | 0.2.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ctutils                           | 0.4.8                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| cua-cursor-motion                 | 0.38.0                        | MIT                                                 | LICENSE |
| cua-driver-contract               | 0.38.0                        | MIT                                                 | LICENSE |
| cua-driver-core                   | 0.38.0                        | MIT                                                 | LICENSE |
| cursor-overlay                    | 0.38.0                        | MIT                                                 | LICENSE |
| data-encoding                     | 2.11.1                        | MIT                                                 | LICENSE |
| deranged                          | 0.5.9                         | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| digest                            | 0.10.7                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| digest                            | 0.11.3                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| dirs                              | 5.0.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| dirs-sys                          | 0.4.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| dispatch2                         | 0.3.1                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| displaydoc                        | 0.2.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| downcast-rs                       | 1.2.1                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| dyn-clone                         | 1.0.20                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| email_address                     | 0.2.9                         | MIT                                                 | LICENSE |
| embedded-io                       | 0.4.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| embedded-io                       | 0.6.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| endi                              | 1.1.1                         | MIT                                                 | LICENSE-MIT |
| enumflags2                        | 0.7.12                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| enumflags2_derive                 | 0.7.12                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| equivalent                        | 1.0.3                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| errno                             | 0.3.14                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| error-code                        | 3.4.0                         | BSL-1.0                                             | LICENSE |
| evdev                             | 0.12.2                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| event-listener                    | 5.4.2                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| event-listener-strategy           | 0.5.4                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| fancy-regex                       | 0.18.0                        | MIT                                                 | AUTHORS, LICENSE |
| fastrand                          | 2.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| fax                               | 0.2.7                         | MIT                                                 | LICENSE |
| fdeflate                          | 0.3.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| find-msvc-tools                   | 0.1.15                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| fixedbitset                       | 0.5.7                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| flate2                            | 1.1.10                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| fluent-uri                        | 0.4.1                         | MIT                                                 | LICENSE |
| fnv                               | 1.0.7                         | Apache-2.0 / MIT                                    | LICENSE-APACHE, LICENSE-MIT |
| foldhash                          | 0.1.5                         | Zlib                                                | LICENSE |
| foldhash                          | 0.2.0                         | Zlib                                                | LICENSE |
| fontdue                           | 0.9.5                         | MIT OR Apache-2.0 OR Zlib                           | LICENSE-APACHE, LICENSE-MIT, LICENSE-ZLIB |
| form_urlencoded                   | 1.2.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| fraction                          | 0.15.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| fs-err                            | 2.11.0                        | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| fs2                               | 0.4.3                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| funty                             | 2.0.0                         | MIT                                                 | LICENSE.txt |
| futures-core                      | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-io                        | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-lite                      | 2.6.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT, LICENSE-THIRD-PARTY |
| futures-macro                     | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-sink                      | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-task                      | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| futures-util                      | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| generic-array                     | 0.14.7                        | MIT                                                 | LICENSE |
| gethostname                       | 1.1.0                         | Apache-2.0                                          | LICENSE |
| getrandom                         | 0.2.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| getrandom                         | 0.3.4                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| getrandom                         | 0.4.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| glob                              | 0.3.4                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| globset                           | 0.4.21                        | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| goblin                            | 0.8.2                         | MIT                                                 | LICENSE |
| half                              | 2.7.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hash32                            | 0.2.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hashbrown                         | 0.15.5                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hashbrown                         | 0.16.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hashbrown                         | 0.17.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| heapless                          | 0.7.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| heck                              | 0.5.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hermit-abi                        | 0.5.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hex                               | 0.4.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hkdf                              | 0.12.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hkdf                              | 0.13.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hmac                              | 0.12.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hmac                              | 0.13.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| http                              | 1.5.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| http-body                         | 1.1.0                         | MIT                                                 | LICENSE |
| http-body-util                    | 0.1.5                         | MIT                                                 | LICENSE |
| httparse                          | 1.10.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| httpdate                          | 1.0.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hybrid-array                      | 0.4.15                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| hyper                             | 1.12.0                        | MIT                                                 | LICENSE |
| hyper-util                        | 0.1.21                        | MIT                                                 | LICENSE |
| iana-time-zone                    | 0.1.65                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| iana-time-zone-haiku              | 0.1.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| icu_collections                   | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_locale_core                   | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_normalizer                    | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_normalizer_data               | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_properties                    | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_properties_data               | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| icu_provider                      | 2.4.0                         | Unicode-3.0                                         | LICENSE |
| idna                              | 1.1.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| idna_adapter                      | 1.2.2                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| image                             | 0.25.10                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| indexmap                          | 2.14.2                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| inout                             | 0.2.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ipnet                             | 2.12.2                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| itoa                              | 1.0.18                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| jobserver                         | 0.1.35                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| js-sys                            | 0.3.107                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| jsonschema                        | 0.46.10                       | MIT                                                 | LICENSE |
| jsonschema-regex                  | 0.46.10                       | MIT                                                 | LICENSE-MIT |
| keyring                           | 4.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| keyring-core                      | 1.0.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| lazy_static                       | 1.5.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| libc                              | 0.2.191                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| libm                              | 0.2.16                        | MIT                                                 | LICENSE.txt |
| libredox                          | 0.1.25                        | MIT                                                 | LICENSE |
| linux-raw-sys                     | 0.12.1                        | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | COPYRIGHT, LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| litemap                           | 0.8.3                         | Unicode-3.0                                         | LICENSE |
| lock_api                          | 0.4.14                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| log                               | 0.4.35                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| lru                               | 0.18.5                        | MIT                                                 | LICENSE |
| matchit                           | 0.8.4                         | MIT AND BSD-3-Clause                                | LICENSE, LICENSE.httprouter |
| memchr                            | 2.8.3                         | Unlicense OR MIT                                    | COPYING, LICENSE-MIT, UNLICENSE |
| memmap2                           | 0.9.12                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| memoffset                         | 0.6.5                         | MIT                                                 | LICENSE |
| memoffset                         | 0.9.1                         | MIT                                                 | LICENSE |
| meval                             | 0.2.0                         | Unlicense/MIT                                       | LICENSE-MIT, UNLICENSE |
| micromap                          | 0.3.0                         | MIT                                                 | LICENSE.txt |
| mime                              | 0.3.17                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| minimal-lexical                   | 0.2.1                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT, LICENSE.md |
| miniz_oxide                       | 0.8.9                         | MIT OR Zlib OR Apache-2.0                           | LICENSE, LICENSE-APACHE.md, LICENSE-MIT.md, LICENSE-ZLIB.md |
| miniz_oxide                       | 0.9.1                         | MIT OR Zlib OR Apache-2.0                           | LICENSE, LICENSE-APACHE.md, LICENSE-MIT.md, LICENSE-ZLIB.md |
| mio                               | 1.2.4                         | MIT                                                 | LICENSE |
| moxcms                            | 0.8.1                         | BSD-3-Clause OR Apache-2.0                          | LICENSE-APACHE.md, LICENSE.md |
| msvc_spectre_libs                 | 0.1.3                         | MIT OR Apache-2.0                                   | LICENSE |
| nix                               | 0.23.2                        | MIT                                                 | LICENSE |
| nom                               | 1.2.4                         | MIT                                                 | LICENSE |
| nom                               | 7.1.3                         | MIT                                                 | LICENSE |
| nom                               | 8.0.0                         | MIT                                                 | LICENSE |
| num                               | 0.4.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| num-bigint                        | 0.4.8                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| num-cmp                           | 0.1.0                         | MIT/Apache-2.0                                      | LICENSE.txt |
| num-complex                       | 0.4.6                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| num-conv                          | 0.2.2                         | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| num-integer                       | 0.1.47                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| num-iter                          | 0.1.46                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| num-rational                      | 0.4.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| num-traits                        | 0.2.19                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| objc2                             | 0.6.5                         | MIT                                                 | LICENSE-MIT |
| objc2-app-kit                     | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-cloud-kit                   | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-data                   | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-foundation             | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-graphics               | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-image                  | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-location               | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-text                   | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-core-video                  | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-encode                      | 4.1.0                         | MIT                                                 | LICENSE-MIT |
| objc2-foundation                  | 0.3.2                         | MIT                                                 | LICENSE-MIT |
| objc2-io-surface                  | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-quartz-core                 | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-ui-kit                      | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| objc2-user-notifications          | 0.3.2                         | Zlib OR Apache-2.0 OR MIT                           | LICENSE-APACHE |
| once_cell                         | 1.21.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| option-ext                        | 0.2.0                         | MPL-2.0                                             | LICENSE.txt |
| ordered-stream                    | 0.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| os_pipe                           | 1.2.3                         | MIT                                                 | LICENSE |
| outref                            | 0.5.2                         | MIT                                                 | LICENSE |
| parking                           | 2.2.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT, LICENSE-THIRD-PARTY |
| parking_lot                       | 0.12.5                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| parking_lot_core                  | 0.9.12                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| percent-encoding                  | 2.3.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| petgraph                          | 0.8.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| phf                               | 0.12.1                        | MIT                                                 | LICENSE |
| phf_shared                        | 0.12.1                        | MIT                                                 | LICENSE |
| pin-project-lite                  | 0.2.17                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| pip-preview                       | 0.38.0                        | MIT                                                 | LICENSE |
| piper                             | 0.2.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| pkg-config                        | 0.3.34                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| plain                             | 0.2.3                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| platform-linux                    | 0.38.0                        | MIT                                                 | LICENSE |
| platform-windows                  | 0.38.0                        | MIT                                                 | LICENSE |
| png                               | 0.18.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| polling                           | 3.11.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| poly1305                          | 0.9.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| postcard                          | 1.1.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| potential_utf                     | 0.1.6                         | Unicode-3.0                                         | LICENSE |
| powerfmt                          | 0.2.1                         | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| ppv-lite86                        | 0.2.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| proc-macro-crate                  | 3.5.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| proc-macro2                       | 1.0.107                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| pxfm                              | 0.1.30                        | BSD-3-Clause OR Apache-2.0                          | LICENSE-APACHE.md, LICENSE.md |
| quick-error                       | 2.0.1                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| quick-xml                         | 0.41.0                        | MIT                                                 | LICENSE-MIT.md |
| quote                             | 1.0.47                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| r-efi                             | 5.3.0                         | MIT OR Apache-2.0 OR LGPL-2.1-or-later              | AUTHORS |
| r-efi                             | 6.0.0                         | MIT OR Apache-2.0 OR LGPL-2.1-or-later              | AUTHORS |
| radium                            | 0.7.0                         | MIT                                                 | LICENSE.txt |
| rand                              | 0.10.3                        | MIT OR Apache-2.0                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| rand                              | 0.8.8                         | MIT OR Apache-2.0                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| rand_chacha                       | 0.3.1                         | MIT OR Apache-2.0                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| rand_core                         | 0.10.1                        | MIT OR Apache-2.0                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| rand_core                         | 0.6.4                         | MIT OR Apache-2.0                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| redox_syscall                     | 0.5.18                        | MIT                                                 | LICENSE |
| redox_users                       | 0.4.6                         | MIT                                                 | LICENSE |
| ref-cast                          | 1.0.27                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ref-cast-impl                     | 1.0.27                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| referencing                       | 0.46.10                       | MIT                                                 | LICENSE |
| regex                             | 1.13.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| regex-automata                    | 0.4.18                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| regex-syntax                      | 0.8.11                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| regorus                           | 0.10.1                        | MIT AND Apache-2.0 AND BSD-3-Clause                 | LICENSE |
| relative-path                     | 2.0.3                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| rquickjs                          | 0.14.0                        | MIT                                                 | LICENSE |
| rquickjs-core                     | 0.14.0                        | MIT                                                 | LICENSE-MIT |
| rquickjs-sys                      | 0.14.0                        | MIT                                                 | LICENSE-MIT |
| rustc-hash                        | 2.1.3                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| rustc_version                     | 0.4.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| rustix                            | 1.1.6                         | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | COPYRIGHT, LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| rustversion                       | 1.0.23                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| ryu                               | 1.0.23                        | Apache-2.0 OR BSL-1.0                               | LICENSE-APACHE, LICENSE-BOOST |
| schemars                          | 1.2.3                         | MIT                                                 | LICENSE |
| schemars_derive                   | 1.2.3                         | MIT                                                 | LICENSE |
| scopeguard                        | 1.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| scroll                            | 0.12.0                        | MIT                                                 | LICENSE |
| scroll_derive                     | 0.12.1                        | MIT                                                 | LICENSE |
| secret-service                    | 5.2.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| security-framework                | 3.7.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| security-framework-sys            | 2.17.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| semver                            | 1.0.28                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde                             | 1.0.229                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_core                        | 1.0.229                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_derive                      | 1.0.229                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_derive_internals            | 0.30.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_json                        | 1.0.154                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_path_to_error               | 0.1.20                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_repr                        | 0.1.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_spanned                     | 1.1.2                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_urlencoded                  | 0.7.1                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| serde_yaml                        | 0.9.34+deprecated             | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| serde_yaml_ng                     | 0.10.0                        | MIT                                                 | LICENSE |
| sha1                              | 0.10.7                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| sha2                              | 0.10.9                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| sha2                              | 0.11.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| shlex                             | 2.0.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| signal-hook-registry              | 1.4.8                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| simd-adler32                      | 0.3.10                        | MIT                                                 | LICENSE.md |
| siphasher                         | 1.0.4                         | MIT OR Apache-2.0                                   | COPYING, LICENSE-APACHE, LICENSE-MIT |
| slab                              | 0.4.12                        | MIT                                                 | LICENSE |
| smallvec                          | 1.16.4                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| smawk                             | 0.3.3                         | MIT                                                 | LICENSE |
| socket2                           | 0.6.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| spin                              | 0.10.1                        | MIT                                                 | LICENSE |
| spin                              | 0.9.9                         | MIT                                                 | LICENSE |
| stable_deref_trait                | 1.2.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| static_assertions                 | 1.1.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| strict-num                        | 0.1.1                         | MIT                                                 | LICENSE |
| subtle                            | 2.6.1                         | BSD-3-Clause                                        | LICENSE |
| syn                               | 2.0.119                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| syn                               | 3.0.9                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| sync_wrapper                      | 1.0.2                         | Apache-2.0                                          | LICENSE |
| synstructure                      | 0.14.0                        | MIT                                                 | LICENSE |
| tap                               | 1.0.1                         | MIT                                                 | LICENSE.txt |
| tempfile                          | 3.27.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| textwrap                          | 0.16.4                        | MIT                                                 | LICENSE |
| thiserror                         | 1.0.69                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| thiserror                         | 2.0.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| thiserror-impl                    | 1.0.69                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| thiserror-impl                    | 2.0.21                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| tiff                              | 0.11.3                        | MIT                                                 | LICENSE |
| time                              | 0.3.55                        | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| time-core                         | 0.1.9                         | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| time-macros                       | 0.2.32                        | MIT OR Apache-2.0                                   | LICENSE-Apache, LICENSE-MIT |
| tiny-skia                         | 0.11.4                        | BSD-3-Clause                                        | LICENSE |
| tiny-skia-path                    | 0.11.4                        | BSD-3-Clause                                        | LICENSE |
| tinystr                           | 0.8.5                         | Unicode-3.0                                         | LICENSE |
| tokio                             | 1.54.1                        | MIT                                                 | LICENSE |
| tokio-macros                      | 2.7.2                         | MIT                                                 | LICENSE |
| tokio-tungstenite                 | 0.24.0                        | MIT                                                 | LICENSE |
| toml                              | 0.9.12+spec-1.1.0             | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| toml_datetime                     | 0.7.5+spec-1.1.0              | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| toml_datetime                     | 1.1.2+spec-1.1.0              | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| toml_edit                         | 0.25.18+spec-1.1.0            | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| toml_parser                       | 1.1.5+spec-1.1.0              | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| toml_writer                       | 1.2.0+spec-1.1.0              | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| tower                             | 0.5.3                         | MIT                                                 | LICENSE |
| tower-layer                       | 0.3.3                         | MIT                                                 | LICENSE |
| tower-service                     | 0.3.3                         | MIT                                                 | LICENSE |
| tracing                           | 0.1.44                        | MIT                                                 | LICENSE |
| tracing-attributes                | 0.1.31                        | MIT                                                 | LICENSE |
| tracing-core                      | 0.1.36                        | MIT                                                 | LICENSE |
| tree_magic_mini                   | 3.2.2                         | MIT                                                 | LICENSE |
| ttf-parser                        | 0.25.1                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| tungstenite                       | 0.24.0                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| typenum                           | 1.20.1                        | MIT OR Apache-2.0                                   | LICENSE, LICENSE-APACHE, LICENSE-MIT |
| uds_windows                       | 1.2.1                         | MIT                                                 | LICENSE |
| unicode-general-category          | 1.1.0                         | Apache-2.0                                          | LICENSE |
| unicode-ident                     | 1.0.27                        | (MIT OR Apache-2.0) AND Unicode-3.0                 | LICENSE-APACHE, LICENSE-MIT, LICENSE-UNICODE |
| unicode-width                     | 0.2.3                         | MIT OR Apache-2.0                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| uniffi                            | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_bindgen                    | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_core                       | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_internal_macros            | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_macros                     | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_meta                       | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_pipeline                   | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| uniffi_udl                        | 0.31.0                        | MPL-2.0                                             | LICENSE-MPL |
| universal-hash                    | 0.6.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| unsafe-libyaml                    | 0.2.11                        | MIT                                                 | LICENSE-MIT |
| url                               | 2.5.8                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| utf-8                             | 0.7.6                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| utf8_iter                         | 1.1.0                         | Apache-2.0 OR MIT                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| utf_types                         | 1.0.0                         | Apache-2.0 OR MIT                                   | COPYRIGHT, LICENSE-APACHE, LICENSE-MIT |
| uuid                              | 1.29.0                        | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| uuid-simd                         | 0.8.0                         | MIT                                                 | LICENSE-MIT |
| version_check                     | 0.9.5                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| vsimd                             | 0.8.0                         | MIT                                                 | LICENSE-MIT |
| wasi                              | 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| wasip2                            | 1.0.4+wasi-0.2.12             | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| wasm-bindgen                      | 0.2.130                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-macro                | 0.2.130                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-macro-support        | 0.2.130                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wasm-bindgen-shared               | 0.2.130                       | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| wayland-backend                   | 0.3.17                        | MIT                                                 | LICENSE.txt |
| wayland-client                    | 0.31.15                       | MIT                                                 | LICENSE.txt |
| wayland-protocols                 | 0.32.13                       | MIT                                                 | LICENSE.txt |
| wayland-protocols-wlr             | 0.3.12                        | MIT                                                 | LICENSE.txt |
| wayland-scanner                   | 0.31.11                       | MIT                                                 | LICENSE.txt |
| wayland-sys                       | 0.31.11                       | MIT                                                 | LICENSE.txt |
| weedle2                           | 5.0.0                         | MIT                                                 | LICENSE.md |
| weezl                             | 0.1.12                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| winapi                            | 0.3.9                         | MIT/Apache-2.0                                      | LICENSE-APACHE, LICENSE-MIT |
| winapi-i686-pc-windows-gnu        | 0.4.0                         | MIT/Apache-2.0                                      | LICENSE-APACHE |
| winapi-x86_64-pc-windows-gnu      | 0.4.0                         | MIT/Apache-2.0                                      | LICENSE-APACHE |
| windows                           | 0.58.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows                           | 0.59.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-core                      | 0.58.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-core                      | 0.59.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-core                      | 0.62.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-implement                 | 0.58.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-implement                 | 0.59.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-implement                 | 0.60.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-interface                 | 0.58.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-interface                 | 0.59.3                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-link                      | 0.1.3                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-link                      | 0.2.1                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-native-keyring-store      | 1.1.0                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| windows-result                    | 0.2.0                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-result                    | 0.3.4                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-result                    | 0.4.1                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-strings                   | 0.1.0                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-strings                   | 0.3.1                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-strings                   | 0.5.1                         | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-sys                       | 0.48.0                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-sys                       | 0.61.2                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-targets                   | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-targets                   | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-targets                   | 0.53.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows-win                       | 3.0.0                         | BSL-1.0                                             | LICENSE |
| windows_aarch64_gnullvm           | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_gnullvm           | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_gnullvm           | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_msvc              | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_msvc              | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_aarch64_msvc              | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnu                  | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnu                  | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnu                  | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnullvm              | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_gnullvm              | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_msvc                 | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_msvc                 | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_i686_msvc                 | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnu                | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnu                | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnu                | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnullvm            | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnullvm            | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_gnullvm            | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_msvc               | 0.48.5                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_msvc               | 0.52.6                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| windows_x86_64_msvc               | 0.53.1                        | MIT OR Apache-2.0                                   | license-apache-2.0, license-mit |
| winnow                            | 0.7.15                        | MIT                                                 | LICENSE-MIT |
| winnow                            | 1.0.4                         | MIT                                                 | LICENSE-MIT |
| wit-bindgen                       | 0.57.1                        | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | LICENSE-APACHE, LICENSE-Apache-2.0_WITH_LLVM-exception, LICENSE-MIT |
| wl-clipboard-rs                   | 0.9.5                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| writeable                         | 0.6.4                         | Unicode-3.0                                         | LICENSE |
| wyz                               | 0.5.1                         | MIT                                                 | LICENSE.txt |
| x11                               | 2.21.0                        | MIT                                                 | LICENSE-MIT |
| x11rb                             | 0.13.2                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| x11rb-protocol                    | 0.13.2                        | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| xkbcommon                         | 0.9.0                         | MIT                                                 | LICENSE |
| xkeysym                           | 0.2.1                         | MIT OR Apache-2.0 OR Zlib                           | LICENSE-APACHE, LICENSE-MIT, LICENSE-ZLIB |
| yoke                              | 0.8.4                         | Unicode-3.0                                         | LICENSE |
| yoke-derive                       | 0.8.4                         | Unicode-3.0                                         | LICENSE |
| zbus                              | 5.19.0                        | MIT                                                 | LICENSE |
| zbus-lockstep                     | 0.5.2                         | MIT                                                 | LICENSE-MIT |
| zbus-lockstep-macros              | 0.5.2                         | MIT                                                 | LICENSE-MIT |
| zbus-secret-service-keyring-store | 1.0.1                         | MIT OR Apache-2.0                                   | LICENSE-APACHE, LICENSE-MIT |
| zbus_macros                       | 5.19.0                        | MIT                                                 | LICENSE |
| zbus_names                        | 4.3.5                         | MIT                                                 | LICENSE |
| zbus_xml                          | 5.2.1                         | MIT                                                 | LICENSE |
| zcheapstr                         | 1.2.0                         | MIT                                                 | LICENSE |
| zerocopy                          | 0.8.63                        | BSD-2-Clause OR Apache-2.0 OR MIT                   | LICENSE-APACHE, LICENSE-BSD, LICENSE-MIT |
| zerocopy-derive                   | 0.8.63                        | BSD-2-Clause OR Apache-2.0 OR MIT                   | LICENSE-APACHE, LICENSE-BSD, LICENSE-MIT |
| zerofrom                          | 0.1.8                         | Unicode-3.0                                         | LICENSE |
| zerofrom-derive                   | 0.1.8                         | Unicode-3.0                                         | LICENSE |
| zeroize                           | 1.9.1                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| zeroize_derive                    | 1.5.0                         | Apache-2.0 OR MIT                                   | LICENSE-APACHE, LICENSE-MIT |
| zerotrie                          | 0.2.5                         | Unicode-3.0                                         | LICENSE |
| zerovec                           | 0.11.9                        | Unicode-3.0                                         | LICENSE |
| zerovec-derive                    | 0.11.7                        | Unicode-3.0                                         | LICENSE |
| zlib-rs                           | 0.6.8                         | Zlib                                                | LICENSE |
| zmij                              | 1.0.23                        | MIT                                                 | LICENSE-MIT |
| zstd                              | 0.13.4                        | BSD-3-Clause                                        | LICENSE |
| zstd-safe                         | 7.4.0                         | BSD-3-Clause                                        | LICENSE |
| zstd-sys                          | 2.1.2+zstd.1.5.7              | BSD-3-Clause                                        | LICENSE, LICENSE.BSD-3-Clause |
| zune-core                         | 0.5.1                         | MIT OR Apache-2.0 OR Zlib                           | LICENSE-APACHE, LICENSE-MIT, LICENSE-ZLIB |
| zune-jpeg                         | 0.5.15                        | MIT OR Apache-2.0 OR Zlib                           | LICENSE-APACHE, LICENSE-MIT, LICENSE-ZLIB |
| zvariant                          | 5.15.1                        | MIT                                                 | LICENSE |
| zvariant_derive                   | 5.15.1                        | MIT                                                 | LICENSE |
| zvariant_utils                    | 4.2.0                         | MIT                                                 | LICENSE |

### Per-crate notices

#### adler2 2.0.1 — 0BSD OR MIT OR Apache-2.0

- Files: licenses/adler2-2.0.1/LICENSE-0BSD, licenses/adler2-2.0.1/LICENSE-APACHE, licenses/adler2-2.0.1/LICENSE-MIT
- Copyright (LICENSE-0BSD): Copyright (C) Jonas Schievink <jonasschievink@gmail.com>
- Repository: https://github.com/oyvindln/adler2

#### aead 0.6.1 — MIT OR Apache-2.0

- Files: licenses/aead-0.6.1/LICENSE-APACHE, licenses/aead-0.6.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019-2026 The RustCrypto Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2019 MobileCoin, LLC
- Repository: https://github.com/RustCrypto/traits

#### aes 0.9.4 — MIT OR Apache-2.0

- Files: licenses/aes-0.9.4/LICENSE-APACHE, licenses/aes-0.9.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2024 The RustCrypto Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2018 Artyom Pavlov
- Repository: https://github.com/RustCrypto/block-ciphers

#### ahash 0.8.12 — MIT OR Apache-2.0

- Files: licenses/ahash-0.8.12/LICENSE-APACHE, licenses/ahash-0.8.12/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 Tom Kaitchuck
- Repository: https://github.com/tkaitchuck/ahash

#### aho-corasick 1.1.5 — Unlicense OR MIT

- Files: licenses/aho-corasick-1.1.5/COPYING, licenses/aho-corasick-1.1.5/LICENSE-MIT, licenses/aho-corasick-1.1.5/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/aho-corasick

#### allocator-api2 0.2.21 — MIT OR Apache-2.0

- Files: licenses/allocator-api2-0.2.21/LICENSE-APACHE, licenses/allocator-api2-0.2.21/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/zakarumych/allocator-api2

#### android_system_properties 0.1.6 — MIT OR Apache-2.0

- Files: licenses/android_system_properties-0.1.6/LICENSE-APACHE, licenses/android_system_properties-0.1.6/LICENSE-MIT
- Copyright (LICENSE-APACHE): Copyright 2016 Nicolas Silva
- Copyright (LICENSE-MIT): Copyright (c) 2013 Nicolas Silva
- Repository: https://github.com/nical/android_system_properties

#### anyhow 1.0.104 — MIT OR Apache-2.0

- Files: licenses/anyhow-1.0.104/LICENSE-APACHE, licenses/anyhow-1.0.104/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/anyhow

#### apple-native-keyring-store 1.0.2 — MIT OR Apache-2.0

- Files: licenses/apple-native-keyring-store-1.0.2/LICENSE-APACHE, licenses/apple-native-keyring-store-1.0.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 keyring Developers
- Repository: https://github.com/open-source-cooperative/apple-native-keyring-store.git

#### arrayref 0.3.9 — BSD-2-Clause

- Files: licenses/arrayref-0.3.9/LICENSE
- Copyright (LICENSE): Copyright (c) 2015 David Roundy <roundyd@physics.oregonstate.edu>
- Repository: https://github.com/droundy/arrayref

#### arrayvec 0.7.8 — MIT OR Apache-2.0

- Files: licenses/arrayvec-0.7.8/LICENSE-APACHE, licenses/arrayvec-0.7.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Ulrik Sverdrup "bluss" 2015-2023
- Repository: https://github.com/bluss/arrayvec

#### ashpd 0.13.13 — MIT

- Files: licenses/ashpd-0.13.13/LICENSE
- Copyright (LICENSE): Copyright (c) 2020 Bilal Elmoussaoui
- Repository: https://github.com/bilelmoussaoui/ashpd

#### askama 0.14.0 — MIT OR Apache-2.0

- Files: licenses/askama-0.14.0/LICENSE-APACHE, licenses/askama-0.14.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2020 Dirkjan Ochtman
- Repository: https://github.com/askama-rs/askama

#### askama_derive 0.14.0 — MIT OR Apache-2.0

- Files: licenses/askama_derive-0.14.0/LICENSE-APACHE, licenses/askama_derive-0.14.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2020 Dirkjan Ochtman
- Repository: https://github.com/askama-rs/askama

#### askama_parser 0.14.0 — MIT OR Apache-2.0

- Files: licenses/askama_parser-0.14.0/LICENSE-APACHE, licenses/askama_parser-0.14.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2020 Dirkjan Ochtman
- Repository: https://github.com/askama-rs/askama

#### async-broadcast 0.7.2 — MIT OR Apache-2.0

- Files: licenses/async-broadcast-0.7.2/LICENSE-APACHE, licenses/async-broadcast-0.7.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020 Yoshua Wuyts
- Repository: https://github.com/smol-rs/async-broadcast

#### async-channel 2.5.0 — Apache-2.0 OR MIT

- Files: licenses/async-channel-2.5.0/LICENSE-APACHE, licenses/async-channel-2.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-channel

#### async-executor 1.14.1 — Apache-2.0 OR MIT

- Files: licenses/async-executor-1.14.1/LICENSE-APACHE, licenses/async-executor-1.14.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-executor

#### async-io 2.6.0 — Apache-2.0 OR MIT

- Files: licenses/async-io-2.6.0/LICENSE-APACHE, licenses/async-io-2.6.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-io

#### async-lock 3.5.0 — Apache-2.0 OR MIT

- Files: licenses/async-lock-3.5.0/LICENSE-APACHE, licenses/async-lock-3.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-lock

#### async-process 2.5.0 — Apache-2.0 OR MIT

- Files: licenses/async-process-2.5.0/LICENSE-APACHE, licenses/async-process-2.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-process

#### async-recursion 1.2.0 — MIT OR Apache-2.0

- Files: licenses/async-recursion-1.2.0/LICENSE-APACHE, licenses/async-recursion-1.2.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dcchut/async-recursion

#### async-signal 0.2.14 — Apache-2.0 OR MIT

- Files: licenses/async-signal-0.2.14/LICENSE-APACHE, licenses/async-signal-0.2.14/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-signal

#### async-task 4.7.1 — Apache-2.0 OR MIT

- Files: licenses/async-task-4.7.1/LICENSE-APACHE, licenses/async-task-4.7.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/async-task

#### async-trait 0.1.92 — MIT OR Apache-2.0

- Files: licenses/async-trait-0.1.92/LICENSE-APACHE, licenses/async-trait-0.1.92/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/async-trait

#### atomic-polyfill 1.0.3 — MIT OR Apache-2.0

- Files: licenses/atomic-polyfill-1.0.3/LICENSE-APACHE, licenses/atomic-polyfill-1.0.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020 Dario Nieuwenhuis
- Repository: https://github.com/embassy-rs/atomic-polyfill

#### atomic-waker 1.1.2 — Apache-2.0 OR MIT

- Files: licenses/atomic-waker-1.1.2/LICENSE-APACHE, licenses/atomic-waker-1.1.2/LICENSE-MIT, licenses/atomic-waker-1.1.2/LICENSE-THIRD-PARTY
- Copyright (LICENSE-THIRD-PARTY): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-THIRD-PARTY): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/smol-rs/atomic-waker

#### atspi 0.30.0 — Apache-2.0 OR MIT

- Files: licenses/atspi-0.30.0/LICENSE-APACHE2.txt, licenses/atspi-0.30.0/LICENSE-MIT.txt
- Copyright (LICENSE-MIT.txt): Copyright (c) 2022 Tait Hoyem <tait@tait.tech>
- Repository: https://github.com/odilia-app/atspi

#### atspi-common 0.14.0 — Apache-2.0 OR MIT

- Files: licenses/atspi-common-0.14.0/LICENSE-APACHE2.txt, licenses/atspi-common-0.14.0/LICENSE-MIT.txt
- Copyright (LICENSE-MIT.txt): Copyright (c) 2022 Tait Hoyem <tait@tait.tech>
- Repository: https://github.com/odilia-app/atspi

#### atspi-connection 0.14.0 — Apache-2.0 OR MIT

- Files: licenses/atspi-connection-0.14.0/LICENSE-APACHE2.txt, licenses/atspi-connection-0.14.0/LICENSE-MIT.txt
- Copyright (LICENSE-MIT.txt): Copyright (c) 2022 Tait Hoyem <tait@tait.tech>
- Repository: https://github.com/odilia-app/atspi/

#### atspi-proxies 0.14.0 — Apache-2.0 OR MIT

- Files: licenses/atspi-proxies-0.14.0/LICENSE-APACHE2.txt, licenses/atspi-proxies-0.14.0/LICENSE-MIT.txt
- Copyright (LICENSE-MIT.txt): Copyright (c) 2022 Tait Hoyem <tait@tait.tech>
- Repository: https://github.com/odilia-app/atspi

#### autocfg 1.5.1 — Apache-2.0 OR MIT

- Files: licenses/autocfg-1.5.1/LICENSE-APACHE, licenses/autocfg-1.5.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 Josh Stone
- Repository: https://github.com/cuviper/autocfg

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

#### basic-toml 0.1.10 — MIT OR Apache-2.0

- Files: licenses/basic-toml-0.1.10/LICENSE-APACHE, licenses/basic-toml-0.1.10/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/basic-toml

#### bit-set 0.8.0 — Apache-2.0 OR MIT

- Files: licenses/bit-set-0.8.0/LICENSE-APACHE, licenses/bit-set-0.8.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2023 The Rust Project Developers
- Repository: https://github.com/contain-rs/bit-set

#### bit-vec 0.8.0 — Apache-2.0 OR MIT

- Files: licenses/bit-vec-0.8.0/LICENSE-APACHE, licenses/bit-vec-0.8.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2023 The Rust Project Developers
- Repository: https://github.com/contain-rs/bit-vec

#### bitflags 1.3.2 — MIT/Apache-2.0

- Files: licenses/bitflags-1.3.2/LICENSE-APACHE, licenses/bitflags-1.3.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/bitflags/bitflags

#### bitflags 2.13.2 — MIT OR Apache-2.0

- Files: licenses/bitflags-2.13.2/LICENSE-APACHE, licenses/bitflags-2.13.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/bitflags/bitflags

#### bitvec 1.1.1 — MIT

- Files: licenses/bitvec-1.1.1/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2018 myrrlyn (Alexander Payne)
- Repository: https://github.com/bitvecto-rs/bitvec

#### block-buffer 0.10.4 — MIT OR Apache-2.0

- Files: licenses/block-buffer-0.10.4/LICENSE-APACHE, licenses/block-buffer-0.10.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2019 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### block-buffer 0.12.1 — MIT OR Apache-2.0

- Files: licenses/block-buffer-0.12.1/LICENSE-APACHE, licenses/block-buffer-0.12.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2025 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### block-padding 0.4.2 — MIT OR Apache-2.0

- Files: licenses/block-padding-0.4.2/LICENSE-APACHE, licenses/block-padding-0.4.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2025 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### block2 0.6.2 — MIT

- Files: licenses/block2-0.6.2/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### blocking 1.7.0 — Apache-2.0 OR MIT

- Files: licenses/blocking-1.7.0/LICENSE-APACHE, licenses/blocking-1.7.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/blocking

#### borrow-or-share 0.2.4 — MIT-0

- Files: licenses/borrow-or-share-0.2.4/LICENSE
- Copyright (LICENSE): Copyright 2024 Scallop Ye
- Repository: https://github.com/yescallop/borrow-or-share

#### bstr 1.13.1 — MIT OR Apache-2.0

- Files: licenses/bstr-1.13.1/COPYING, licenses/bstr-1.13.1/LICENSE-APACHE, licenses/bstr-1.13.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2019 Andrew Gallant
- Repository: https://github.com/BurntSushi/bstr

#### bumpalo 3.20.3 — MIT OR Apache-2.0

- Files: licenses/bumpalo-3.20.3/LICENSE-APACHE, licenses/bumpalo-3.20.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019 Nick Fitzgerald
- Repository: https://github.com/fitzgen/bumpalo

#### bytecount 0.6.9 — Apache-2.0/MIT

- Files: licenses/bytecount-0.6.9/LICENSE.Apache2, licenses/bytecount-0.6.9/LICENSE.MIT
- Copyright (LICENSE.MIT): Copyright (c) 2017 The bytecount Developers
- Repository: https://github.com/llogiq/bytecount

#### bytemuck 1.25.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/bytemuck-1.25.2/LICENSE-APACHE, licenses/bytemuck-1.25.2/LICENSE-MIT, licenses/bytemuck-1.25.2/LICENSE-ZLIB
- Copyright (LICENSE-MIT): Copyright (c) 2019 Daniel "Lokathor" Gee.
- Copyright (LICENSE-ZLIB): Copyright (c) 2019 Daniel "Lokathor" Gee.
- Repository: https://github.com/Lokathor/bytemuck

#### byteorder 1.5.0 — Unlicense OR MIT

- Files: licenses/byteorder-1.5.0/COPYING, licenses/byteorder-1.5.0/LICENSE-MIT, licenses/byteorder-1.5.0/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/byteorder

#### byteorder-lite 0.1.0 — Unlicense OR MIT

- Files: licenses/byteorder-lite-0.1.0/LICENSE-MIT, licenses/byteorder-lite-0.1.0/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/image-rs/byteorder-lite

#### bytes 1.12.1 — MIT

- Files: licenses/bytes-1.12.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2018 Carl Lerche
- Repository: https://github.com/tokio-rs/bytes

#### camino 1.2.6 — MIT OR Apache-2.0

- Files: licenses/camino-1.2.6/LICENSE-APACHE, licenses/camino-1.2.6/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/camino-rs/camino

#### cargo-platform 0.1.9 — MIT OR Apache-2.0

- Files: licenses/cargo-platform-0.1.9/LICENSE-APACHE, licenses/cargo-platform-0.1.9/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/rust-lang/cargo

#### cargo_metadata 0.19.2 — MIT

- Files: licenses/cargo_metadata-0.19.2/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/oli-obk/cargo_metadata

#### cbc 0.2.1 — MIT OR Apache-2.0

- Files: licenses/cbc-0.2.1/LICENSE-APACHE, licenses/cbc-0.2.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2022 RustCrypto Developers
- Copyright (LICENSE-MIT): Copyright (c) 2018 Artyom Pavlov
- Repository: https://github.com/RustCrypto/block-modes

#### cc 1.11.0 — MIT OR Apache-2.0

- Files: licenses/cc-1.11.0/LICENSE-APACHE, licenses/cc-1.11.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/cc-rs

#### cfg-if 1.0.5 — MIT OR Apache-2.0

- Files: licenses/cfg-if-1.0.5/LICENSE-APACHE, licenses/cfg-if-1.0.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/cfg-if

#### chacha20 0.10.2 — MIT OR Apache-2.0

- Files: licenses/chacha20-0.10.2/LICENSE-APACHE, licenses/chacha20-0.10.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/stream-ciphers

#### chacha20poly1305 0.11.0 — Apache-2.0 OR MIT

- Files: licenses/chacha20poly1305-0.11.0/LICENSE-APACHE, licenses/chacha20poly1305-0.11.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/AEADs

#### chrono 0.4.45 — MIT OR Apache-2.0

- Files: licenses/chrono-0.4.45/LICENSE.txt
- Copyright (LICENSE.txt): Apache 2.0 License [2]. Copyright (c) 2014--2026, Kang Seonghoon and
- Repository: https://github.com/chronotope/chrono

#### chrono-tz 0.10.4 — MIT OR Apache-2.0

- Files: licenses/chrono-tz-0.10.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2016-2024 Benjamin Sago & the chronotope maintainers
- Repository: https://github.com/chronotope/chrono-tz

#### ciborium 0.2.2 — Apache-2.0

- Files: licenses/ciborium-0.2.2/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/enarx/ciborium

#### ciborium-io 0.2.2 — Apache-2.0

- Files: licenses/ciborium-io-0.2.2/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/enarx/ciborium

#### ciborium-ll 0.2.2 — Apache-2.0

- Files: licenses/ciborium-ll-0.2.2/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/enarx/ciborium

#### cipher 0.5.2 — MIT OR Apache-2.0

- Files: licenses/cipher-0.5.2/LICENSE-APACHE, licenses/cipher-0.5.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016-2025 RustCrypto Developers
- Repository: https://github.com/RustCrypto/traits

#### clipboard-rs 0.3.5 — MIT

- Files: licenses/clipboard-rs-0.3.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 桃树夭
- Repository: https://github.com/ChurchTao/clipboard-rs

#### clipboard-win 5.4.3 — BSL-1.0

- Files: licenses/clipboard-win-5.4.3/LICENSE-BSL
- License text: canonical BSL-1.0 text from the SPDX license list (scripts/license-overrides/BSL-1.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/DoumanAsh/clipboard-win

#### cmov 0.5.4 — Apache-2.0 OR MIT

- Files: licenses/cmov-0.5.4/LICENSE-APACHE, licenses/cmov-0.5.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2022-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### cobs 0.3.0 — MIT OR Apache-2.0

- Files: licenses/cobs-0.3.0/LICENSE-APACHE, licenses/cobs-0.3.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 The cobs.rs Developers
- Repository: https://github.com/jamesmunns/cobs.rs

#### concurrent-queue 2.5.0 — Apache-2.0 OR MIT

- Files: licenses/concurrent-queue-2.5.0/LICENSE-APACHE, licenses/concurrent-queue-2.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/concurrent-queue

#### const-oid 0.10.2 — Apache-2.0 OR MIT

- Files: licenses/const-oid-0.10.2/LICENSE-APACHE, licenses/const-oid-0.10.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/formats

#### core-foundation 0.10.1 — MIT OR Apache-2.0

- Files: licenses/core-foundation-0.10.1/LICENSE-APACHE, licenses/core-foundation-0.10.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2012-2013 Mozilla Foundation
- Repository: https://github.com/servo/core-foundation-rs

#### core-foundation-sys 0.8.7 — MIT OR Apache-2.0

- Files: licenses/core-foundation-sys-0.8.7/LICENSE-APACHE, licenses/core-foundation-sys-0.8.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2012-2013 Mozilla Foundation
- Repository: https://github.com/servo/core-foundation-rs

#### core_maths 0.1.1 — MIT

- Files: licenses/core_maths-0.1.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Robert Bastian
- Repository: https://github.com/robertbastian/core_maths

#### coset 0.4.2 — Apache-2.0

- Files: licenses/coset-0.4.2/AUTHORS, licenses/coset-0.4.2/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/google/coset

#### cpubits 0.1.1 — MIT OR Apache-2.0

- Files: licenses/cpubits-0.1.1/LICENSE-APACHE, licenses/cpubits-0.1.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2023-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### cpufeatures 0.2.17 — MIT OR Apache-2.0

- Files: licenses/cpufeatures-0.2.17/LICENSE-APACHE, licenses/cpufeatures-0.2.17/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020-2025 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### cpufeatures 0.3.1 — MIT OR Apache-2.0

- Files: licenses/cpufeatures-0.3.1/LICENSE-APACHE, licenses/cpufeatures-0.3.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### crc32fast 1.5.4 — MIT OR Apache-2.0

- Files: licenses/crc32fast-1.5.4/LICENSE-APACHE, licenses/crc32fast-1.5.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 Sam Rijs, Alex Crichton and contributors
- Repository: https://github.com/srijs/rust-crc32fast

#### critical-section 1.2.0 — MIT OR Apache-2.0

- Files: licenses/critical-section-1.2.0/LICENSE-APACHE, licenses/critical-section-1.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2022 The critical-section authors
- Repository: https://github.com/rust-embedded/critical-section

#### crossbeam-channel 0.5.17 — MIT OR Apache-2.0

- Files: licenses/crossbeam-channel-0.5.17/LICENSE-APACHE, licenses/crossbeam-channel-0.5.17/LICENSE-MIT, licenses/crossbeam-channel-0.5.17/LICENSE-THIRD-PARTY
- Copyright (LICENSE-MIT): Copyright (c) 2019 The Crossbeam Project Developers
- Repository: https://github.com/crossbeam-rs/crossbeam

#### crossbeam-utils 0.8.23 — MIT OR Apache-2.0

- Files: licenses/crossbeam-utils-0.8.23/LICENSE-APACHE, licenses/crossbeam-utils-0.8.23/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019 The Crossbeam Project Developers
- Repository: https://github.com/crossbeam-rs/crossbeam

#### crunchy 0.2.4 — MIT

- Files: licenses/crunchy-0.2.4/LICENSE
- Copyright (LICENSE): Copyright 2017-2023 Eira Fransham.
- Repository: https://github.com/eira-fransham/crunchy

#### crypto-common 0.1.7 — MIT OR Apache-2.0

- Files: licenses/crypto-common-0.1.7/LICENSE-APACHE, licenses/crypto-common-0.1.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2021 RustCrypto Developers
- Repository: https://github.com/RustCrypto/traits

#### crypto-common 0.2.2 — MIT OR Apache-2.0

- Files: licenses/crypto-common-0.2.2/LICENSE-APACHE, licenses/crypto-common-0.2.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2021-2026 RustCrypto Developers
- Repository: https://github.com/RustCrypto/traits

#### ctutils 0.4.8 — Apache-2.0 OR MIT

- Files: licenses/ctutils-0.4.8/LICENSE-APACHE, licenses/ctutils-0.4.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2025-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/utils

#### cua-cursor-motion 0.38.0 — MIT

- Files: licenses/cua-cursor-motion-0.38.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.
- Repository: https://github.com/trycua/cua

#### cua-driver-contract 0.38.0 — MIT

- Files: licenses/cua-driver-contract-0.38.0/LICENSE
- License text: upstream repository license, reproduced byte-for-byte; the published crate ships no per-crate file (see vendor/cua-driver/PROVENANCE.md)
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.

#### cua-driver-core 0.38.0 — MIT

- Files: licenses/cua-driver-core-0.38.0/LICENSE
- License text: upstream repository license, reproduced byte-for-byte; the published crate ships no per-crate file (see vendor/cua-driver/PROVENANCE.md)
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.

#### cursor-overlay 0.38.0 — MIT

- Files: licenses/cursor-overlay-0.38.0/LICENSE
- License text: upstream repository license, reproduced byte-for-byte; the published crate ships no per-crate file (see vendor/cua-driver/PROVENANCE.md)
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.

#### data-encoding 2.11.1 — MIT

- Files: licenses/data-encoding-2.11.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2015-2020 Julien Cretin
- Copyright (LICENSE): Copyright (c) 2017-2020 Google Inc.
- Repository: https://github.com/ia0/data-encoding

#### deranged 0.5.9 — MIT OR Apache-2.0

- Files: licenses/deranged-0.5.9/LICENSE-Apache, licenses/deranged-0.5.9/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2024 Jacob Pratt et al.
- Repository: https://github.com/jhpratt/deranged

#### digest 0.10.7 — MIT OR Apache-2.0

- Files: licenses/digest-0.10.7/LICENSE-APACHE, licenses/digest-0.10.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Artyom Pavlov
- Repository: https://github.com/RustCrypto/traits

#### digest 0.11.3 — MIT OR Apache-2.0

- Files: licenses/digest-0.11.3/LICENSE-APACHE, licenses/digest-0.11.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2025 RustCrypto Developers
- Copyright (LICENSE-MIT): Copyright (c) 2017 Artyom Pavlov
- Repository: https://github.com/RustCrypto/traits

#### dirs 5.0.1 — MIT OR Apache-2.0

- Files: licenses/dirs-5.0.1/LICENSE-APACHE, licenses/dirs-5.0.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2019 dirs-rs contributors
- Repository: https://github.com/soc/dirs-rs

#### dirs-sys 0.4.1 — MIT OR Apache-2.0

- Files: licenses/dirs-sys-0.4.1/LICENSE-APACHE, licenses/dirs-sys-0.4.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2019 dirs-rs contributors
- Repository: https://github.com/dirs-dev/dirs-sys-rs

#### dispatch2 0.3.1 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/dispatch2-0.3.1/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### displaydoc 0.2.7 — MIT OR Apache-2.0

- Files: licenses/displaydoc-0.2.7/LICENSE-APACHE, licenses/displaydoc-0.2.7/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/yaahc/displaydoc

#### downcast-rs 1.2.1 — MIT/Apache-2.0

- Files: licenses/downcast-rs-1.2.1/LICENSE-APACHE, licenses/downcast-rs-1.2.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020 Ashish Myles and contributors
- Repository: https://github.com/marcianx/downcast-rs

#### dyn-clone 1.0.20 — MIT OR Apache-2.0

- Files: licenses/dyn-clone-1.0.20/LICENSE-APACHE, licenses/dyn-clone-1.0.20/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/dyn-clone

#### email_address 0.2.9 — MIT

- Files: licenses/email_address-0.2.9/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Simon Johnston
- Repository: https://github.com/johnstonskj/rust-email_address.git

#### embedded-io 0.4.0 — MIT OR Apache-2.0

- Files: licenses/embedded-io-0.4.0/LICENSE-APACHE, licenses/embedded-io-0.4.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2022 The embedded-io authors
- Repository: https://github.com/embassy-rs/embedded-io

#### embedded-io 0.6.1 — MIT OR Apache-2.0

- Files: licenses/embedded-io-0.6.1/LICENSE-APACHE, licenses/embedded-io-0.6.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2023 The embedded-io authors
- Repository: https://github.com/rust-embedded/embedded-hal

#### endi 1.1.1 — MIT

- Files: licenses/endi-1.1.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/zeenix/endi

#### enumflags2 0.7.12 — MIT OR Apache-2.0

- Files: licenses/enumflags2-0.7.12/LICENSE-APACHE, licenses/enumflags2-0.7.12/LICENSE-MIT
- Copyright (LICENSE-APACHE): Copyright 2017-2023 Maik Klein, Maja Kądziołka
- Copyright (LICENSE-MIT): Copyright (c) 2017-2023 Maik Klein, Maja Kądziołka
- Repository: https://github.com/meithecatte/enumflags2

#### enumflags2_derive 0.7.12 — MIT OR Apache-2.0

- Files: licenses/enumflags2_derive-0.7.12/LICENSE-APACHE, licenses/enumflags2_derive-0.7.12/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Maik Klein
- Repository: https://github.com/meithecatte/enumflags2

#### equivalent 1.0.3 — Apache-2.0 OR MIT

- Files: licenses/equivalent-1.0.3/LICENSE-APACHE, licenses/equivalent-1.0.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016--2023
- Repository: https://github.com/indexmap-rs/equivalent

#### errno 0.3.14 — MIT OR Apache-2.0

- Files: licenses/errno-0.3.14/LICENSE-APACHE, licenses/errno-0.3.14/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Chris Wong
- Repository: https://github.com/lambda-fairy/rust-errno

#### error-code 3.4.0 — BSL-1.0

- Files: licenses/error-code-3.4.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/DoumanAsh/error-code

#### evdev 0.12.2 — Apache-2.0 OR MIT

- Files: licenses/evdev-0.12.2/LICENSE-APACHE, licenses/evdev-0.12.2/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/cmr/evdev

#### event-listener 5.4.2 — Apache-2.0 OR MIT

- Files: licenses/event-listener-5.4.2/LICENSE-APACHE, licenses/event-listener-5.4.2/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/event-listener

#### event-listener-strategy 0.5.4 — Apache-2.0 OR MIT

- Files: licenses/event-listener-strategy-0.5.4/LICENSE-APACHE, licenses/event-listener-strategy-0.5.4/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/event-listener-strategy

#### fancy-regex 0.18.0 — MIT

- Files: licenses/fancy-regex-0.18.0/AUTHORS, licenses/fancy-regex-0.18.0/LICENSE
- Copyright (LICENSE): Copyright 2015 The Fancy Regex Authors.
- Repository: https://github.com/fancy-regex/fancy-regex

#### fastrand 2.5.0 — Apache-2.0 OR MIT

- Files: licenses/fastrand-2.5.0/LICENSE-APACHE, licenses/fastrand-2.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/fastrand

#### fax 0.2.7 — MIT

- Files: licenses/fax-0.2.7/LICENSE
- Copyright (LICENSE): Copyright © 2021 The pdf-rs contributers.
- Repository: https://github.com/pdf-rs/fax

#### fdeflate 0.3.7 — MIT OR Apache-2.0

- Files: licenses/fdeflate-0.3.7/LICENSE-APACHE, licenses/fdeflate-0.3.7/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/image-rs/fdeflate

#### find-msvc-tools 0.1.15 — MIT OR Apache-2.0

- Files: licenses/find-msvc-tools-0.1.15/LICENSE-APACHE, licenses/find-msvc-tools-0.1.15/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/cc-rs

#### fixedbitset 0.5.7 — MIT OR Apache-2.0

- Files: licenses/fixedbitset-0.5.7/LICENSE-APACHE, licenses/fixedbitset-0.5.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2017
- Repository: https://github.com/petgraph/fixedbitset

#### flate2 1.1.10 — MIT OR Apache-2.0

- Files: licenses/flate2-1.1.10/LICENSE-APACHE, licenses/flate2-1.1.10/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014-2026 Alex Crichton
- Repository: https://github.com/rust-lang/flate2-rs

#### fluent-uri 0.4.1 — MIT

- Files: licenses/fluent-uri-0.4.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Scallop Ye
- Repository: https://github.com/yescallop/fluent-uri-rs

#### fnv 1.0.7 — Apache-2.0 / MIT

- Files: licenses/fnv-1.0.7/LICENSE-APACHE, licenses/fnv-1.0.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Contributors
- Repository: https://github.com/servo/rust-fnv

#### foldhash 0.1.5 — Zlib

- Files: licenses/foldhash-0.1.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Orson Peters
- Repository: https://github.com/orlp/foldhash

#### foldhash 0.2.0 — Zlib

- Files: licenses/foldhash-0.2.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Orson Peters
- Repository: https://github.com/orlp/foldhash

#### fontdue 0.9.5 — MIT OR Apache-2.0 OR Zlib

- Files: licenses/fontdue-0.9.5/LICENSE-APACHE, licenses/fontdue-0.9.5/LICENSE-MIT, licenses/fontdue-0.9.5/LICENSE-ZLIB
- Copyright (LICENSE-MIT): Copyright (c) 2019 Joe C (mooman219)
- Copyright (LICENSE-ZLIB): Copyright (c) 2019 Joe C (mooman219)
- Repository: https://github.com/mooman219/fontdue

#### form_urlencoded 1.2.2 — MIT OR Apache-2.0

- Files: licenses/form_urlencoded-1.2.2/LICENSE-APACHE, licenses/form_urlencoded-1.2.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2016 The rust-url developers
- Repository: https://github.com/servo/rust-url

#### fraction 0.15.4 — MIT OR Apache-2.0

- Files: licenses/fraction-0.15.4/LICENSE-APACHE, licenses/fraction-0.15.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/dnsl48/fraction.git

#### fs-err 2.11.0 — MIT/Apache-2.0

- Files: licenses/fs-err-2.11.0/LICENSE-APACHE, licenses/fs-err-2.11.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/andrewhickman/fs-err

#### fs2 0.4.3 — MIT/Apache-2.0

- Files: licenses/fs2-0.4.3/LICENSE-APACHE, licenses/fs2-0.4.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 The Rust Project Developers
- Repository: https://github.com/danburkert/fs2-rs

#### funty 2.0.0 — MIT

- Files: licenses/funty-2.0.0/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2020 myrrlyn (Alexander Payne)
- Repository: https://github.com/myrrlyn/funty

#### futures-core 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-core-0.3.34/LICENSE-APACHE, licenses/futures-core-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-io 0.3.34 — MIT OR Apache-2.0

- Files: licenses/futures-io-0.3.34/LICENSE-APACHE, licenses/futures-io-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-MIT): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/rust-lang/futures-rs

#### futures-lite 2.6.1 — Apache-2.0 OR MIT

- Files: licenses/futures-lite-2.6.1/LICENSE-APACHE, licenses/futures-lite-2.6.1/LICENSE-MIT, licenses/futures-lite-2.6.1/LICENSE-THIRD-PARTY
- Copyright (LICENSE-THIRD-PARTY): Copyright (c) 2016 Alex Crichton
- Copyright (LICENSE-THIRD-PARTY): Copyright (c) 2017 The Tokio Authors
- Repository: https://github.com/smol-rs/futures-lite

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

#### gethostname 1.1.0 — Apache-2.0

- Files: licenses/gethostname-1.1.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://codeberg.org/swsnr/gethostname.rs.git

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

#### getrandom 0.4.3 — MIT OR Apache-2.0

- Files: licenses/getrandom-0.4.3/LICENSE-APACHE, licenses/getrandom-0.4.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2026 The rust-random Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/getrandom

#### glob 0.3.4 — MIT OR Apache-2.0

- Files: licenses/glob-0.3.4/LICENSE-APACHE, licenses/glob-0.3.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-lang/glob

#### globset 0.4.21 — Unlicense OR MIT

- Files: licenses/globset-0.4.21/COPYING, licenses/globset-0.4.21/LICENSE-MIT, licenses/globset-0.4.21/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/ripgrep/tree/master/crates/globset

#### goblin 0.8.2 — MIT

- Files: licenses/goblin-0.8.2/LICENSE
- Copyright (LICENSE): Copyright (c) m4b 2016-2024
- Repository: https://github.com/m4b/goblin

#### half 2.7.1 — MIT OR Apache-2.0

- Files: licenses/half-2.7.1/LICENSE-APACHE, licenses/half-2.7.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/VoidStarKat/half-rs

#### hash32 0.2.1 — MIT OR Apache-2.0

- Files: licenses/hash32-0.2.1/LICENSE-APACHE, licenses/hash32-0.2.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 Jorge Aparicio
- Repository: https://github.com/japaric/hash32

#### hashbrown 0.15.5 — MIT OR Apache-2.0

- Files: licenses/hashbrown-0.15.5/LICENSE-APACHE, licenses/hashbrown-0.15.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Amanieu d'Antras
- Repository: https://github.com/rust-lang/hashbrown

#### hashbrown 0.16.1 — MIT OR Apache-2.0

- Files: licenses/hashbrown-0.16.1/LICENSE-APACHE, licenses/hashbrown-0.16.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Amanieu d'Antras
- Repository: https://github.com/rust-lang/hashbrown

#### hashbrown 0.17.1 — MIT OR Apache-2.0

- Files: licenses/hashbrown-0.17.1/LICENSE-APACHE, licenses/hashbrown-0.17.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Amanieu d'Antras
- Repository: https://github.com/rust-lang/hashbrown

#### heapless 0.7.17 — MIT OR Apache-2.0

- Files: licenses/heapless-0.7.17/LICENSE-APACHE, licenses/heapless-0.7.17/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Jorge Aparicio
- Repository: https://github.com/japaric/heapless

#### heck 0.5.0 — MIT OR Apache-2.0

- Files: licenses/heck-0.5.0/LICENSE-APACHE, licenses/heck-0.5.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 The Rust Project Developers
- Repository: https://github.com/withoutboats/heck

#### hermit-abi 0.5.3 — MIT OR Apache-2.0

- Files: licenses/hermit-abi-0.5.3/LICENSE-APACHE, licenses/hermit-abi-0.5.3/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/hermit-os/hermit-rs

#### hex 0.4.3 — MIT OR Apache-2.0

- Files: licenses/hex-0.4.3/LICENSE-APACHE, licenses/hex-0.4.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2014 The Rust Project Developers.
- Copyright (LICENSE-MIT): Copyright (c) 2015-2020 The rust-hex Developers
- Repository: https://github.com/KokaKiwi/rust-hex

#### hkdf 0.12.4 — MIT OR Apache-2.0

- Files: licenses/hkdf-0.12.4/LICENSE-APACHE, licenses/hkdf-0.12.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2018 Vlad Filippov
- Copyright (LICENSE-MIT): Copyright (c) 2018-2021 RustCrypto Developers
- Repository: https://github.com/RustCrypto/KDFs/

#### hkdf 0.13.0 — MIT OR Apache-2.0

- Files: licenses/hkdf-0.13.0/LICENSE-APACHE, licenses/hkdf-0.13.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2018 Vlad Filippov
- Copyright (LICENSE-MIT): Copyright (c) 2018-2021 RustCrypto Developers
- Repository: https://github.com/RustCrypto/KDFs/

#### hmac 0.12.1 — MIT OR Apache-2.0

- Files: licenses/hmac-0.12.1/LICENSE-APACHE, licenses/hmac-0.12.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Artyom Pavlov
- Repository: https://github.com/RustCrypto/MACs

#### hmac 0.13.0 — MIT OR Apache-2.0

- Files: licenses/hmac-0.13.0/LICENSE-APACHE, licenses/hmac-0.13.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Artyom Pavlov
- Repository: https://github.com/RustCrypto/MACs

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

#### httparse 1.10.1 — MIT OR Apache-2.0

- Files: licenses/httparse-1.10.1/LICENSE-APACHE, licenses/httparse-1.10.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2025 Sean McArthur
- Repository: https://github.com/seanmonstar/httparse

#### httpdate 1.0.3 — MIT OR Apache-2.0

- Files: licenses/httpdate-1.0.3/LICENSE-APACHE, licenses/httpdate-1.0.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Pyfisch
- Repository: https://github.com/pyfisch/httpdate

#### hybrid-array 0.4.15 — MIT OR Apache-2.0

- Files: licenses/hybrid-array-0.4.15/LICENSE-APACHE, licenses/hybrid-array-0.4.15/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2022-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/hybrid-array

#### hyper 1.12.0 — MIT

- Files: licenses/hyper-1.12.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2014-2026 Sean McArthur
- Repository: https://github.com/hyperium/hyper

#### hyper-util 0.1.21 — MIT

- Files: licenses/hyper-util-0.1.21/LICENSE
- Copyright (LICENSE): Copyright (c) 2023-2025 Sean McArthur
- Repository: https://github.com/hyperium/hyper-util

#### iana-time-zone 0.1.65 — MIT OR Apache-2.0

- Files: licenses/iana-time-zone-0.1.65/LICENSE-APACHE, licenses/iana-time-zone-0.1.65/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020 Andrew D. Straw
- Repository: https://github.com/strawlab/iana-time-zone

#### iana-time-zone-haiku 0.1.2 — MIT OR Apache-2.0

- Files: licenses/iana-time-zone-haiku-0.1.2/LICENSE-APACHE, licenses/iana-time-zone-haiku-0.1.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020 Andrew D. Straw
- Repository: https://github.com/strawlab/iana-time-zone

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

#### image 0.25.10 — MIT OR Apache-2.0

- Files: licenses/image-0.25.10/LICENSE-APACHE, licenses/image-0.25.10/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/image-rs/image

#### indexmap 2.14.2 — Apache-2.0 OR MIT

- Files: licenses/indexmap-2.14.2/LICENSE-APACHE, licenses/indexmap-2.14.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016--2017
- Repository: https://github.com/indexmap-rs/indexmap

#### inout 0.2.5 — MIT OR Apache-2.0

- Files: licenses/inout-0.2.5/LICENSE-APACHE, licenses/inout-0.2.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2022-2025 The RustCrypto Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2022 Artyom Pavlov
- Repository: https://github.com/RustCrypto/utils

#### ipnet 2.12.2 — MIT OR Apache-2.0

- Files: licenses/ipnet-2.12.2/LICENSE-APACHE, licenses/ipnet-2.12.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2017 Juniper Networks, Inc.
- Repository: https://github.com/krisprice/ipnet

#### itoa 1.0.18 — MIT OR Apache-2.0

- Files: licenses/itoa-1.0.18/LICENSE-APACHE, licenses/itoa-1.0.18/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/itoa

#### jobserver 0.1.35 — MIT OR Apache-2.0

- Files: licenses/jobserver-0.1.35/LICENSE-APACHE, licenses/jobserver-0.1.35/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/jobserver-rs

#### js-sys 0.3.107 — MIT OR Apache-2.0

- Files: licenses/js-sys-0.3.107/LICENSE-APACHE, licenses/js-sys-0.3.107/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/js-sys

#### jsonschema 0.46.10 — MIT

- Files: licenses/jsonschema-0.46.10/LICENSE
- Copyright (LICENSE): Copyright (c) 2020-2026 Dmitry Dygalo
- Repository: https://github.com/Stranger6667/jsonschema

#### jsonschema-regex 0.46.10 — MIT

- Files: licenses/jsonschema-regex-0.46.10/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/Stranger6667/jsonschema

#### keyring 4.2.0 — MIT OR Apache-2.0

- Files: licenses/keyring-4.2.0/LICENSE-APACHE, licenses/keyring-4.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 keyring Developers
- Repository: https://github.com/open-source-cooperative/keyring-rs

#### keyring-core 1.0.1 — MIT OR Apache-2.0

- Files: licenses/keyring-core-1.0.1/LICENSE-APACHE, licenses/keyring-core-1.0.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 keyring Developers
- Repository: https://github.com/open-source-cooperative/keyring-core.git

#### lazy_static 1.5.1 — MIT OR Apache-2.0

- Files: licenses/lazy_static-1.5.1/LICENSE-APACHE, licenses/lazy_static-1.5.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/rust-lang-nursery/lazy-static.rs

#### libc 0.2.191 — MIT OR Apache-2.0

- Files: licenses/libc-0.2.191/LICENSE-APACHE, licenses/libc-0.2.191/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) The Rust Project Developers
- Repository: https://github.com/rust-lang/libc

#### libm 0.2.16 — MIT

- Files: licenses/libm-0.2.16/LICENSE.txt
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/rust-lang/compiler-builtins

#### libredox 0.1.25 — MIT

- Files: licenses/libredox-0.1.25/LICENSE
- Copyright (LICENSE): Copyright (c) 2023 4lDO2
- Repository: https://gitlab.redox-os.org/redox-os/libredox.git

#### linux-raw-sys 0.12.1 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/linux-raw-sys-0.12.1/COPYRIGHT, licenses/linux-raw-sys-0.12.1/LICENSE-APACHE, licenses/linux-raw-sys-0.12.1/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/linux-raw-sys-0.12.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/sunfishcode/linux-raw-sys

#### litemap 0.8.3 — Unicode-3.0

- Files: licenses/litemap-0.8.3/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### lock_api 0.4.14 — MIT OR Apache-2.0

- Files: licenses/lock_api-0.4.14/LICENSE-APACHE, licenses/lock_api-0.4.14/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 The Rust Project Developers
- Repository: https://github.com/Amanieu/parking_lot

#### log 0.4.35 — MIT OR Apache-2.0

- Files: licenses/log-0.4.35/LICENSE-APACHE, licenses/log-0.4.35/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-lang/log

#### lru 0.18.5 — MIT

- Files: licenses/lru-0.18.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 Jerome Froelich
- Repository: https://github.com/jeromefroe/lru-rs.git

#### matchit 0.8.4 — MIT AND BSD-3-Clause

- Files: licenses/matchit-0.8.4/LICENSE, licenses/matchit-0.8.4/LICENSE.httprouter
- Copyright (LICENSE): Copyright (c) 2022 Ibraheem Ahmed
- Copyright (LICENSE.httprouter): Copyright (c) 2013, Julien Schmidt
- Repository: https://github.com/ibraheemdev/matchit

#### memchr 2.8.3 — Unlicense OR MIT

- Files: licenses/memchr-2.8.3/COPYING, licenses/memchr-2.8.3/LICENSE-MIT, licenses/memchr-2.8.3/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Andrew Gallant
- Repository: https://github.com/BurntSushi/memchr

#### memmap2 0.9.12 — MIT OR Apache-2.0

- Files: licenses/memmap2-0.9.12/LICENSE-APACHE, licenses/memmap2-0.9.12/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2020 Yevhenii Reizner
- Copyright (LICENSE-MIT): Copyright (c) 2015 Dan Burkert
- Repository: https://github.com/RazrFalcon/memmap2-rs

#### memoffset 0.6.5 — MIT

- Files: licenses/memoffset-0.6.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Gilad Naaman
- Repository: https://github.com/Gilnaa/memoffset

#### memoffset 0.9.1 — MIT

- Files: licenses/memoffset-0.9.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Gilad Naaman
- Repository: https://github.com/Gilnaa/memoffset

#### meval 0.2.0 — Unlicense/MIT

- Files: licenses/meval-0.2.0/LICENSE-MIT, licenses/meval-0.2.0/UNLICENSE
- Copyright (LICENSE-MIT): Copyright (c) 2015 Norbert Pozar
- Repository: https://github.com/rekka/meval-rs

#### micromap 0.3.0 — MIT

- Files: licenses/micromap-0.3.0/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2023-2026 Yegor Bugayenko
- Repository: https://github.com/yegor256/micromap

#### mime 0.3.17 — MIT OR Apache-2.0

- Files: licenses/mime-0.3.17/LICENSE-APACHE, licenses/mime-0.3.17/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Sean McArthur
- Repository: https://github.com/hyperium/mime

#### minimal-lexical 0.2.1 — MIT/Apache-2.0

- Files: licenses/minimal-lexical-0.2.1/LICENSE-APACHE, licenses/minimal-lexical-0.2.1/LICENSE-MIT, licenses/minimal-lexical-0.2.1/LICENSE.md
- Copyright (LICENSE.md): Copyright (c) 2009 The Go Authors. All rights reserved.
- Repository: https://github.com/Alexhuszagh/minimal-lexical

#### miniz_oxide 0.8.9 — MIT OR Zlib OR Apache-2.0

- Files: licenses/miniz_oxide-0.8.9/LICENSE, licenses/miniz_oxide-0.8.9/LICENSE-APACHE.md, licenses/miniz_oxide-0.8.9/LICENSE-MIT.md, licenses/miniz_oxide-0.8.9/LICENSE-ZLIB.md
- Copyright (LICENSE): Copyright 2013-2014 RAD Game Tools and Valve Software
- Copyright (LICENSE): Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC
- Copyright (LICENSE): Copyright (c) 2017 Frommi
- ... (+1 more copyright line(s) in LICENSE; see full text)
- Copyright (LICENSE-MIT.md): Copyright 2013-2014 RAD Game Tools and Valve Software
- Copyright (LICENSE-MIT.md): Copyright 2010-2014 Rich Geldreich and Tenacious Software LLC
- Copyright (LICENSE-MIT.md): Copyright (c) 2017 Frommi
- ... (+1 more copyright line(s) in LICENSE-MIT.md; see full text)
- Repository: https://github.com/Frommi/miniz_oxide/tree/master/miniz_oxide

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

#### moxcms 0.8.1 — BSD-3-Clause OR Apache-2.0

- Files: licenses/moxcms-0.8.1/LICENSE-APACHE.md, licenses/moxcms-0.8.1/LICENSE.md
- Copyright (LICENSE.md): Copyright (c) Radzivon Bartoshyk. All rights reserved.
- Repository: https://github.com/awxkee/moxcms.git

#### msvc_spectre_libs 0.1.3 — MIT OR Apache-2.0

- Files: licenses/msvc_spectre_libs-0.1.3/LICENSE
- Copyright (LICENSE): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/rust_msvc_spectre_libs

#### nix 0.23.2 — MIT

- Files: licenses/nix-0.23.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2015 Carl Lerche + nix-rust Authors
- Repository: https://github.com/nix-rust/nix

#### nom 1.2.4 — MIT

- Files: licenses/nom-1.2.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2015 Geoffroy Couprie
- Repository: https://github.com/Geal/nom

#### nom 7.1.3 — MIT

- Files: licenses/nom-7.1.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2014-2019 Geoffroy Couprie
- Repository: https://github.com/Geal/nom

#### nom 8.0.0 — MIT

- Files: licenses/nom-8.0.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2014-2019 Geoffroy Couprie
- Repository: https://github.com/rust-bakery/nom

#### num 0.4.3 — MIT OR Apache-2.0

- Files: licenses/num-0.4.3/LICENSE-APACHE, licenses/num-0.4.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num

#### num-bigint 0.4.8 — MIT OR Apache-2.0

- Files: licenses/num-bigint-0.4.8/LICENSE-APACHE, licenses/num-bigint-0.4.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num-bigint

#### num-cmp 0.1.0 — MIT/Apache-2.0

- Files: licenses/num-cmp-0.1.0/LICENSE.txt
- Copyright (LICENSE.txt): Apache 2.0 License [3]. Copyright (c) 2017, Kang Seonghoon.
- Repository: https://github.com/lifthrasiir/num-cmp

#### num-complex 0.4.6 — MIT OR Apache-2.0

- Files: licenses/num-complex-0.4.6/LICENSE-APACHE, licenses/num-complex-0.4.6/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num-complex

#### num-conv 0.2.2 — MIT OR Apache-2.0

- Files: licenses/num-conv-0.2.2/LICENSE-Apache, licenses/num-conv-0.2.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Jacob Pratt
- Repository: https://github.com/jhpratt/num-conv

#### num-integer 0.1.47 — MIT OR Apache-2.0

- Files: licenses/num-integer-0.1.47/LICENSE-APACHE, licenses/num-integer-0.1.47/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num-integer

#### num-iter 0.1.46 — MIT OR Apache-2.0

- Files: licenses/num-iter-0.1.46/LICENSE-APACHE, licenses/num-iter-0.1.46/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num-iter

#### num-rational 0.4.2 — MIT OR Apache-2.0

- Files: licenses/num-rational-0.4.2/LICENSE-APACHE, licenses/num-rational-0.4.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num-rational

#### num-traits 0.2.19 — MIT OR Apache-2.0

- Files: licenses/num-traits-0.2.19/LICENSE-APACHE, licenses/num-traits-0.2.19/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-num/num-traits

#### objc2 0.6.5 — MIT

- Files: licenses/objc2-0.6.5/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-app-kit 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-app-kit-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-cloud-kit 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-cloud-kit-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-data 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-data-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-foundation 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-foundation-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-graphics 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-graphics-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-image 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-image-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-location 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-location-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-text 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-text-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-core-video 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-core-video-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-encode 4.1.0 — MIT

- Files: licenses/objc2-encode-4.1.0/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-foundation 0.3.2 — MIT

- Files: licenses/objc2-foundation-0.3.2/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-io-surface 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-io-surface-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-quartz-core 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-quartz-core-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-ui-kit 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-ui-kit-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### objc2-user-notifications 0.3.2 — Zlib OR Apache-2.0 OR MIT

- Files: licenses/objc2-user-notifications-0.3.2/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/madsmtm/objc2

#### once_cell 1.21.4 — MIT OR Apache-2.0

- Files: licenses/once_cell-1.21.4/LICENSE-APACHE, licenses/once_cell-1.21.4/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/matklad/once_cell

#### option-ext 0.2.0 — MPL-2.0

- Files: licenses/option-ext-0.2.0/LICENSE.txt
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/soc/option-ext.git

#### ordered-stream 0.2.0 — MIT OR Apache-2.0

- Files: licenses/ordered-stream-0.2.0/LICENSE-APACHE, licenses/ordered-stream-0.2.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/danieldg/ordered-stream

#### os_pipe 1.2.3 — MIT

- Files: licenses/os_pipe-1.2.3/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/oconnor663/os_pipe.rs

#### outref 0.5.2 — MIT

- Files: licenses/outref-0.5.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2022 Nugine
- Repository: https://github.com/Nugine/outref

#### parking 2.2.1 — Apache-2.0 OR MIT

- Files: licenses/parking-2.2.1/LICENSE-APACHE, licenses/parking-2.2.1/LICENSE-MIT, licenses/parking-2.2.1/LICENSE-THIRD-PARTY
- Copyright (LICENSE-THIRD-PARTY): Copyright 2014-2020 The Rust Project Developers
- Repository: https://github.com/smol-rs/parking

#### parking_lot 0.12.5 — MIT OR Apache-2.0

- Files: licenses/parking_lot-0.12.5/LICENSE-APACHE, licenses/parking_lot-0.12.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 The Rust Project Developers
- Repository: https://github.com/Amanieu/parking_lot

#### parking_lot_core 0.9.12 — MIT OR Apache-2.0

- Files: licenses/parking_lot_core-0.9.12/LICENSE-APACHE, licenses/parking_lot_core-0.9.12/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 The Rust Project Developers
- Repository: https://github.com/Amanieu/parking_lot

#### percent-encoding 2.3.2 — MIT OR Apache-2.0

- Files: licenses/percent-encoding-2.3.2/LICENSE-APACHE, licenses/percent-encoding-2.3.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2025 The rust-url developers
- Repository: https://github.com/servo/rust-url/

#### petgraph 0.8.3 — MIT OR Apache-2.0

- Files: licenses/petgraph-0.8.3/LICENSE-APACHE, licenses/petgraph-0.8.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015
- Repository: https://github.com/petgraph/petgraph

#### phf 0.12.1 — MIT

- Files: licenses/phf-0.12.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2014-2022 Steven Fackler, Yuki Okushi
- Repository: https://github.com/rust-phf/rust-phf

#### phf_shared 0.12.1 — MIT

- Files: licenses/phf_shared-0.12.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2014-2022 Steven Fackler, Yuki Okushi
- Repository: https://github.com/rust-phf/rust-phf

#### pin-project-lite 0.2.17 — Apache-2.0 OR MIT

- Files: licenses/pin-project-lite-0.2.17/LICENSE-APACHE, licenses/pin-project-lite-0.2.17/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/taiki-e/pin-project-lite

#### pip-preview 0.38.0 — MIT

- Files: licenses/pip-preview-0.38.0/LICENSE
- License text: upstream repository license, reproduced byte-for-byte; the published crate ships no per-crate file (see vendor/cua-driver/PROVENANCE.md)
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.

#### piper 0.2.5 — MIT OR Apache-2.0

- Files: licenses/piper-0.2.5/LICENSE-APACHE, licenses/piper-0.2.5/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/piper

#### pkg-config 0.3.34 — MIT OR Apache-2.0

- Files: licenses/pkg-config-0.3.34/LICENSE-APACHE, licenses/pkg-config-0.3.34/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/pkg-config-rs

#### plain 0.2.3 — MIT/Apache-2.0

- Files: licenses/plain-0.2.3/LICENSE-APACHE, licenses/plain-0.2.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Plain contributors
- Repository: https://github.com/randomites/plain

#### platform-linux 0.38.0 — MIT

- Files: licenses/platform-linux-0.38.0/LICENSE
- License text: upstream repository license, reproduced byte-for-byte; the published crate ships no per-crate file (see vendor/cua-driver/PROVENANCE.md)
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.

#### platform-windows 0.38.0 — MIT

- Files: licenses/platform-windows-0.38.0/LICENSE
- License text: upstream repository license, reproduced byte-for-byte; the published crate ships no per-crate file (see vendor/cua-driver/PROVENANCE.md)
- Copyright (LICENSE): Copyright (c) 2025 Cua AI, Inc.

#### png 0.18.1 — MIT OR Apache-2.0

- Files: licenses/png-0.18.1/LICENSE-APACHE, licenses/png-0.18.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 nwin
- Repository: https://github.com/image-rs/image-png

#### polling 3.11.0 — Apache-2.0 OR MIT

- Files: licenses/polling-3.11.0/LICENSE-APACHE, licenses/polling-3.11.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/smol-rs/polling

#### poly1305 0.9.1 — Apache-2.0 OR MIT

- Files: licenses/poly1305-0.9.1/LICENSE-APACHE, licenses/poly1305-0.9.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2026 The RustCrypto Project Developers
- Repository: https://github.com/RustCrypto/universal-hashes

#### postcard 1.1.3 — MIT OR Apache-2.0

- Files: licenses/postcard-1.1.3/LICENSE-APACHE, licenses/postcard-1.1.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019 Anthony James Munns
- Repository: https://github.com/jamesmunns/postcard

#### potential_utf 0.1.6 — Unicode-3.0

- Files: licenses/potential_utf-0.1.6/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### powerfmt 0.2.1 — MIT OR Apache-2.0

- Files: licenses/powerfmt-0.2.1/LICENSE-Apache, licenses/powerfmt-0.2.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2023 Jacob Pratt et al.
- Repository: https://github.com/jhpratt/powerfmt

#### ppv-lite86 0.2.21 — MIT OR Apache-2.0

- Files: licenses/ppv-lite86-0.2.21/LICENSE-APACHE, licenses/ppv-lite86-0.2.21/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019 The CryptoCorrosion Contributors
- Repository: https://github.com/cryptocorrosion/cryptocorrosion

#### proc-macro-crate 3.5.0 — MIT OR Apache-2.0

- Files: licenses/proc-macro-crate-3.5.0/LICENSE-APACHE, licenses/proc-macro-crate-3.5.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bkchr/proc-macro-crate

#### proc-macro2 1.0.107 — MIT OR Apache-2.0

- Files: licenses/proc-macro2-1.0.107/LICENSE-APACHE, licenses/proc-macro2-1.0.107/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/proc-macro2

#### pxfm 0.1.30 — BSD-3-Clause OR Apache-2.0

- Files: licenses/pxfm-0.1.30/LICENSE-APACHE.md, licenses/pxfm-0.1.30/LICENSE.md
- Copyright (LICENSE.md): Copyright (c) Radzivon Bartoshyk. All rights reserved.
- Repository: https://github.com/awxkee/pxfm

#### quick-error 2.0.1 — MIT/Apache-2.0

- Files: licenses/quick-error-2.0.1/LICENSE-APACHE, licenses/quick-error-2.0.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 The quick-error Developers
- Repository: http://github.com/tailhook/quick-error

#### quick-xml 0.41.0 — MIT

- Files: licenses/quick-xml-0.41.0/LICENSE-MIT.md
- Copyright (LICENSE-MIT.md): Copyright (c) 2016 Johann Tuffe
- Repository: https://github.com/tafia/quick-xml

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

#### r-efi 6.0.0 — MIT OR Apache-2.0 OR LGPL-2.1-or-later

- Files: licenses/r-efi-6.0.0/AUTHORS
- Copyright (AUTHORS): Copyright (C) 2017-2023 Red Hat, Inc.
- Copyright (AUTHORS): Copyright (C) 2019-2023 Microsoft Corporation
- Copyright (AUTHORS): Copyright (C) 2022-2023 David Rheinsberg
- Repository: https://github.com/r-efi/r-efi

#### radium 0.7.0 — MIT

- Files: licenses/radium-0.7.0/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2019 kneecaw (Nika Layzell)
- Repository: https://github.com/bitvecto-rs/radium

#### rand 0.10.3 — MIT OR Apache-2.0

- Files: licenses/rand-0.10.3/COPYRIGHT, licenses/rand-0.10.3/LICENSE-APACHE, licenses/rand-0.10.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2018 Developers of the Rand project
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/rand

#### rand 0.8.8 — MIT OR Apache-2.0

- Files: licenses/rand-0.8.8/COPYRIGHT, licenses/rand-0.8.8/LICENSE-APACHE, licenses/rand-0.8.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2018 Developers of the Rand project
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/rand

#### rand_chacha 0.3.1 — MIT OR Apache-2.0

- Files: licenses/rand_chacha-0.3.1/COPYRIGHT, licenses/rand_chacha-0.3.1/LICENSE-APACHE, licenses/rand_chacha-0.3.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2018 Developers of the Rand project
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/rand

#### rand_core 0.10.1 — MIT OR Apache-2.0

- Files: licenses/rand_core-0.10.1/COPYRIGHT, licenses/rand_core-0.10.1/LICENSE-APACHE, licenses/rand_core-0.10.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018-2026 The Rand Project Developers
- Repository: https://github.com/rust-random/rand_core

#### rand_core 0.6.4 — MIT OR Apache-2.0

- Files: licenses/rand_core-0.6.4/COPYRIGHT, licenses/rand_core-0.6.4/LICENSE-APACHE, licenses/rand_core-0.6.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2018 Developers of the Rand project
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-random/rand

#### redox_syscall 0.5.18 — MIT

- Files: licenses/redox_syscall-0.5.18/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Redox OS Developers
- Repository: https://gitlab.redox-os.org/redox-os/syscall

#### redox_users 0.4.6 — MIT

- Files: licenses/redox_users-0.4.6/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Jose Narvaez
- Repository: https://gitlab.redox-os.org/redox-os/users

#### ref-cast 1.0.27 — MIT OR Apache-2.0

- Files: licenses/ref-cast-1.0.27/LICENSE-APACHE, licenses/ref-cast-1.0.27/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/ref-cast

#### ref-cast-impl 1.0.27 — MIT OR Apache-2.0

- Files: licenses/ref-cast-impl-1.0.27/LICENSE-APACHE, licenses/ref-cast-impl-1.0.27/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/ref-cast

#### referencing 0.46.10 — MIT

- Files: licenses/referencing-0.46.10/LICENSE
- Copyright (LICENSE): Copyright (c) 2020-2026 Dmitry Dygalo
- Repository: https://github.com/Stranger6667/jsonschema

#### regex 1.13.1 — MIT OR Apache-2.0

- Files: licenses/regex-1.13.1/LICENSE-APACHE, licenses/regex-1.13.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-lang/regex

#### regex-automata 0.4.18 — MIT OR Apache-2.0

- Files: licenses/regex-automata-0.4.18/LICENSE-APACHE, licenses/regex-automata-0.4.18/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-lang/regex

#### regex-syntax 0.8.11 — MIT OR Apache-2.0

- Files: licenses/regex-syntax-0.8.11/LICENSE-APACHE, licenses/regex-syntax-0.8.11/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/rust-lang/regex

#### regorus 0.10.1 — MIT AND Apache-2.0 AND BSD-3-Clause

- Files: licenses/regorus-0.10.1/LICENSE
- Copyright (LICENSE): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/regorus

#### relative-path 2.0.3 — MIT OR Apache-2.0

- Files: licenses/relative-path-2.0.3/LICENSE-APACHE, licenses/relative-path-2.0.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Repository: https://github.com/udoprog/relative-path

#### rquickjs 0.14.0 — MIT

- Files: licenses/rquickjs-0.14.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2020 Mees Delzenne
- Copyright (LICENSE): Copyright (c) 2025 Rquickjs Contributors
- Repository: https://github.com/DelSkayn/rquickjs.git

#### rquickjs-core 0.14.0 — MIT

- Files: licenses/rquickjs-core-0.14.0/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/DelSkayn/rquickjs.git

#### rquickjs-sys 0.14.0 — MIT

- Files: licenses/rquickjs-sys-0.14.0/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/DelSkayn/rquickjs.git

#### rustc-hash 2.1.3 — Apache-2.0 OR MIT

- Files: licenses/rustc-hash-2.1.3/LICENSE-APACHE, licenses/rustc-hash-2.1.3/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/rust-lang/rustc-hash

#### rustc_version 0.4.1 — MIT OR Apache-2.0

- Files: licenses/rustc_version-0.4.1/LICENSE-APACHE, licenses/rustc_version-0.4.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 The Rust Project Developers
- Repository: https://github.com/djc/rustc-version-rs

#### rustix 1.1.6 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/rustix-1.1.6/COPYRIGHT, licenses/rustix-1.1.6/LICENSE-APACHE, licenses/rustix-1.1.6/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/rustix-1.1.6/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/rustix

#### rustversion 1.0.23 — MIT OR Apache-2.0

- Files: licenses/rustversion-1.0.23/LICENSE-APACHE, licenses/rustversion-1.0.23/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/rustversion

#### ryu 1.0.23 — Apache-2.0 OR BSL-1.0

- Files: licenses/ryu-1.0.23/LICENSE-APACHE, licenses/ryu-1.0.23/LICENSE-BOOST
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/ryu

#### schemars 1.2.3 — MIT

- Files: licenses/schemars-1.2.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Graham Esau
- Repository: https://github.com/GREsau/schemars

#### schemars_derive 1.2.3 — MIT

- Files: licenses/schemars_derive-1.2.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Graham Esau
- Repository: https://github.com/GREsau/schemars

#### scopeguard 1.2.0 — MIT OR Apache-2.0

- Files: licenses/scopeguard-1.2.0/LICENSE-APACHE, licenses/scopeguard-1.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016-2019 Ulrik Sverdrup "bluss" and scopeguard developers
- Repository: https://github.com/bluss/scopeguard

#### scroll 0.12.0 — MIT

- Files: licenses/scroll-0.12.0/LICENSE
- Copyright (LICENSE): Copyright (c) m4b 2016
- Repository: https://github.com/m4b/scroll

#### scroll_derive 0.12.1 — MIT

- Files: licenses/scroll_derive-0.12.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2017
- Repository: https://github.com/m4b/scroll

#### secret-service 5.2.0 — MIT OR Apache-2.0

- Files: licenses/secret-service-5.2.0/LICENSE-APACHE, licenses/secret-service-5.2.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2025 secret-service Developers
- Repository: https://github.com/hwchen/secret-service-rs.git

#### security-framework 3.7.0 — MIT OR Apache-2.0

- Files: licenses/security-framework-3.7.0/LICENSE-APACHE, licenses/security-framework-3.7.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Fackler
- Repository: https://github.com/kornelski/rust-security-framework

#### security-framework-sys 2.17.0 — MIT OR Apache-2.0

- Files: licenses/security-framework-sys-2.17.0/LICENSE-APACHE, licenses/security-framework-sys-2.17.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Fackler
- Repository: https://github.com/kornelski/rust-security-framework

#### semver 1.0.28 — MIT OR Apache-2.0

- Files: licenses/semver-1.0.28/LICENSE-APACHE, licenses/semver-1.0.28/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/semver

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

#### serde_derive_internals 0.30.0 — MIT OR Apache-2.0

- Files: licenses/serde_derive_internals-0.30.0/LICENSE-APACHE, licenses/serde_derive_internals-0.30.0/LICENSE-MIT
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

#### serde_repr 0.1.21 — MIT OR Apache-2.0

- Files: licenses/serde_repr-0.1.21/LICENSE-APACHE, licenses/serde_repr-0.1.21/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/serde-repr

#### serde_spanned 1.1.2 — MIT OR Apache-2.0

- Files: licenses/serde_spanned-1.1.2/LICENSE-APACHE, licenses/serde_spanned-1.1.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### serde_urlencoded 0.7.1 — MIT/Apache-2.0

- Files: licenses/serde_urlencoded-0.7.1/LICENSE-APACHE, licenses/serde_urlencoded-0.7.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 Anthony Ramine
- Repository: https://github.com/nox/serde_urlencoded

#### serde_yaml 0.9.34+deprecated — MIT OR Apache-2.0

- Files: licenses/serde_yaml-0.9.34+deprecated/LICENSE-APACHE, licenses/serde_yaml-0.9.34+deprecated/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/serde-yaml

#### serde_yaml_ng 0.10.0 — MIT

- Files: licenses/serde_yaml_ng-0.10.0/LICENSE
- Copyright (LICENSE): Copyright 2024 Antoine Catton
- Copyright (LICENSE): Copyright 2016-2024 David Tolnay
- Repository: https://github.com/acatton/serde-yaml-ng

#### sha1 0.10.7 — MIT OR Apache-2.0

- Files: licenses/sha1-0.10.7/LICENSE-APACHE, licenses/sha1-0.10.7/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2006-2009 Graydon Hoare
- Copyright (LICENSE-MIT): Copyright (c) 2009-2013 Mozilla Foundation
- Copyright (LICENSE-MIT): Copyright (c) 2016 Artyom Pavlov
- Repository: https://github.com/RustCrypto/hashes

#### sha2 0.10.9 — MIT OR Apache-2.0

- Files: licenses/sha2-0.10.9/LICENSE-APACHE, licenses/sha2-0.10.9/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2006-2009 Graydon Hoare
- Copyright (LICENSE-MIT): Copyright (c) 2009-2013 Mozilla Foundation
- Copyright (LICENSE-MIT): Copyright (c) 2016 Artyom Pavlov
- Repository: https://github.com/RustCrypto/hashes

#### sha2 0.11.1 — MIT OR Apache-2.0

- Files: licenses/sha2-0.11.1/LICENSE-APACHE, licenses/sha2-0.11.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016-2026 The RustCrypto Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2016 Artyom Pavlov
- Copyright (LICENSE-MIT): Copyright (c) 2009-2013 Mozilla Foundation
- ... (+1 more copyright line(s) in LICENSE-MIT; see full text)
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

#### siphasher 1.0.4 — MIT OR Apache-2.0

- Files: licenses/siphasher-1.0.4/COPYING, licenses/siphasher-1.0.4/LICENSE-APACHE, licenses/siphasher-1.0.4/LICENSE-MIT
- Copyright (COPYING): Copyright 2012-2016 The Rust Project Developers.
- Copyright (COPYING): Copyright 2016-2026 Frank Denis.
- Repository: https://github.com/jedisct1/rust-siphash

#### slab 0.4.12 — MIT

- Files: licenses/slab-0.4.12/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Carl Lerche
- Repository: https://github.com/tokio-rs/slab

#### smallvec 1.16.4 — MIT OR Apache-2.0

- Files: licenses/smallvec-1.16.4/LICENSE-APACHE, licenses/smallvec-1.16.4/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 The Servo Project Developers
- Repository: https://github.com/servo/rust-smallvec

#### smawk 0.3.3 — MIT

- Files: licenses/smawk-0.3.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Martin Geisler
- Repository: https://github.com/mgeisler/smawk

#### socket2 0.6.5 — MIT OR Apache-2.0

- Files: licenses/socket2-0.6.5/LICENSE-APACHE, licenses/socket2-0.6.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/rust-lang/socket2

#### spin 0.10.1 — MIT

- Files: licenses/spin-0.10.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2014 Mathijs van de Nes
- Repository: https://github.com/mvdnes/spin-rs.git

#### spin 0.9.9 — MIT

- Files: licenses/spin-0.9.9/LICENSE
- Copyright (LICENSE): Copyright (c) 2014 Mathijs van de Nes
- Repository: https://github.com/mvdnes/spin-rs.git

#### stable_deref_trait 1.2.1 — MIT OR Apache-2.0

- Files: licenses/stable_deref_trait-1.2.1/LICENSE-APACHE, licenses/stable_deref_trait-1.2.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Robert Grosse
- Repository: https://github.com/storyyeller/stable_deref_trait

#### static_assertions 1.1.0 — MIT OR Apache-2.0

- Files: licenses/static_assertions-1.1.0/LICENSE-APACHE, licenses/static_assertions-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Nikolai Vazquez
- Repository: https://github.com/nvzqz/static-assertions-rs

#### strict-num 0.1.1 — MIT

- Files: licenses/strict-num-0.1.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2022 Yevhenii Reizner
- Repository: https://github.com/RazrFalcon/strict-num

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

#### tap 1.0.1 — MIT

- Files: licenses/tap-1.0.1/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2017 Elliot Linder <darfink@gmail.com>
- Repository: https://github.com/myrrlyn/tap

#### tempfile 3.27.0 — MIT OR Apache-2.0

- Files: licenses/tempfile-3.27.0/LICENSE-APACHE, licenses/tempfile-3.27.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 Steven Allen
- Repository: https://github.com/Stebalien/tempfile

#### textwrap 0.16.4 — MIT

- Files: licenses/textwrap-0.16.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 Martin Geisler
- Repository: https://github.com/mgeisler/textwrap

#### thiserror 1.0.69 — MIT OR Apache-2.0

- Files: licenses/thiserror-1.0.69/LICENSE-APACHE, licenses/thiserror-1.0.69/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/thiserror

#### thiserror 2.0.21 — MIT OR Apache-2.0

- Files: licenses/thiserror-2.0.21/LICENSE-APACHE, licenses/thiserror-2.0.21/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/thiserror

#### thiserror-impl 1.0.69 — MIT OR Apache-2.0

- Files: licenses/thiserror-impl-1.0.69/LICENSE-APACHE, licenses/thiserror-impl-1.0.69/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/thiserror

#### thiserror-impl 2.0.21 — MIT OR Apache-2.0

- Files: licenses/thiserror-impl-2.0.21/LICENSE-APACHE, licenses/thiserror-impl-2.0.21/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/thiserror

#### tiff 0.11.3 — MIT

- Files: licenses/tiff-0.11.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2018 PistonDevelopers
- Repository: https://github.com/image-rs/image-tiff

#### time 0.3.55 — MIT OR Apache-2.0

- Files: licenses/time-0.3.55/LICENSE-Apache, licenses/time-0.3.55/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Jacob Pratt et al.
- Repository: https://github.com/time-rs/time

#### time-core 0.1.9 — MIT OR Apache-2.0

- Files: licenses/time-core-0.1.9/LICENSE-Apache, licenses/time-core-0.1.9/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Jacob Pratt et al.
- Repository: https://github.com/time-rs/time

#### time-macros 0.2.32 — MIT OR Apache-2.0

- Files: licenses/time-macros-0.2.32/LICENSE-Apache, licenses/time-macros-0.2.32/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Jacob Pratt et al.
- Repository: https://github.com/time-rs/time

#### tiny-skia 0.11.4 — BSD-3-Clause

- Files: licenses/tiny-skia-0.11.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2011 Google Inc. All rights reserved.
- Copyright (LICENSE): Copyright (c) 2020 Yevhenii Reizner All rights reserved.
- Repository: https://github.com/RazrFalcon/tiny-skia

#### tiny-skia-path 0.11.4 — BSD-3-Clause

- Files: licenses/tiny-skia-path-0.11.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2011 Google Inc. All rights reserved.
- Copyright (LICENSE): Copyright (c) 2020 Yevhenii Reizner All rights reserved.
- Repository: https://github.com/RazrFalcon/tiny-skia/tree/master/path

#### tinystr 0.8.5 — Unicode-3.0

- Files: licenses/tinystr-0.8.5/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### tokio 1.54.1 — MIT

- Files: licenses/tokio-1.54.1/LICENSE
- Copyright (LICENSE): Copyright (c) Tokio Contributors
- Repository: https://github.com/tokio-rs/tokio

#### tokio-macros 2.7.2 — MIT

- Files: licenses/tokio-macros-2.7.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Yoshua Wuyts
- Copyright (LICENSE): Copyright (c) Tokio Contributors
- Repository: https://github.com/tokio-rs/tokio

#### tokio-tungstenite 0.24.0 — MIT

- Files: licenses/tokio-tungstenite-0.24.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Daniel Abramov
- Copyright (LICENSE): Copyright (c) 2017 Alexey Galakhov
- Repository: https://github.com/snapview/tokio-tungstenite

#### toml 0.9.12+spec-1.1.0 — MIT OR Apache-2.0

- Files: licenses/toml-0.9.12+spec-1.1.0/LICENSE-APACHE, licenses/toml-0.9.12+spec-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### toml_datetime 0.7.5+spec-1.1.0 — MIT OR Apache-2.0

- Files: licenses/toml_datetime-0.7.5+spec-1.1.0/LICENSE-APACHE, licenses/toml_datetime-0.7.5+spec-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### toml_datetime 1.1.2+spec-1.1.0 — MIT OR Apache-2.0

- Files: licenses/toml_datetime-1.1.2+spec-1.1.0/LICENSE-APACHE, licenses/toml_datetime-1.1.2+spec-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### toml_edit 0.25.18+spec-1.1.0 — MIT OR Apache-2.0

- Files: licenses/toml_edit-0.25.18+spec-1.1.0/LICENSE-APACHE, licenses/toml_edit-0.25.18+spec-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### toml_parser 1.1.5+spec-1.1.0 — MIT OR Apache-2.0

- Files: licenses/toml_parser-1.1.5+spec-1.1.0/LICENSE-APACHE, licenses/toml_parser-1.1.5+spec-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### toml_writer 1.2.0+spec-1.1.0 — MIT OR Apache-2.0

- Files: licenses/toml_writer-1.2.0+spec-1.1.0/LICENSE-APACHE, licenses/toml_writer-1.2.0+spec-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) Individual contributors
- Repository: https://github.com/toml-rs/toml

#### tower 0.5.3 — MIT

- Files: licenses/tower-0.5.3/LICENSE
- Copyright (LICENSE): Copyright (c) 2019 Tower Contributors
- Repository: https://github.com/tower-rs/tower

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

#### tree_magic_mini 3.2.2 — MIT

- Files: licenses/tree_magic_mini-3.2.2/LICENSE
- Copyright (LICENSE): Copyright (c) 2017 Aaron Hancock
- Repository: https://github.com/mbrubeck/tree_magic/

#### ttf-parser 0.25.1 — MIT OR Apache-2.0

- Files: licenses/ttf-parser-0.25.1/LICENSE-APACHE, licenses/ttf-parser-0.25.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2018 Yevhenii Reizner
- Repository: https://github.com/harfbuzz/ttf-parser

#### tungstenite 0.24.0 — MIT OR Apache-2.0

- Files: licenses/tungstenite-0.24.0/LICENSE-APACHE, licenses/tungstenite-0.24.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017 Alexey Galakhov
- Copyright (LICENSE-MIT): Copyright (c) 2016 Jason Housley
- Repository: https://github.com/snapview/tungstenite-rs

#### typenum 1.20.1 — MIT OR Apache-2.0

- Files: licenses/typenum-1.20.1/LICENSE, licenses/typenum-1.20.1/LICENSE-APACHE, licenses/typenum-1.20.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Paho Lurie-Gregg
- Repository: https://github.com/paholg/typenum

#### uds_windows 1.2.1 — MIT

- Files: licenses/uds_windows-1.2.1/LICENSE
- Copyright (LICENSE): Copyright (c) Microsoft Corporation. All rights reserved.
- Repository: https://github.com/haraldh/rust_uds_windows

#### unicode-general-category 1.1.0 — Apache-2.0

- Files: licenses/unicode-general-category-1.1.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/yeslogic/unicode-general-category

#### unicode-ident 1.0.27 — (MIT OR Apache-2.0) AND Unicode-3.0

- Files: licenses/unicode-ident-1.0.27/LICENSE-APACHE, licenses/unicode-ident-1.0.27/LICENSE-MIT, licenses/unicode-ident-1.0.27/LICENSE-UNICODE
- Copyright (LICENSE-UNICODE): Copyright © 1991-2023 Unicode, Inc.
- Repository: https://github.com/dtolnay/unicode-ident

#### unicode-width 0.2.3 — MIT OR Apache-2.0

- Files: licenses/unicode-width-0.2.3/COPYRIGHT, licenses/unicode-width-0.2.3/LICENSE-APACHE, licenses/unicode-width-0.2.3/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015 The Rust Project Developers
- Repository: https://github.com/unicode-rs/unicode-width

#### uniffi 0.31.0 — MPL-2.0

- Files: licenses/uniffi-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_bindgen 0.31.0 — MPL-2.0

- Files: licenses/uniffi_bindgen-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_core 0.31.0 — MPL-2.0

- Files: licenses/uniffi_core-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_internal_macros 0.31.0 — MPL-2.0

- Files: licenses/uniffi_internal_macros-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_macros 0.31.0 — MPL-2.0

- Files: licenses/uniffi_macros-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_meta 0.31.0 — MPL-2.0

- Files: licenses/uniffi_meta-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_pipeline 0.31.0 — MPL-2.0

- Files: licenses/uniffi_pipeline-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### uniffi_udl 0.31.0 — MPL-2.0

- Files: licenses/uniffi_udl-0.31.0/LICENSE-MPL
- License text: canonical MPL-2.0 text from the SPDX license list (scripts/license-overrides/MPL-2.0.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/mozilla/uniffi-rs

#### universal-hash 0.6.1 — MIT OR Apache-2.0

- Files: licenses/universal-hash-0.6.1/LICENSE-APACHE, licenses/universal-hash-0.6.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019-2025 RustCrypto Developers
- Repository: https://github.com/RustCrypto/traits

#### unsafe-libyaml 0.2.11 — MIT

- Files: licenses/unsafe-libyaml-0.2.11/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/dtolnay/unsafe-libyaml

#### url 2.5.8 — MIT OR Apache-2.0

- Files: licenses/url-2.5.8/LICENSE-APACHE, licenses/url-2.5.8/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2013-2025 The rust-url developers
- Repository: https://github.com/servo/rust-url

#### utf-8 0.7.6 — MIT OR Apache-2.0

- Files: licenses/utf-8-0.7.6/LICENSE-APACHE, licenses/utf-8-0.7.6/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/SimonSapin/rust-utf8

#### utf8_iter 1.1.0 — Apache-2.0 OR MIT

- Files: licenses/utf8_iter-1.1.0/COPYRIGHT, licenses/utf8_iter-1.1.0/LICENSE-APACHE, licenses/utf8_iter-1.1.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/hsivonen/utf8_iter

#### utf_types 1.0.0 — Apache-2.0 OR MIT

- Files: licenses/utf_types-1.0.0/COPYRIGHT, licenses/utf_types-1.0.0/LICENSE-APACHE, licenses/utf_types-1.0.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/hsivonen/utf_types

#### uuid 1.29.0 — Apache-2.0 OR MIT

- Files: licenses/uuid-1.29.0/LICENSE-APACHE, licenses/uuid-1.29.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 The Rust Project Developers
- Copyright (LICENSE-MIT): Copyright (c) 2018 Ashley Mannix, Christopher Armstrong, Dylan DPC, Hunar Roop Kahlon
- Repository: https://github.com/uuid-rs/uuid

#### uuid-simd 0.8.0 — MIT

- Files: licenses/uuid-simd-0.8.0/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/Nugine/simd

#### version_check 0.9.5 — MIT/Apache-2.0

- Files: licenses/version_check-0.9.5/LICENSE-APACHE, licenses/version_check-0.9.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2017-2018 Sergio Benitez
- Repository: https://github.com/SergioBenitez/version_check

#### vsimd 0.8.0 — MIT

- Files: licenses/vsimd-0.8.0/LICENSE-MIT
- License text: canonical MIT text from the SPDX license list (scripts/license-overrides/MIT.txt); the crate package ships no license file and embeds no copyright notice
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/Nugine/simd

#### wasi 0.11.1+wasi-snapshot-preview1 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/wasi-0.11.1+wasi-snapshot-preview1/LICENSE-APACHE, licenses/wasi-0.11.1+wasi-snapshot-preview1/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/wasi-0.11.1+wasi-snapshot-preview1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/wasi

#### wasip2 1.0.4+wasi-0.2.12 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/wasip2-1.0.4+wasi-0.2.12/LICENSE-APACHE, licenses/wasip2-1.0.4+wasi-0.2.12/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/wasip2-1.0.4+wasi-0.2.12/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/wasi-rs

#### wasm-bindgen 0.2.130 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-0.2.130/LICENSE-APACHE, licenses/wasm-bindgen-0.2.130/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen

#### wasm-bindgen-macro 0.2.130 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-macro-0.2.130/LICENSE-APACHE, licenses/wasm-bindgen-macro-0.2.130/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/macro

#### wasm-bindgen-macro-support 0.2.130 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-macro-support-0.2.130/LICENSE-APACHE, licenses/wasm-bindgen-macro-support-0.2.130/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/main/crates/macro-support

#### wasm-bindgen-shared 0.2.130 — MIT OR Apache-2.0

- Files: licenses/wasm-bindgen-shared-0.2.130/LICENSE-APACHE, licenses/wasm-bindgen-shared-0.2.130/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2014 Alex Crichton
- Repository: https://github.com/wasm-bindgen/wasm-bindgen/tree/master/crates/shared

#### wayland-backend 0.3.17 — MIT

- Files: licenses/wayland-backend-0.3.17/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2015 Elinor Berger
- Repository: https://github.com/smithay/wayland-rs

#### wayland-client 0.31.15 — MIT

- Files: licenses/wayland-client-0.31.15/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2015 Elinor Berger
- Repository: https://github.com/smithay/wayland-rs

#### wayland-protocols 0.32.13 — MIT

- Files: licenses/wayland-protocols-0.32.13/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2015 Elinor Berger
- Repository: https://github.com/smithay/wayland-rs

#### wayland-protocols-wlr 0.3.12 — MIT

- Files: licenses/wayland-protocols-wlr-0.3.12/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2015 Elinor Berger
- Repository: https://github.com/smithay/wayland-rs

#### wayland-scanner 0.31.11 — MIT

- Files: licenses/wayland-scanner-0.31.11/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2015 Elinor Berger
- Repository: https://github.com/smithay/wayland-rs

#### wayland-sys 0.31.11 — MIT

- Files: licenses/wayland-sys-0.31.11/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2015 Elinor Berger
- Repository: https://github.com/smithay/wayland-rs

#### weedle2 5.0.0 — MIT

- Files: licenses/weedle2-5.0.0/LICENSE.md
- Copyright (LICENSE.md): Copyright 2018-Present Sharad Chand
- Copyright (LICENSE.md): Copyright 2022 Jan-Erik Rediger
- Repository: https://github.com/mozilla/uniffi-rs

#### weezl 0.1.12 — MIT OR Apache-2.0

- Files: licenses/weezl-0.1.12/LICENSE-APACHE, licenses/weezl-0.1.12/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) HeroicKatora 2020
- Repository: https://github.com/image-rs/weezl

#### winapi 0.3.9 — MIT/Apache-2.0

- Files: licenses/winapi-0.3.9/LICENSE-APACHE, licenses/winapi-0.3.9/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2015-2018 The winapi-rs Developers
- Repository: https://github.com/retep998/winapi-rs

#### winapi-i686-pc-windows-gnu 0.4.0 — MIT/Apache-2.0

- Files: licenses/winapi-i686-pc-windows-gnu-0.4.0/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/retep998/winapi-rs

#### winapi-x86_64-pc-windows-gnu 0.4.0 — MIT/Apache-2.0

- Files: licenses/winapi-x86_64-pc-windows-gnu-0.4.0/LICENSE-APACHE
- License text: canonical Apache-2.0 text; the crate package ships no license file — reproduced byte-for-byte from ahash-0.8.12/LICENSE-APACHE in the local registry
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/retep998/winapi-rs

#### windows 0.58.0 — MIT OR Apache-2.0

- Files: licenses/windows-0.58.0/license-apache-2.0, licenses/windows-0.58.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows 0.59.0 — MIT OR Apache-2.0

- Files: licenses/windows-0.59.0/license-apache-2.0, licenses/windows-0.59.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-core 0.58.0 — MIT OR Apache-2.0

- Files: licenses/windows-core-0.58.0/license-apache-2.0, licenses/windows-core-0.58.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-core 0.59.0 — MIT OR Apache-2.0

- Files: licenses/windows-core-0.59.0/license-apache-2.0, licenses/windows-core-0.59.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-core 0.62.2 — MIT OR Apache-2.0

- Files: licenses/windows-core-0.62.2/license-apache-2.0, licenses/windows-core-0.62.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-implement 0.58.0 — MIT OR Apache-2.0

- Files: licenses/windows-implement-0.58.0/license-apache-2.0, licenses/windows-implement-0.58.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-implement 0.59.0 — MIT OR Apache-2.0

- Files: licenses/windows-implement-0.59.0/license-apache-2.0, licenses/windows-implement-0.59.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-implement 0.60.2 — MIT OR Apache-2.0

- Files: licenses/windows-implement-0.60.2/license-apache-2.0, licenses/windows-implement-0.60.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-interface 0.58.0 — MIT OR Apache-2.0

- Files: licenses/windows-interface-0.58.0/license-apache-2.0, licenses/windows-interface-0.58.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-interface 0.59.3 — MIT OR Apache-2.0

- Files: licenses/windows-interface-0.59.3/license-apache-2.0, licenses/windows-interface-0.59.3/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-link 0.1.3 — MIT OR Apache-2.0

- Files: licenses/windows-link-0.1.3/license-apache-2.0, licenses/windows-link-0.1.3/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-link 0.2.1 — MIT OR Apache-2.0

- Files: licenses/windows-link-0.2.1/license-apache-2.0, licenses/windows-link-0.2.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-native-keyring-store 1.1.0 — MIT OR Apache-2.0

- Files: licenses/windows-native-keyring-store-1.1.0/LICENSE-APACHE, licenses/windows-native-keyring-store-1.1.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 keyring Developers
- Repository: https://github.com/open-source-cooperative/windows-native-keyring-store.git

#### windows-result 0.2.0 — MIT OR Apache-2.0

- Files: licenses/windows-result-0.2.0/license-apache-2.0, licenses/windows-result-0.2.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-result 0.3.4 — MIT OR Apache-2.0

- Files: licenses/windows-result-0.3.4/license-apache-2.0, licenses/windows-result-0.3.4/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-result 0.4.1 — MIT OR Apache-2.0

- Files: licenses/windows-result-0.4.1/license-apache-2.0, licenses/windows-result-0.4.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-strings 0.1.0 — MIT OR Apache-2.0

- Files: licenses/windows-strings-0.1.0/license-apache-2.0, licenses/windows-strings-0.1.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-strings 0.3.1 — MIT OR Apache-2.0

- Files: licenses/windows-strings-0.3.1/license-apache-2.0, licenses/windows-strings-0.3.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-strings 0.5.1 — MIT OR Apache-2.0

- Files: licenses/windows-strings-0.5.1/license-apache-2.0, licenses/windows-strings-0.5.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-sys 0.48.0 — MIT OR Apache-2.0

- Files: licenses/windows-sys-0.48.0/license-apache-2.0, licenses/windows-sys-0.48.0/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-sys 0.61.2 — MIT OR Apache-2.0

- Files: licenses/windows-sys-0.61.2/license-apache-2.0, licenses/windows-sys-0.61.2/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-targets 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows-targets-0.48.5/license-apache-2.0, licenses/windows-targets-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-targets 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows-targets-0.52.6/license-apache-2.0, licenses/windows-targets-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-targets 0.53.5 — MIT OR Apache-2.0

- Files: licenses/windows-targets-0.53.5/license-apache-2.0, licenses/windows-targets-0.53.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows-win 3.0.0 — BSL-1.0

- Files: licenses/windows-win-3.0.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/DoumanAsh/windows-win-rs

#### windows_aarch64_gnullvm 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_gnullvm-0.48.5/license-apache-2.0, licenses/windows_aarch64_gnullvm-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_gnullvm 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_gnullvm-0.52.6/license-apache-2.0, licenses/windows_aarch64_gnullvm-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_gnullvm 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_gnullvm-0.53.1/license-apache-2.0, licenses/windows_aarch64_gnullvm-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_msvc 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_msvc-0.48.5/license-apache-2.0, licenses/windows_aarch64_msvc-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_msvc 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_msvc-0.52.6/license-apache-2.0, licenses/windows_aarch64_msvc-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_aarch64_msvc 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_aarch64_msvc-0.53.1/license-apache-2.0, licenses/windows_aarch64_msvc-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnu 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnu-0.48.5/license-apache-2.0, licenses/windows_i686_gnu-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnu 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnu-0.52.6/license-apache-2.0, licenses/windows_i686_gnu-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnu 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnu-0.53.1/license-apache-2.0, licenses/windows_i686_gnu-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnullvm 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnullvm-0.52.6/license-apache-2.0, licenses/windows_i686_gnullvm-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_gnullvm 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_i686_gnullvm-0.53.1/license-apache-2.0, licenses/windows_i686_gnullvm-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_msvc 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_i686_msvc-0.48.5/license-apache-2.0, licenses/windows_i686_msvc-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_msvc 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_i686_msvc-0.52.6/license-apache-2.0, licenses/windows_i686_msvc-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_i686_msvc 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_i686_msvc-0.53.1/license-apache-2.0, licenses/windows_i686_msvc-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnu 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnu-0.48.5/license-apache-2.0, licenses/windows_x86_64_gnu-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnu 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnu-0.52.6/license-apache-2.0, licenses/windows_x86_64_gnu-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnu 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnu-0.53.1/license-apache-2.0, licenses/windows_x86_64_gnu-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnullvm 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnullvm-0.48.5/license-apache-2.0, licenses/windows_x86_64_gnullvm-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnullvm 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnullvm-0.52.6/license-apache-2.0, licenses/windows_x86_64_gnullvm-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_gnullvm 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_gnullvm-0.53.1/license-apache-2.0, licenses/windows_x86_64_gnullvm-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_msvc 0.48.5 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_msvc-0.48.5/license-apache-2.0, licenses/windows_x86_64_msvc-0.48.5/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_msvc 0.52.6 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_msvc-0.52.6/license-apache-2.0, licenses/windows_x86_64_msvc-0.52.6/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### windows_x86_64_msvc 0.53.1 — MIT OR Apache-2.0

- Files: licenses/windows_x86_64_msvc-0.53.1/license-apache-2.0, licenses/windows_x86_64_msvc-0.53.1/license-mit
- Copyright (license-mit): Copyright (c) Microsoft Corporation.
- Repository: https://github.com/microsoft/windows-rs

#### winnow 0.7.15 — MIT

- Files: licenses/winnow-0.7.15/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/winnow-rs/winnow

#### winnow 1.0.4 — MIT

- Files: licenses/winnow-1.0.4/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/winnow-rs/winnow

#### wit-bindgen 0.57.1 — Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT

- Files: licenses/wit-bindgen-0.57.1/LICENSE-APACHE, licenses/wit-bindgen-0.57.1/LICENSE-Apache-2.0_WITH_LLVM-exception, licenses/wit-bindgen-0.57.1/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/bytecodealliance/wit-bindgen

#### wl-clipboard-rs 0.9.5 — MIT OR Apache-2.0

- Files: licenses/wl-clipboard-rs-0.9.5/LICENSE-APACHE, licenses/wl-clipboard-rs-0.9.5/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019 Ivan Molodetskikh
- Repository: https://github.com/YaLTeR/wl-clipboard-rs

#### writeable 0.6.4 — Unicode-3.0

- Files: licenses/writeable-0.6.4/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### wyz 0.5.1 — MIT

- Files: licenses/wyz-0.5.1/LICENSE.txt
- Copyright (LICENSE.txt): Copyright (c) 2018 myrrlyn (Alexander Payne)
- Repository: https://github.com/myrrlyn/wyz

#### x11 2.21.0 — MIT

- Files: licenses/x11-2.21.0/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/AltF02/x11-rs.git

#### x11rb 0.13.2 — MIT OR Apache-2.0

- Files: licenses/x11rb-0.13.2/LICENSE-APACHE, licenses/x11rb-0.13.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2019 x11rb Contributers
- Repository: https://github.com/psychon/x11rb

#### x11rb-protocol 0.13.2 — MIT OR Apache-2.0

- Files: licenses/x11rb-protocol-0.13.2/LICENSE-APACHE, licenses/x11rb-protocol-0.13.2/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright 2019 x11rb Contributers
- Repository: https://github.com/psychon/x11rb

#### xkbcommon 0.9.0 — MIT

- Files: licenses/xkbcommon-0.9.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2016 Remi Thebault <remi.thebault@gmail.com>
- Repository: https://github.com/rust-x-bindings/xkbcommon-rs

#### xkeysym 0.2.1 — MIT OR Apache-2.0 OR Zlib

- Files: licenses/xkeysym-0.2.1/LICENSE-APACHE, licenses/xkeysym-0.2.1/LICENSE-MIT, licenses/xkeysym-0.2.1/LICENSE-ZLIB
- Copyright (LICENSE-MIT): Copyright (c) 2022-2023 John Nunley
- Copyright (LICENSE-ZLIB): Copyright (c) 2022-2023 John Nunley
- Repository: https://github.com/notgull/xkeysym

#### yoke 0.8.4 — Unicode-3.0

- Files: licenses/yoke-0.8.4/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### yoke-derive 0.8.4 — Unicode-3.0

- Files: licenses/yoke-derive-0.8.4/LICENSE
- Copyright (LICENSE): Copyright © 2020-2024 Unicode, Inc.
- Repository: https://github.com/unicode-org/icu4x

#### zbus 5.19.0 — MIT

- Files: licenses/zbus-5.19.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Zeeshan Ali Khan & zbus contributors
- Repository: https://github.com/z-galaxy/zbus/

#### zbus-lockstep 0.5.2 — MIT

- Files: licenses/zbus-lockstep-0.5.2/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/luukvanderduim/zbus-lockstep

#### zbus-lockstep-macros 0.5.2 — MIT

- Files: licenses/zbus-lockstep-macros-0.5.2/LICENSE-MIT
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/luukvanderduim/zbus-lockstep

#### zbus-secret-service-keyring-store 1.0.1 — MIT OR Apache-2.0

- Files: licenses/zbus-secret-service-keyring-store-1.0.1/LICENSE-APACHE, licenses/zbus-secret-service-keyring-store-1.0.1/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2016 keyring Developers
- Repository: https://github.com/open-source-cooperative/zbus-secret-service-keyring-store.git

#### zbus_macros 5.19.0 — MIT

- Files: licenses/zbus_macros-5.19.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Zeeshan Ali Khan & zbus contributors
- Repository: https://github.com/z-galaxy/zbus/

#### zbus_names 4.3.5 — MIT

- Files: licenses/zbus_names-4.3.5/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Zeeshan Ali Khan & zbus contributors
- Repository: https://github.com/z-galaxy/zbus/

#### zbus_xml 5.2.1 — MIT

- Files: licenses/zbus_xml-5.2.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Zeeshan Ali Khan & zbus contributors
- Repository: https://github.com/z-galaxy/zbus/

#### zcheapstr 1.2.0 — MIT

- Files: licenses/zcheapstr-1.2.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2026 Zeeshan Ali Khan & zcheapstr contributors
- Repository: https://github.com/z-galaxy/zcheapstr/

#### zerocopy 0.8.63 — BSD-2-Clause OR Apache-2.0 OR MIT

- Files: licenses/zerocopy-0.8.63/LICENSE-APACHE, licenses/zerocopy-0.8.63/LICENSE-BSD, licenses/zerocopy-0.8.63/LICENSE-MIT
- Copyright (LICENSE-BSD): Copyright 2019 The Fuchsia Authors.
- Copyright (LICENSE-MIT): Copyright 2023 The Fuchsia Authors
- Repository: https://github.com/google/zerocopy

#### zerocopy-derive 0.8.63 — BSD-2-Clause OR Apache-2.0 OR MIT

- Files: licenses/zerocopy-derive-0.8.63/LICENSE-APACHE, licenses/zerocopy-derive-0.8.63/LICENSE-BSD, licenses/zerocopy-derive-0.8.63/LICENSE-MIT
- Copyright (LICENSE-BSD): Copyright 2019 The Fuchsia Authors.
- Copyright (LICENSE-MIT): Copyright 2023 The Fuchsia Authors
- Repository: https://github.com/google/zerocopy

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

#### zeroize_derive 1.5.0 — Apache-2.0 OR MIT

- Files: licenses/zeroize_derive-1.5.0/LICENSE-APACHE, licenses/zeroize_derive-1.5.0/LICENSE-MIT
- Copyright (LICENSE-MIT): Copyright (c) 2019-2026 The RustCrypto Project Developers
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

#### zstd 0.13.4 — BSD-3-Clause

- Files: licenses/zstd-0.13.4/LICENSE
- Copyright (LICENSE): Copyright (c) 2026, Alexandre Bury
- Repository: https://github.com/gyscos/zstd-rs

#### zstd-safe 7.4.0 — BSD-3-Clause

- Files: licenses/zstd-safe-7.4.0/LICENSE
- Copyright (LICENSE): Copyright (c) 2026, Alexandre Bury
- Repository: https://github.com/gyscos/zstd-rs

#### zstd-sys 2.1.2+zstd.1.5.7 — BSD-3-Clause

- Files: licenses/zstd-sys-2.1.2+zstd.1.5.7/LICENSE, licenses/zstd-sys-2.1.2+zstd.1.5.7/LICENSE.BSD-3-Clause
- Copyright (LICENSE): Copyright (c) 2026, Alexandre Bury
- Copyright (LICENSE.BSD-3-Clause): Copyright (c) Meta Platforms, Inc. and affiliates. All rights reserved.
- Repository: https://github.com/gyscos/zstd-rs

#### zune-core 0.5.1 — MIT OR Apache-2.0 OR Zlib

- Files: licenses/zune-core-0.5.1/LICENSE-APACHE, licenses/zune-core-0.5.1/LICENSE-MIT, licenses/zune-core-0.5.1/LICENSE-ZLIB
- Copyright (LICENSE-MIT): Copyright (c) zune-image developers
- Repository: https://github.com/etemesi254/zune-image

#### zune-jpeg 0.5.15 — MIT OR Apache-2.0 OR Zlib

- Files: licenses/zune-jpeg-0.5.15/LICENSE-APACHE, licenses/zune-jpeg-0.5.15/LICENSE-MIT, licenses/zune-jpeg-0.5.15/LICENSE-ZLIB
- Copyright (LICENSE-MIT): Copyright (c) zune-image developers
- Repository: https://github.com/etemesi254/zune-image/tree/dev/crates/zune-jpeg

#### zvariant 5.15.1 — MIT

- Files: licenses/zvariant-5.15.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Zeeshan Ali Khan & zbus contributors
- Repository: https://github.com/z-galaxy/zbus/

#### zvariant_derive 5.15.1 — MIT

- Files: licenses/zvariant_derive-5.15.1/LICENSE
- Copyright (LICENSE): Copyright (c) 2024 Zeeshan Ali Khan & zbus contributors
- Repository: https://github.com/z-galaxy/zbus/

#### zvariant_utils 4.2.0 — MIT

- Files: licenses/zvariant_utils-4.2.0/LICENSE
- Copyright: none embedded in the license file(s); see full text
- Repository: https://github.com/z-galaxy/zbus/

---

Bundle generated by `scripts/generate_notices.py` from the locked dependency graph. Do not edit by hand; regenerate instead.
