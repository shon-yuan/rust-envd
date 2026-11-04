//! `coorworker-cua` — the Cua computer-use worker supervised by rust-envd's
//! `cua` extension.
//!
//! ```text
//! coorworker-cua [--listen 127.0.0.1:3211] [--token-file PATH]
//! ```
//!
//! `CUA_ENV_TOKEN` supplies the bearer token when `--token-file` is absent,
//! matching the upstream daemon's environment contract. A non-loopback bind
//! without a token is refused, and the token is never printed.

use coorworker_cua::{app, desktop_provider, Auth};
use std::net::SocketAddr;
use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("coorworker-cua: no runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coorworker-cua: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), String> {
    let args = Args::parse(std::env::args().skip(1))?;
    let listen: SocketAddr = args
        .listen
        .parse()
        .map_err(|error| format!("invalid --listen address: {error}"))?;

    let token = args.token()?;
    if !listen.ip().is_loopback() && token.is_none() {
        return Err(format!(
            "refusing to bind {listen} without a token (CUA_ENV_TOKEN or --token-file)"
        ));
    }
    let auth = match token {
        Some(token) => Auth::bearer(token),
        None => Auth::disabled(),
    };
    let has_token = auth.is_enabled();

    let router = app(desktop_provider(), auth);
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|error| format!("cannot bind {listen}: {error}"))?;
    let local = listener
        .local_addr()
        .map_err(|error| format!("cannot read local address: {error}"))?;
    eprintln!(
        "coorworker-cua: MCP on http://{local}/mcp (auth {})",
        if has_token {
            "bearer"
        } else {
            "disabled on loopback"
        }
    );

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|error| format!("serve: {error}"))
}

async fn shutdown() {
    let terminate = async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            if let Ok(mut stream) = signal(SignalKind::terminate()) {
                stream.recv().await;
            }
        }
        #[cfg(not(unix))]
        {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {},
        _ = terminate => {},
    }
}

struct Args {
    listen: String,
    token_file: Option<PathBuf>,
}

impl Args {
    fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Self, String> {
        let mut args = Self {
            listen: "127.0.0.1:3211".to_owned(),
            token_file: None,
        };
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--listen" => {
                    args.listen = arguments
                        .next()
                        .ok_or_else(|| "--listen needs an address".to_owned())?;
                }
                "--token-file" => {
                    let path = arguments
                        .next()
                        .ok_or_else(|| "--token-file needs a path".to_owned())?;
                    args.token_file = Some(PathBuf::from(path));
                }
                "-h" | "--help" => {
                    println!("usage: coorworker-cua [--listen 127.0.0.1:3211] [--token-file PATH]");
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        Ok(args)
    }

    /// `--token-file` wins over `CUA_ENV_TOKEN`; an empty file is an error
    /// rather than a silent tokenless bind.
    fn token(&self) -> Result<Option<Vec<u8>>, String> {
        if let Some(path) = &self.token_file {
            let token = std::fs::read(path)
                .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
            let token = trim(token);
            if token.is_empty() {
                return Err(format!("{} is empty", path.display()));
            }
            return Ok(Some(token));
        }
        match std::env::var("CUA_ENV_TOKEN") {
            Ok(value) => {
                let token = trim(value.into_bytes());
                if token.is_empty() {
                    return Err("CUA_ENV_TOKEN is empty".to_owned());
                }
                Ok(Some(token))
            }
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err("CUA_ENV_TOKEN is not valid UTF-8".to_owned())
            }
        }
    }
}

fn trim(mut token: Vec<u8>) -> Vec<u8> {
    while token.last().is_some_and(|byte| byte.is_ascii_whitespace()) {
        token.pop();
    }
    let start = token
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(token.len());
    token.drain(..start);
    token
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Result<Args, String> {
        Args::parse(values.iter().map(|value| (*value).to_owned()))
    }

    #[test]
    fn defaults_to_loopback() {
        assert_eq!(args(&[]).unwrap().listen, "127.0.0.1:3211");
    }

    #[test]
    fn parses_both_flags() {
        let parsed = args(&["--listen", "0.0.0.0:3211", "--token-file", "/run/cua/token"]).unwrap();
        assert_eq!(parsed.listen, "0.0.0.0:3211");
        assert_eq!(parsed.token_file.unwrap(), PathBuf::from("/run/cua/token"));
    }

    #[test]
    fn rejects_unknown_arguments() {
        assert!(args(&["--port", "1"]).is_err());
    }

    #[test]
    fn trims_whitespace_from_tokens() {
        assert_eq!(trim(b"\n secret \r\n".to_vec()), b"secret");
    }
}
