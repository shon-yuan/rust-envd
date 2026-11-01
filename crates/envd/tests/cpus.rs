#![cfg(target_os = "linux")]
use coorworker_envd::cpus::{
    parse_mask, write_online_file, ChildWrites, CpuManager, CpuSysfs, KernelSysfs, State,
};
use std::{
    fs, io,
    path::Path,
    sync::{Arc, Mutex},
};

struct NoChildren;
impl ChildWrites for NoChildren {
    fn write(&self, _: &Path, _: bool) -> io::Result<()> {
        panic!("unexpected child write")
    }
    fn wait_adopted(&self) {}
}

#[test]
fn masks_parse_ordered_ranges_and_bound_cpu_numbers() {
    assert_eq!(parse_mask("").unwrap(), Vec::<usize>::new());
    assert_eq!(parse_mask("0-1,4,6-7").unwrap(), vec![0, 1, 4, 6, 7]);
    assert_eq!(parse_mask("0-8191").unwrap().len(), 8192);
    for text in [
        "0-x",
        "0,,2",
        "0,",
        ",0",
        "-",
        "-1",
        "+1",
        "0x1",
        "1e3",
        "0 ,1",
        "1-2-3",
        "\0",
        "１",
        "3-1",
        "1,0",
        "0-3,2",
        "0-3,3",
        "8192",
        "0-8192",
        "99999999999999999999",
    ] {
        assert!(parse_mask(text).is_err(), "accepted {text:?}");
    }
    assert_eq!(
        parse_mask("3-1").unwrap_err().to_string(),
        "cpu list range \"3-1\": out of order"
    );
    assert_eq!(
        parse_mask("8192").unwrap_err().to_string(),
        "cpu list range \"8192\": not a CPU number below 8192"
    );
}

#[test]
fn read_masks_are_bounded_and_use_aggregate_files_only() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("possible"), "0-3\n").unwrap();
    fs::write(dir.path().join("online"), " 0-1 \n").unwrap();
    let sys = KernelSysfs::new(dir.path(), Arc::new(NoChildren));
    assert_eq!(sys.possible().unwrap(), vec![0, 1, 2, 3]);
    assert_eq!(sys.online().unwrap(), vec![0, 1]);
    fs::write(dir.path().join("online"), vec![b'0'; 65537]).unwrap();
    assert_eq!(
        sys.online().unwrap_err().to_string(),
        "online: longer than 65536 bytes"
    );
}

#[test]
fn helper_validates_path_and_bit_without_creating_attributes() {
    for args in [
        vec!["missing"],
        vec!["cpu2/online", "2"],
        vec!["cpuX/online", "1"],
        vec!["cpu-2/online", "1"],
        vec!["cpu8192/online", "1"],
        vec!["cpu2/uevent", "1"],
        vec!["/etc/passwd", "1"],
        vec!["cpu2/online/../../etc/passwd", "1"],
    ] {
        let error =
            write_online_file(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
                .unwrap_err()
                .to_string();
        assert!(error.contains("usage:") || error.contains("is not a cpuN/online file"));
    }
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("cpu2")).unwrap();
    let path = dir.path().join("cpu2/online");
    let args = vec![path.to_str().unwrap().to_owned(), "1".into()];
    assert_eq!(
        write_online_file(&args).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
    assert!(!path.exists());
    fs::write(&path, "0\n").unwrap();
    write_online_file(&args).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "1");
}

#[test]
fn noop_status_is_zero_and_any_target_is_accepted() {
    let noop = CpuManager::noop();
    for target in [i64::MIN, -1, 0, 64, i64::MAX] {
        noop.set_target(target).unwrap();
    }
    assert_eq!(noop.status(), State::default());
}

struct FakeSys {
    online: Mutex<Vec<usize>>,
    writes: Mutex<Vec<(usize, bool)>>,
}
impl CpuSysfs for FakeSys {
    fn possible(&self) -> io::Result<Vec<usize>> {
        Ok((0..8).collect())
    }
    fn online(&self) -> io::Result<Vec<usize>> {
        Ok(self.online.lock().unwrap().clone())
    }
    fn set_online(&self, cpu: usize, online: bool) -> io::Result<()> {
        assert_ne!(cpu, 0);
        self.writes.lock().unwrap().push((cpu, online));
        let mut cpus = self.online.lock().unwrap();
        cpus.retain(|item| *item != cpu);
        if online {
            cpus.push(cpu);
            cpus.sort_unstable();
        }
        Ok(())
    }
}

#[tokio::test]
async fn persisted_target_load_is_bounded_trimmed_and_validated() {
    for (content, expected) in [
        (" +0002 \n".to_owned(), 2),
        ("0".to_owned(), 0),
        ("-1".to_owned(), 0),
        ("9".to_owned(), 0),
        ("lots".to_owned(), 0),
        ("1".repeat(65), 0),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("target");
        fs::write(&path, content).unwrap();
        let sys = Arc::new(FakeSys {
            online: Mutex::new(vec![0, 1]),
            writes: Mutex::new(vec![]),
        });
        let manager = Arc::new(CpuManager::new(sys, Arc::new(NoChildren), path));
        let (cancel, stop) = tokio::sync::oneshot::channel();
        let run = manager.run(stop);
        tokio::pin!(run);
        tokio::select! { biased; _ = &mut run => panic!("unexpected stop"), _ = tokio::task::yield_now() => {} }
        assert_eq!(manager.status().target, expected);
        cancel.send(()).unwrap();
        run.await;
    }
}

#[tokio::test]
async fn manager_persists_target_then_grows_lowest_and_shrinks_highest() {
    let dir = tempfile::tempdir().unwrap();
    let sys = Arc::new(FakeSys {
        online: Mutex::new(vec![0, 1, 3, 5]),
        writes: Mutex::new(vec![]),
    });
    let manager = Arc::new(CpuManager::new(
        sys.clone(),
        Arc::new(NoChildren),
        dir.path().join("run/target"),
    ));
    manager.set_target(6).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("run/target")).unwrap(),
        "6"
    );
    assert_eq!(
        sys.online().unwrap(),
        vec![0, 1, 3, 5],
        "SetTarget must not write sysfs"
    );
    let (cancel, stop) = tokio::sync::oneshot::channel();
    let task = tokio::spawn({
        let manager = manager.clone();
        async move { manager.run(stop).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while manager.status().online != 6 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    manager.set_target(1).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while manager.status().online != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.send(()).unwrap();
    task.await.unwrap();
    assert_eq!(
        *sys.writes.lock().unwrap(),
        vec![
            (2, true),
            (4, true),
            (5, false),
            (4, false),
            (3, false),
            (2, false),
            (1, false)
        ]
    );
}
