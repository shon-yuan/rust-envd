# Wiki — rust-envd

Start with [wiki/index.md](wiki/index.md), then verify relevant source and tests. Existing documents remain the source for protocol, platform and distribution details.

rust-envd implements the pinned official envd surface in Rust, with additive CUA modes. The default build excludes desktop dependencies; the optional desktop build reuses the standalone worker's MCP implementation. Model/provider credentials belong in AgentHost.
