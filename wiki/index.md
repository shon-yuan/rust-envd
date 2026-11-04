# rust-envd documentation index

| Document | Scope |
| --- | --- |
| [README](../README.md) | Supported platforms, pinned protocol baseline and builds |
| [Acceptance notes](../docs/acceptance-notes.md) | Linux protocol acceptance and evidence limits |
| [Extensions](../docs/extensions.md) | CUA modes, shared credential admission, worker lifecycle and acceptance |
| [Windows port](../docs/windows-port.md) | Native backend, service/session boundaries and validation |
| [Packaging](../third_party/PACKAGING.md) | Artifact profiles, dependency notices and provenance |
| [Dependency security](../docs/dependency-security.md) | Targeted advisory fixes, ownership migration and remaining warnings |
| [Dependency runtime regression](../docs/dependency-regression.md) | Fresh musl build, isolated Go/Rust comparison, CUA and upgrade evidence |
| [Windows dependency regression](../docs/windows-dependency-regression.md) | Native base/desktop/worker builds and tests, SDK and Session 0 refusal |
| [Cua Driver provenance](../vendor/cua-driver/PROVENANCE.md) | Pinned vendor source, local changes and update procedure |

Check the current manifests and lockfiles before making dependency claims. Historical evidence applies only to the recorded revision and scope.
