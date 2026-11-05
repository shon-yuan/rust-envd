#![cfg(all(target_os = "linux", feature = "cua-desktop"))]

//! Exercise the shipped envd native CUA API without connecting to the host desktop.
//! Workspace projections follow Hyprland v0.56.2 src/debug/HyprCtl.cpp and
//! v0.57.0 src/ipc/s1/Commands.cpp (getMonitorData/getWindowData).

use serde_json::{json, Value};
use std::fs::{self, DirBuilder};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SIGNATURE: &str = "0123456789abcdef_1700000000_123456789";
const TOKEN: &str = "wayland-native-test-credential";
const IO_TIMEOUT: Duration = Duration::from_secs(5);

struct PrivateDir(PathBuf);

impl PrivateDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(format!(
            "/tmp/cew-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        let dir = Self(path);
        fs::create_dir(dir.0.join("home")).unwrap();
        fs::create_dir_all(dir.0.join("hypr").join(SIGNATURE)).unwrap();
        dir
    }
}

impl Drop for PrivateDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct SocketThread {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl SocketThread {
    fn start(path: &Path, mut serve: impl FnMut(UnixStream) + Send + 'static) -> Self {
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => serve(stream),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::park_timeout(Duration::from_millis(5));
                    }
                    Err(error) => panic!("private listener failed: {error}"),
                }
            }
        });
        Self {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for SocketThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
struct Script {
    monitors: String,
    clients: String,
    queries: Vec<String>,
    unexpected: Vec<String>,
}

struct Compositor {
    script: Arc<Mutex<Script>>,
    ipc_path: PathBuf,
    _ipc: SocketThread,
    _wayland: SocketThread,
}

impl Compositor {
    fn new(root: &Path, monitors: Value, clients: Value) -> Self {
        let script = Arc::new(Mutex::new(Script {
            monitors: monitors.to_string(),
            clients: clients.to_string(),
            ..Script::default()
        }));
        let ipc_path = root.join("hypr").join(SIGNATURE).join(".socket.sock");
        let ipc_script = script.clone();
        let ipc = SocketThread::start(&ipc_path, move |stream| {
            if let Err(error) = serve_ipc(stream, &ipc_script) {
                ipc_script
                    .lock()
                    .unwrap()
                    .unexpected
                    .push(error.to_string());
            }
        });
        // The backend compares SO_PEERCRED on both sockets. No Wayland protocol
        // exchange is needed for that check; protocol bytes indicate fallback.
        let wayland_script = script.clone();
        let wayland = SocketThread::start(&root.join("fake"), move |mut stream| {
            stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            if let Ok(count) = stream.read(&mut [0; 256]) {
                if count > 0 {
                    wayland_script
                        .lock()
                        .unwrap()
                        .unexpected
                        .push("unexpected Wayland protocol probe/fallback".into());
                }
            }
        });
        Self {
            script,
            ipc_path,
            _ipc: ipc,
            _wayland: wayland,
        }
    }

    fn replace(&self, monitors: Value, clients: Value) {
        self.raw_replies(monitors.to_string(), clients.to_string());
    }

    fn raw_replies(&self, monitors: String, clients: String) {
        let mut script = self.script.lock().unwrap();
        script.monitors = monitors;
        script.clients = clients;
    }

    fn assert_no_fallback(&self) {
        let script = self.script.lock().unwrap();
        assert!(script.unexpected.is_empty(), "{:?}", script.unexpected);
    }
}

fn serve_ipc(mut stream: UnixStream, script: &Mutex<Script>) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    let mut command = Vec::new();
    let mut byte = [0];
    loop {
        if stream.read(&mut byte)? == 0 {
            // An attestation failure may close IPC before sending any query.
            return Ok(());
        }
        command.push(byte[0]);
        if command == b"j/monitors" || command == b"j/clients" {
            break;
        }
        if !b"j/monitors".starts_with(&command) && !b"j/clients".starts_with(&command) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "non-query IPC command",
            ));
        }
    }
    let reply = {
        let mut script = script.lock().unwrap();
        script
            .queries
            .push(String::from_utf8(command.clone()).unwrap());
        if command == b"j/monitors" {
            script.monitors.clone()
        } else {
            script.clients.clone()
        }
    };
    stream.write_all(reply.as_bytes())
}

struct Envd {
    child: Child,
    stdout: Option<JoinHandle<()>>,
    address: SocketAddr,
}

impl Envd {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_coorworker-envd"))
            .args(["--development", "--listen", "127.0.0.1:0"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.join("home"))
            .env("XDG_RUNTIME_DIR", root)
            .env("WAYLAND_DISPLAY", "fake")
            .env("CUA_DRIVER_RS_ENABLE_WAYLAND", "1")
            .env("HYPRLAND_INSTANCE_SIGNATURE", SIGNATURE)
            .env("TOKIO_WORKER_THREADS", "2")
            .current_dir(root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (ready, receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(address) = line
                    .strip_prefix("envd listening ")
                    .and_then(|address| address.parse::<SocketAddr>().ok())
                {
                    let _ = ready.send(address);
                }
            }
        });
        // Own the process before waiting, so readiness and /init failures clean up.
        let mut envd = Self {
            child,
            stdout: Some(reader),
            address: "127.0.0.1:0".parse().unwrap(),
        };
        envd.address = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("envd did not print its actual listening address");
        let body = envd.post("/init", json!({"accessToken": TOKEN}), None, 204);
        assert!(body.is_empty(), "unexpected /init body: {body}");
        envd
    }

    fn post(&self, path: &str, payload: Value, token: Option<&str>, status: u16) -> String {
        let body = payload.to_string();
        let mut stream = TcpStream::connect_timeout(&self.address, IO_TIMEOUT).unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        stream.set_write_timeout(Some(IO_TIMEOUT)).unwrap();
        write!(stream, "POST {path} HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            self.address, body.len()).unwrap();
        if let Some(token) = token {
            write!(stream, "X-Access-Token: {token}\r\n").unwrap();
        }
        write!(stream, "\r\n{body}").unwrap();
        let mut bytes = Vec::new();
        stream.take(1024 * 1024).read_to_end(&mut bytes).unwrap();
        let response = String::from_utf8(bytes).unwrap();
        let (headers, body) = response
            .split_once("\r\n\r\n")
            .expect("HTTP response headers");
        assert!(
            headers.starts_with(&format!("HTTP/1.1 {status} ")),
            "{path}: {response}"
        );
        body.to_owned()
    }

    fn list(&self, arguments: Value) -> Value {
        let body = self.post(
            "/cua/call",
            json!({"name":"list_windows", "arguments":arguments}),
            Some(TOKEN),
            200,
        );
        let value: Value = serde_json::from_str(&body).expect("native JSON HTTP response");
        assert!(value["success"].is_boolean(), "{value}");
        assert!(value["content"].is_array(), "{value}");
        for legacy in ["jsonrpc", "id", "result", "isError", "structuredContent"] {
            assert!(value.get(legacy).is_none(), "legacy {legacy}: {value}");
        }
        value
    }
}

impl Drop for Envd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.stdout.take() {
            let _ = reader.join();
        }
    }
}

struct Fixture {
    // Drop the child before its private listeners, and listeners before paths.
    envd: Envd,
    compositor: Compositor,
    _dir: PrivateDir,
}

impl Fixture {
    fn new(monitors: Value, clients: Value) -> Self {
        let dir = PrivateDir::new();
        let compositor = Compositor::new(&dir.0, monitors, clients);
        let envd = Envd::start(&dir.0);
        Self {
            envd,
            compositor,
            _dir: dir,
        }
    }
}

fn monitor(active: Value, special: Value) -> Value {
    json!([{
        "id":0, "name":"FAKE-1", "width":1920, "height":1080,
        "x":0, "y":0, "scale":1.0, "transform":0, "dpmsStatus":true,
        "activeWorkspace":active, "specialWorkspace":special
    }])
}

fn old_monitors() -> Value {
    monitor(json!({"id":1,"name":"1"}), json!({"id":0,"name":""}))
}

fn absent_special() -> Value {
    json!({"address":"", "type":"", "name":""})
}

fn numbered() -> Value {
    json!({"address":"1", "id":1, "type":"normal", "name":"Shared"})
}

fn client(address: &str, workspace: Value) -> Value {
    json!({
        "address":address, "mapped":true, "hidden":false,
        "pid":std::process::id(), "title":"Fixture '窗口'", "class":"fixture.editor",
        "at":[-1830,38], "size":[800,600], "workspace":workspace,
        "xwayland":false, "focusHistoryID":0
    })
}

fn windows(result: &Value) -> Vec<Value> {
    assert_eq!(result["success"], true, "{result}");
    result["data"]["windows"]
        .as_array()
        .unwrap_or_else(|| panic!("missing native windows data: {result}"))
        .clone()
}

fn ids(windows: &[Value]) -> Vec<u64> {
    let mut ids: Vec<_> = windows
        .iter()
        .map(|w| w["window_id"].as_u64().unwrap())
        .collect();
    ids.sort_unstable();
    ids
}

fn assert_geometry(window: &Value, address: u64) {
    assert_eq!(window["window_id"], address);
    assert_eq!(window["pid"], std::process::id());
    assert_eq!(window["title"], "Fixture '窗口'");
    assert_eq!(window["app_name"], "fixture.editor");
    assert_eq!(
        window["bounds"],
        json!({"x":-1830,"y":38,"width":800,"height":600})
    );
    for field in ["x", "y", "width", "height"] {
        assert_eq!(window[field], window["bounds"][field], "{field}");
    }
    assert_eq!(window["z_index"], Value::Null);
}

#[test]
fn legacy_numeric_workspaces_preserve_ids_geometry_and_tool_filters() {
    let clients = json!([
        client("0x100000010", json!({"id":1,"name":"1"})),
        client("0x200000010", json!({"id":2,"name":"2"}))
    ]);
    let fixture = Fixture::new(old_monitors(), clients.clone());
    let all = windows(&fixture.envd.list(json!({})));
    assert_eq!(ids(&all), [0x100000010, 0x200000010]);
    assert_geometry(&all[0], 0x100000010);
    assert_geometry(&all[1], 0x200000010);
    assert_eq!(all[0]["is_on_screen"], true);
    assert_eq!(all[1]["is_on_screen"], false);
    assert_eq!(
        ids(&windows(&fixture.envd.list(json!({"on_screen_only":true})))),
        [0x100000010]
    );

    // Both owners are live; filtering cannot pass merely by dropping dead PIDs.
    let mut other_owner = clients.clone();
    other_owner[1]["pid"] = json!(fixture.envd.child.id());
    fixture.compositor.replace(old_monitors(), other_owner);
    assert_eq!(
        ids(&windows(
            &fixture.envd.list(json!({"pid":std::process::id()}))
        )),
        [0x100000010]
    );
    assert_eq!(
        ids(&windows(
            &fixture.envd.list(json!({"pid":fixture.envd.child.id()}))
        )),
        [0x200000010]
    );
    fixture.compositor.replace(
        old_monitors(),
        json!([clients[1].clone(), clients[0].clone()]),
    );
    assert_eq!(
        ids(&windows(&fixture.envd.list(json!({})))),
        [0x100000010, 0x200000010]
    );
    fixture.compositor.assert_no_fallback();
}

#[test]
fn hyprland_057_uses_workspace_addresses_and_accepts_absent_special_workspace() {
    let named = json!({"address":"project", "type":"normal", "name":"Shared"});
    let special = json!({"address":"special:project", "type":"special", "name":"Shared"});
    let other = json!({"address":"other", "type":"normal", "name":"Shared"});
    let clients = json!([
        client("0x100000010", numbered()),
        client("0x200000010", named.clone()),
        client("0x300000010", special.clone()),
        client("0x400000010", other)
    ]);
    let fixture = Fixture::new(monitor(numbered(), absent_special()), clients.clone());
    let all = windows(&fixture.envd.list(json!({})));
    assert_eq!(
        ids(&all),
        [0x100000010, 0x200000010, 0x300000010, 0x400000010],
        "v0.57 workspace objects must not become a successful empty list"
    );
    assert_eq!(
        all.iter()
            .map(|w| w["is_on_screen"].clone())
            .collect::<Vec<_>>(),
        [json!(true), json!(false), json!(false), json!(false)]
    );
    assert_geometry(&all[0], 0x100000010);
    assert_eq!(
        ids(&windows(&fixture.envd.list(json!({"on_screen_only":true})))),
        [0x100000010]
    );

    fixture
        .compositor
        .replace(monitor(named, special), clients.clone());
    let visible = windows(
        &fixture
            .envd
            .list(json!({"pid":std::process::id(), "on_screen_only":true})),
    );
    assert_eq!(
        ids(&visible),
        [0x200000010, 0x300000010],
        "identical display names must not match different workspace addresses"
    );
    let mut hidden = clients;
    hidden[1]["hidden"] = json!(true);
    hidden[2]["mapped"] = json!(false);
    fixture.compositor.replace(
        monitor(
            json!({"address":"project","type":"normal","name":"Shared"}),
            absent_special(),
        ),
        hidden,
    );
    assert!(windows(&fixture.envd.list(json!({"on_screen_only":true}))).is_empty());
    fixture.compositor.assert_no_fallback();
}

#[test]
fn genuine_empty_desktop_is_a_successful_empty_list() {
    let fixture = Fixture::new(old_monitors(), json!([]));
    for monitors in [old_monitors(), json!([])] {
        fixture.compositor.replace(monitors, json!([]));
        let result = fixture.envd.list(json!({}));
        assert!(windows(&result).is_empty());
        assert!(result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Found 0 windows"));
    }
    let script = fixture.compositor.script.lock().unwrap();
    assert!(script.queries.iter().any(|query| query == "j/monitors"));
    assert!(script.queries.iter().any(|query| query == "j/clients"));
    drop(script);
    fixture.compositor.assert_no_fallback();
}

fn assert_enumeration_error(result: &Value, context: &str) {
    assert_eq!(
        result["success"], false,
        "{context}: authoritative Hyprland failure must be a native tool error: {result}"
    );
    let data = &result["data"];
    assert!(
        data["code"].as_str().is_some_and(|code| !code.is_empty()),
        "{context}: expected an error code in native data: {result}"
    );
    assert!(data.get("windows").is_none(), "{context}: {result}");
    let text = result["content"][0]["text"].as_str().expect("error text");
    assert!(
        !text.is_empty() && !text.contains("Found 0 windows"),
        "{context}: {result}"
    );
    assert!(
        text.to_lowercase().contains("hyprland"),
        "{context}: backend context lost: {result}"
    );
}

#[test]
fn absent_hyprland_ipc_is_a_native_error_without_backend_fallback() {
    let fixture = Fixture::new(old_monitors(), json!([]));
    fs::remove_file(&fixture.compositor.ipc_path).unwrap();
    let result = fixture.envd.list(json!({}));
    assert_enumeration_error(&result, "missing IPC socket");
    fixture.compositor.assert_no_fallback();
}

#[test]
fn malformed_json_and_missing_required_identity_are_native_errors() {
    let valid_clients = json!([client("0x100000010", json!({"id":1,"name":"1"}))]);
    let fixture = Fixture::new(old_monitors(), valid_clients.clone());
    // Establish a previous successful snapshot: an authoritative failure must
    // neither return stale windows nor fall back to an unrelated provider.
    assert_eq!(windows(&fixture.envd.list(json!({}))).len(), 1);
    let mut missing_address = valid_clients.clone();
    missing_address[0]
        .as_object_mut()
        .unwrap()
        .remove("address");
    let mut missing_workspace = valid_clients.clone();
    missing_workspace[0]["workspace"] = json!({"name":"1"});
    let cases = [
        (
            "malformed monitor JSON",
            "{".into(),
            valid_clients.to_string(),
        ),
        (
            "malformed client JSON",
            old_monitors().to_string(),
            "{".into(),
        ),
        (
            "invalid client reply type",
            old_monitors().to_string(),
            "null".into(),
        ),
        (
            "mapped client with empty address identity",
            old_monitors().to_string(),
            json!([client("0x100000010", absent_special())]).to_string(),
        ),
        (
            "mapped client with empty legacy identity",
            old_monitors().to_string(),
            json!([client("0x100000010", json!({"id":0,"name":""}))]).to_string(),
        ),
        (
            "missing active workspace identity",
            monitor(json!({"name":"1"}), json!({"id":0,"name":""})).to_string(),
            valid_clients.to_string(),
        ),
        (
            "missing window address",
            old_monitors().to_string(),
            missing_address.to_string(),
        ),
        (
            "missing client workspace identity",
            old_monitors().to_string(),
            missing_workspace.to_string(),
        ),
    ];
    let mut failures = Vec::new();
    for (context, monitors, clients) in cases {
        fixture.compositor.raw_replies(monitors, clients);
        let result = fixture.envd.list(json!({}));
        // Collect every regression signature instead of stopping at row 1.
        if std::panic::catch_unwind(|| assert_enumeration_error(&result, context)).is_err() {
            failures.push(format!("{context}: {result}"));
        }
    }
    fixture.compositor.assert_no_fallback();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
