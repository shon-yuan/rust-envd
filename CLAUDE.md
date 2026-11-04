# rust-envd

Read [WIKI.md](WIKI.md) and [wiki/index.md](wiki/index.md) before architecture, module or interface work, then verify the relevant source. Build and platform conventions are documented in README.md.

Preserve the frozen S0 protocol and existing vendor patches. CUA is additive; keep desktop dependencies optional and model/provider credentials in AgentHost. Record vendor changes in their provenance and regenerate dependency notices when the resolved graph changes.

Preserve existing uncommitted work. Use Cargo `-j4` and run compilation jobs sequentially. Desktop tests must use isolated fixtures unless the user authorizes a specific real desktop action. Report dependency advisories and platform acceptance limits accurately.
