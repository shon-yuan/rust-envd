fn main() -> Result<(), Box<dyn std::error::Error>> {
    connectrpc_build::Config::new()
        .files(&[
            "third_party/e2b/spec/process/process.proto",
            "third_party/e2b/spec/filesystem/filesystem.proto",
            "third_party/e2b/spec/upgrade/handover.proto",
            "third_party/e2b/legacy/legacyfilesystem.proto",
            "third_party/e2b/legacy/legacyprocess.proto",
        ])
        .includes(&["third_party/e2b/spec", "third_party/e2b/legacy"])
        .include_file("_connectrpc.rs")
        .emit_descriptor_set("envd_descriptor.bin")
        .compile()?;
    Ok(())
}
