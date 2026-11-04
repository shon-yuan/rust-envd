# Windows CUA native acceptance

## Scope — 2026-11-04

This record follows the [native dependency regression](windows-dependency-regression.md)
with actual graphical operations and service deployment checks. The executables
are the same debug artifacts built from
`c3cb81a3428115bca25046f8acbc49cb12495036`; no production source or lockfile is
changed by this campaign. Their hashes remain those in the native regression
record. Windows evidence is contract/behavioral evidence, not comparison with a
Go oracle.

## Isolated graphical fixture

The existing validation VM had an active user desktop, so the campaign did not
capture or inject input there. A libvirt full live disk backup completed with
zero remaining bytes before a separate transient VM was created. The original
VM stayed running. The backup was not guest-agent quiesced; this is not an
application-consistent backup acceptance claim. The copied system booted
successfully and its UUID was verified over SSH before fixture changes.

The copy has its own disk, UUID, MAC, NVRAM and fresh TPM state. Its system
volume was confirmed unencrypted before copying; no original TPM state was
copied. It uses four vCPUs and 6 GiB RAM, a dedicated libvirt network without
forwarding or DNS, and no guest default route. No external service or cloud
resource is required. The copy is disposable; the original Windows source
checkout and interactive session remain outside the test target.

A new ordinary local account, `CuaFixtureTf`, logs into the copy's Session 1.
All fixture scripts verify the copy's UUID; graphical launchers additionally
verify this account and a nonzero session. A Windows Forms application owns a
text box, a button/counter, a distinctive magenta marker and a JSON state file.
Its identity and observable state provide acceptance evidence independently of
MCP's action-success response.

The first capture showed the account's initial sign-in screen rather than the
fixture; the marker assertion failed before any click or typing. A retry
encountered a remaining welcome page. The copy's first-sign-in privacy-screen
policy was configured, and the welcome process belonging to the verified test
account was closed. No privacy-consent button was clicked. Both failed capture
attempts were retained before a clean runtime restart and successful rerun.

## Embedded user mode

The desktop executable ran as the ordinary fixture account with
`COORWORKER_CUA_MODE=embedded`, `--development --listen 127.0.0.1:49989`, and a
campaign-specific `ProgramData`. Its child environment was rebuilt from an
explicit runtime-variable allowlist. Model/provider credentials were not
supplied.

| Observation | Result |
| --- | --- |
| Process and listener ownership | One envd in Session 1, one loopback listener, no standalone worker |
| Native MCP discovery | 60 Windows tools registered |
| Launch | `launch_app` started only the campaign's guarded Windows Forms fixture |
| Desktop capture | 1280×800 PNG; fixture marker color verified at its observed position |
| Mouse input | Screenshot-grounded desktop click increased the independent button counter from 0 to 1 |
| Text input | A unique nonce appeared in the text box, fixture JSON, final screenshot and native UIA readback |
| Window discovery | PID-filtered `list_windows` returned the fixture HWND |
| Window geometry | `set_window_frame` moved/resized visible bounds from `(87,80,662,412)` to `(127,100,702,432)` exactly; original bounds restored |
| Credential rotation | DPAPI CurrentUser provisioning of token B followed by `/init` invalidated A; B admitted MCP |
| Header separation | S0 accepted `X-Access-Token` and rejected Bearer alone; CUA rejected `X-Access-Token` alone |

The successful final input screenshot SHA-256 is
`887c34bd18a6a4de39bf5cbea3046a714108479184127b4e2e51cd9e4ab54b0f`.
Images are retained only in local test evidence, not added to this repository.
The test application and runtime were stopped before service-mode testing.

### Native live-reset limitation

Unauthorized `/init {}` with an installed token returned 401 and preserved the
current token. Provisioning an encrypted Windows metadata record with an empty
`accessToken` and attempting the same reset also returned 401. The backend
currently treats empty metadata tokens as unprovisioned; it cannot supply the
SHA-512 of the empty token required by the existing reset authorization policy.

Consequently **authorized live credential reset is not established on the
native Windows provisioning path**. Existing synthetic-provider reset tests do
not substitute for this native check. Rotation works; reset refusal preserves
access control. This campaign records the limitation without changing the
frozen S0 authentication policy or weakening admission.

## SCM and user-session helper

The base executable was registered with the actual Service Control Manager
as `coorworker-envd`, running as LocalSystem in Session 0. It listened only on
`127.0.0.1:49991` with `COORWORKER_CUA_MODE=shared-external`. A logon scheduled
task launched the standalone helper as the ordinary fixture account in
Session 1, listening on `127.0.0.1:49990`. The host forwarded only envd's port;
all successful client MCP calls used the envd frontdoor and current envd token.

| Observation | Result |
| --- | --- |
| SCM lifecycle | Start, interrogate, stop, restart and final stop succeeded; running service PIDs belonged to Session 0 |
| Native MCP discovery | 60 tools from the actual Windows provider |
| Graphical operation through service | Guarded app launch, 1280×800 capture, marker check, button counter 0→1 and typed nonce all passed |
| Credential boundaries | S0/CUA header separation passed; the canonical envd token received 401 when presented directly to the helper |
| Helper stopped | CUA health became 503; S0 health remained 204; worker-up gauge became 0 and no replacement worker appeared |
| Helper restarted by deployment fixture | MCP recovered; observed-recovery counter increased by exactly 1; supervisor-restart counter remained 0 |
| Service stopped | Frontdoor listener disappeared while the independently managed helper remained alive |
| Service restarted | CUA rejected the old token before initialization; after `/init`, envd readopted the existing helper and MCP succeeded |
| Listener inventory | Exactly two listeners for the selected ports, both loopback, owned respectively by service and helper |
| Final cleanup | Service stopped, helper task ready, zero campaign runtime/GUI processes or selected-port listeners; no fixture failure file |

The successful service final-input screenshot SHA-256 is
`5d6848165814348bee87e44f81b1169ff269fd0014df24f1e55aa2ee99de4420`.
The independent fixture JSON and inspected screenshot both show counter 1 and
the same unique input nonce. A tool's reported input delivery alone was not
accepted as evidence of the application effect.

The helper's private transport token was generated inside the disposable guest;
it was not the canonical envd token. The token file allowed the fixture user
read access, with SYSTEM/Administrators full control. Before writing the
credential-bearing service environment, the service registry key was restricted
to SYSTEM/Administrators. Runtime children used an environment allowlist; the
SCM fixture masked non-runtime variables enumerated from the machine registry.
That mask does not establish a complete audit of SCM's cached startup environment
or constitute a production installer. No model/provider credentials were
supplied to the test daemons.

Two failed service attempts are retained. The original `Move-Item -Force`
state publisher raised an existing-file error, and its first `File.Replace`
revision raised an illegal-path error with PowerShell's `$null` backup
argument. Both produced modal fixture errors and failed the independent counter
assertion. The final fixture uses a concrete backup path, verified with repeated
native replacements, bounded I/O retries, and base-exception recording/closure.
The complete unchanged service assertions then passed with exit 0. These are
test-fixture corrections; no production CUA source was changed.

## Local receipts and limits

The evidence root is `/tmp/envd-windows-cua-e2e-tf_mzs88`; the copy's fixture
root is `C:\envd\cua-e2e-tf_mzs88`. Local receipts include the live-backup
completion, sanitized clone/network XML, setup/account identity, runtime
identities and executable hashes, actual MCP tool schemas, capture metadata,
PNG files, independent fixture state, UIA/window observations and retained
failed fixture attempts. These temporary paths are not durable downloads or
published artifacts. After collecting receipts, the copy shut down cleanly;
its disk, isolated network, NVRAM and TPM state were removed through libvirt.
The original VM remained running with its original UUID. The final host cleanup
receipt is `host-cleanup.json`.

This fixture demonstrates selected primary-display actions on one Windows 11
configuration. It does not establish every registered tool, multiple monitors,
UAC/secure-desktop interaction, arbitrary packaged application behavior or a
Windows guest-image/Cloud Hypervisor pipeline. It does not inspect or operate
the original VM's user desktop. Remote CI and delivery evidence are recorded
separately from these native observations.
