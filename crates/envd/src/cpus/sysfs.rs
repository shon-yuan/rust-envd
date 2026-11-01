// SPDX-License-Identifier: Apache-2.0
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

pub trait CpuSysfs: Send + Sync {
    fn possible(&self) -> io::Result<Vec<usize>>;
    fn online(&self) -> io::Result<Vec<usize>>;
    fn set_online(&self, cpu: usize, online: bool) -> io::Result<()>;
}

/// Owns a synchronous helper write through its completion. Implementations must
/// never kill a pending helper when its caller's future is cancelled.
pub trait ChildWrites: Send + Sync {
    fn write(&self, path: &Path, online: bool) -> io::Result<()>;
    fn wait_adopted(&self);
}

pub struct KernelSysfs {
    root: PathBuf,
    children: Arc<dyn ChildWrites>,
}

impl KernelSysfs {
    pub fn new(root: impl Into<PathBuf>, children: Arc<dyn ChildWrites>) -> Self {
        Self {
            root: root.into(),
            children,
        }
    }

    fn read_mask(&self, name: &str) -> io::Result<Vec<usize>> {
        let bytes = read_bounded(&self.root.join(name), 65536, name)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid("CPU mask is not UTF-8"))?;
        parse_mask(text.trim())
    }
}

impl CpuSysfs for KernelSysfs {
    fn possible(&self) -> io::Result<Vec<usize>> {
        self.read_mask("possible")
    }
    fn online(&self) -> io::Result<Vec<usize>> {
        self.read_mask("online")
    }
    fn set_online(&self, cpu: usize, online: bool) -> io::Result<()> {
        self.children
            .write(&self.root.join(format!("cpu{cpu}/online")), online)
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn read_bounded(path: &Path, limit: usize, label: &str) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid(format!("{label}: longer than {limit} bytes")));
    }
    Ok(bytes)
}

fn parse_cpu(text: &str) -> io::Result<usize> {
    let value = text.parse::<usize>().ok().filter(|value| *value < 8192);
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid("not a CPU number below 8192"));
    }
    value.ok_or_else(|| invalid("not a CPU number below 8192"))
}

pub fn parse_mask(text: &str) -> io::Result<Vec<usize>> {
    if text.is_empty() {
        return Ok(vec![]);
    }
    let mut cpus = Vec::new();
    let mut next = 0;
    for part in text.split(',') {
        let (low, high) = part
            .split_once('-')
            .map_or((part, None), |(low, high)| (low, Some(high)));
        let label = format!(
            "cpu list range {:?}",
            part.chars().take(16).collect::<String>()
        );
        let start = parse_cpu(low).map_err(|error| invalid(format!("{label}: {error}")))?;
        let end = high
            .map(parse_cpu)
            .transpose()
            .map_err(|error| invalid(format!("{label}: {error}")))?
            .unwrap_or(start);
        if start < next || end < start {
            return Err(invalid(format!("{label}: out of order")));
        }
        cpus.extend(start..=end);
        next = end + 1;
    }
    Ok(cpus)
}

fn clean(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir if result.file_name().is_some_and(|name| name != "..") => {
                result.pop();
            }
            Component::ParentDir if !result.has_root() => result.push(".."),
            Component::CurDir | Component::ParentDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

/// Reexec helper body. Call only after the binary validates its root guest/helper
/// dispatch mode. Does not create attributes, and never runs implicitly here.
pub fn write_online_file(args: &[String]) -> io::Result<()> {
    if args.len() != 2 || !matches!(args[1].as_str(), "0" | "1") {
        return Err(invalid("usage: cpu-online PATH 0|1"));
    }
    let path = clean(Path::new(&args[0]));
    let cpu = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("cpu"));
    if path.file_name().is_none_or(|name| name != "online")
        || cpu.is_none_or(|cpu| parse_cpu(cpu).is_err())
    {
        return Err(invalid(format!(
            "{:?} is not a cpuN/online file",
            args[0].chars().take(64).collect::<String>()
        )));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&args[0])?;
    file.write_all(args[1].as_bytes())?;
    // Explicit close reports close failures as the upstream helper does.
    use std::os::fd::IntoRawFd;
    let fd = file.into_raw_fd();
    // SAFETY: fd has just been transferred from File and is closed exactly once.
    if unsafe { nix::libc::close(fd) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
