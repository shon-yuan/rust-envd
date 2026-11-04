//! Linux staging for `POST /upgrade`: the oracle's fixed-path binary write.
//!
//! Moved verbatim from `daemon.rs` (`write_upgrade_binary`) so the shared
//! handler can stay platform-neutral. The semantics are Go's `main.go:447-468`
//! exactly: unlink first, create `0o755`, stream, flush.

use std::path::Path;

use axum::body::Body;

/// Streams the delivered binary to the fixed upgrade path.
///
/// Unlink-first matters on a chained upgrade: this envd runs FROM that path,
/// so `O_TRUNC` on the live image fails with `ETXTBSY`; the create then lands
/// on a fresh inode while the running process keeps its unlinked image.
pub async fn stage_binary(body: Body, path: &Path) -> Result<(), String> {
    use futures_util::StreamExt;
    let _ = tokio::fs::remove_file(path).await;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o755)
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
    // Same seam as `cacerts::prepare_file`: tokio's fs File reports write_all
    // complete once the write is QUEUED on the blocking pool, so without this
    // await a straggler chunk can still be in flight when the response (and,
    // on the upgrade path, the swap) goes out. Go streams through a
    // synchronous io.Copy and returns with every byte in the file; flush is
    // the await that waits for the write syscall itself.
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|error| error.to_string())?;
    // While this write fd is open, a concurrently forked child of this
    // process inherits it (O_CLOEXEC closes only at that child's execve), so
    // an exec of this image racing a fork can fail ETXTBSY. Go's io.Copy
    // holds the same window; parity behavior, not a defect.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::stage_binary;

    /// Go main.go:447-468 streams the binary through a synchronous
    /// `io.Copy`/`WriteFile`: when the handler returns, the staged file holds
    /// every byte. Tokio's `fs::File` completes `write_all` when the write is
    /// QUEUED on the blocking pool (fs/file.rs `poll_write` spawns the task and
    /// returns Ready); the awaited `flush` is what waits for the syscall.
    /// Without it the response can return while the last chunk is still in
    /// flight, so a caller that stats or execs the staged image immediately can
    /// observe a truncated binary. Each round writes a multi-chunk body and
    /// reads the length back as soon as the write future resolves; the same
    /// class was captured in `cacerts::prepare_file` (wrote=N on_disk=0).
    #[tokio::test]
    async fn a_completed_write_holds_every_byte_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("envd.next");
        const CHUNKS: usize = 64;
        const CHUNK: usize = 1024;
        for round in 0..200 {
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

    /// The staged image is executable (Go's `os.WriteFile(path, data, 0o755)`).
    #[tokio::test]
    async fn the_staged_image_is_created_executable() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("envd.next");
        stage_binary(axum::body::Body::from(b"#!/bin/sh\n".to_vec()), &path)
            .await
            .expect("write");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}
