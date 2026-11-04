//! Windows staging for `POST /upgrade`.
//!
//! The oracle writes the delivered binary to a fixed path and execs it. The
//! Windows equivalents differ in exactly one way: a running `.exe` cannot be
//! deleted, but it CAN be renamed. When the fixed path is the image this
//! process runs from, the live image is renamed to `<name>.old-<pid>` first,
//! the new bytes are written to the canonical path, and the successor removes
//! the stale image on startup. Every other error is reported with the same
//! `Display` text the Linux arm would produce.

use std::path::Path;

use axum::body::Body;

/// `ERROR_ACCESS_DENIED` and `ERROR_SHARING_VIOLATION`: the two codes a delete
/// of the running image (or a file another process holds open) reports.
const ERROR_ACCESS_DENIED: i32 = 5;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// Streams the delivered binary to the fixed upgrade path.
pub async fn stage_binary(body: Body, path: &Path) -> Result<(), String> {
    use futures_util::StreamExt;
    // The installer normally owns `%ProgramData%\e2b`; creating it here keeps
    // a fresh guest (and the in-process tests) working without one.
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) if is_held_open(&error) => {
            // The canonical path is this process's own image (or a file held
            // open without delete sharing). Windows permits renaming a
            // running image, so move it aside and let the successor delete it.
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "envd.next.exe".to_owned());
            let stale = path.with_file_name(format!("{name}.old-{}", std::process::id()));
            let _ = tokio::fs::remove_file(&stale).await;
            tokio::fs::rename(path, &stale)
                .await
                .map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .await
        .map_err(|error| error.to_string())?;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| error.to_string())?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|error| error.to_string())?;
    }
    // Same flush seam as the Linux arm: tokio's fs writes are queued on the
    // blocking pool, and the successor must not start before every byte is in
    // the file.
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Whether a delete failed because the target is open (running image or a
/// file without delete sharing) rather than because of a bad path.
fn is_held_open(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
    )
}

#[cfg(test)]
mod tests {
    use super::stage_binary;

    /// The same flush discipline as the Linux arm: once the write future
    /// resolves, every delivered byte is in the file.
    #[tokio::test]
    async fn a_completed_write_holds_every_byte_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("envd.next.exe");
        const CHUNKS: usize = 64;
        const CHUNK: usize = 1024;
        for round in 0..50 {
            let body = axum::body::Body::from_stream(futures_util::stream::iter(
                (0..CHUNKS)
                    .map(|index| Ok::<_, std::io::Error>(vec![b'a' + (index % 26) as u8; CHUNK])),
            ));
            stage_binary(body, &path).await.expect("write");
            let on_disk = std::fs::metadata(&path)
                .expect("the staged upgrade binary exists")
                .len();
            assert_eq!(
                on_disk as usize,
                CHUNKS * CHUNK,
                "round {round}: the write future resolved before the staged binary held every byte"
            );
        }
    }

    /// A running image cannot be deleted but can be renamed: staging over it
    /// moves the live image to `<name>.old-<pid>` and writes the new bytes to
    /// the canonical path, with no sharing violation escaping.
    #[tokio::test]
    async fn stage_binary_replaces_a_running_image_by_rename() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("envd.next.exe");
        let system_cmd = std::path::Path::new(r"C:\Windows\System32\cmd.exe");
        std::fs::copy(system_cmd, &path).expect("copy cmd.exe");
        // Keep the copied image loaded while staging. `cmd /c ping` stays
        // resident for about five seconds, which bounds the whole test.
        let mut child = std::process::Command::new(&path)
            .args(["/c", "ping", "-n", "6", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the copied image");
        let staged = stage_binary(axum::body::Body::from(b"new image".to_vec()), &path).await;
        let _ = child.kill();
        let _ = child.wait();
        staged.expect("staging over a running image must rename, not fail");
        assert_eq!(std::fs::read(&path).unwrap(), b"new image");
        let stale = path.with_file_name(format!("envd.next.exe.old-{}", std::process::id()));
        assert!(
            stale.exists(),
            "the running image is preserved under {}",
            stale.display()
        );
        assert_ne!(std::fs::read(&stale).unwrap(), b"new image");
    }
}
