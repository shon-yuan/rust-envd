# rust-envd documentation index

| Document | Scope |
| --- | --- |
| [README](../README.md) | Supported platforms, pinned protocol baseline and builds |
| [中文 README](../README.zh-CN.md) | Simplified Chinese translation of the README |
| [Acceptance notes](../docs/acceptance-notes.md) | Linux protocol acceptance and evidence limits |
| [Native CUA contract](../docs/native-cua.md) | Implemented four-route REST API, strict S0 token admission, completion tracking, verified local matrix and acceptance limits |
| [Extensions](../docs/extensions.md) | Current native CUA instructions and migration; older MCP/worker acceptance explicitly scoped to prior revisions |
| [Windows port](../docs/windows-port.md) | Native S0 backend, current CUA unavailability before registry construction in service/Session 0/unknown contexts, and historical validation |
| [Packaging](../third_party/PACKAGING.md) | Artifact profiles, dependency notices and provenance |
| [Dependency security](../docs/dependency-security.md) | Targeted advisory fixes, ownership migration and remaining warnings |
| [Dependency runtime regression](../docs/dependency-regression.md) | Older-revision static musl, isolated Go/Rust, MCP CUA and upgrade evidence; not native REST acceptance |
| [Windows dependency regression](../docs/windows-dependency-regression.md) | Older-revision native base/desktop/worker and SDK results, including Session 0 refusal |
| [Windows CUA acceptance](../docs/windows-cua-acceptance.md) | Older-revision embedded graphics/window actions, SCM/helper recovery, credential boundaries and reset limitation; not acceptance of the native REST API |
| [Cua Driver provenance](../vendor/cua-driver/PROVENANCE.md) | Pinned vendor source, local changes and update procedure |

Check current manifests and lockfiles before making dependency claims. [Local native CUA verification](../docs/native-cua.md#verified-local-state) passed default 907/base 881 tests (zero failures; 24 doctests ignored each), including 15 desktop completion regressions. Final default/base all-target strict Clippy, formatting, Windows base MSVC crosscheck, 17 distribution checks, notices verification and rebuild passed. The final Rust deadline recheck closed with no remaining blocking defect. Observed Linux smoke reported 63 tools, one process and one listener without desktop actions; the final rebuilt smoke repetition passed. Scoped root-module coverage and acceptance limits are in the verification record. Native Windows desktop/live S0 acceptance, a fresh local musl build, Go/SDK comparison, remote CI and commit/push/merge/deploy remain outside this evidence. Historical evidence applies only to its recorded revision and scope.
