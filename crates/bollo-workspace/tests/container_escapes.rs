#![cfg(windows)]
//! Adversarial containment tests for the Windows AppContainer backend.
//!
//! These do not test the happy path (that lives in the unit tests); they try to
//! escape the container: file symlinks and directory junctions, renames across
//! the grant boundary, alternate data streams on files outside the grant,
//! foreign inheritable handles, and processes that outlive the brokered child.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT};

use bollo_workspace::process::ExecRequest;
use bollo_workspace::sandbox_win::AppContainer;
use bollo_workspace::ChildSandbox;

struct Roots {
    root: PathBuf,
    granted: PathBuf,
    outside: PathBuf,
}

impl Roots {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("bollo-escape-test-{}-{tag}", std::process::id()));
        let granted = root.join("granted");
        let outside = root.join("outside");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Self {
            root,
            granted,
            outside,
        }
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        // `remove_dir_all` does not follow reparse points, so a planted
        // junction is removed, not traversed.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn cmd() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe")
}

fn request(words: &[&str], cwd: &Path, timeout_secs: u64) -> ExecRequest {
    let mut argv = vec![cmd().display().to_string(), "/C".into()];
    argv.extend(words.iter().map(|word| word.to_string()));
    ExecRequest {
        argv,
        cwd: cwd.to_path_buf(),
        timeout: Duration::from_secs(timeout_secs),
        env: std::collections::BTreeMap::new(),
        max_output_bytes: 65536,
    }
}

fn container(granted: &Path) -> AppContainer {
    let mut container = AppContainer::create().unwrap();
    container.grant_modify(granted).unwrap();
    container
}

/// Creates a real file symlink. Requires Developer Mode (or
/// SeCreateSymbolicLinkPrivilege), which this host has; junctions cover the
/// portable case separately.
fn symlink_file(link: &Path, target: &Path) {
    let made = Command::new(cmd())
        .args(["/C", "mklink"])
        .arg(link)
        .arg(target)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(
        made.success(),
        "mklink {} -> {} failed",
        link.display(),
        target.display()
    );
}

/// A contained child must not write through a junction that points outside
/// the granted root: the access check runs on the final target.
#[test]
fn junction_escape_write_is_denied() {
    let roots = Roots::new("junction");
    // The container and its grant come first: objects created afterwards
    // inherit the container ACE, so the denial below is the junction target's
    // ACL, not an unreadable reparse point.
    let container = container(&roots.granted);
    let junction = roots.granted.join("link");
    let made = Command::new(cmd())
        .args(["/C", "mklink", "/J"])
        .arg(&junction)
        .arg(&roots.outside)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(made.success(), "mklink /J failed");
    let escaped = container.run(&request(
        &["echo", "escaped>", r"link\escaped.txt"],
        &roots.granted,
        15,
    ));
    assert_ne!(
        escaped.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        escaped.stdout,
        escaped.stderr
    );
    assert!(
        !roots.outside.join("escaped.txt").exists(),
        "contained child wrote through a junction outside the grant"
    );

    // Positive control: the same write without the junction still works, so
    // the denial is the boundary, not a broken command line.
    let inside = container.run(&request(
        &["echo", "inside>", "inside.txt"],
        &roots.granted,
        15,
    ));
    assert_eq!(
        inside.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        inside.stdout,
        inside.stderr
    );
    assert!(roots.granted.join("inside.txt").is_file());
}

/// A file symlink planted inside the granted root — what a checked-out
/// repository can carry — must not let a contained child write through it:
/// the access check runs on the final target, so the file outside the grant
/// stays untouched. A symlink whose target is also inside the grant still
/// works, so the denial is the boundary, not broken symlink handling.
#[test]
fn symlink_escape_write_is_denied() {
    let roots = Roots::new("symlink");
    // Grant first, then create the outside target, so the denial below is the
    // container boundary and not a stale descriptor.
    let container = container(&roots.granted);
    let outside_target = roots.outside.join("target.txt");
    std::fs::write(&outside_target, "outside\n").unwrap();
    symlink_file(&roots.granted.join("escape.txt"), &outside_target);

    let escaped = container.run(&request(
        &["echo", "escaped>", "escape.txt"],
        &roots.granted,
        15,
    ));
    assert_ne!(
        escaped.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        escaped.stdout,
        escaped.stderr
    );
    assert_eq!(
        std::fs::read_to_string(&outside_target).unwrap(),
        "outside\n",
        "contained child wrote through a symlink outside the grant"
    );

    // Positive control: a symlink to a granted file is usable.
    let inside_target = roots.granted.join("inside.txt");
    std::fs::write(&inside_target, "inside\n").unwrap();
    symlink_file(&roots.granted.join("allowed.txt"), &inside_target);

    let allowed = container.run(&request(
        &["echo", "inner>", "allowed.txt"],
        &roots.granted,
        15,
    ));
    assert_eq!(
        allowed.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        allowed.stdout,
        allowed.stderr
    );
    let written = std::fs::read_to_string(&inside_target).unwrap();
    assert!(written.contains("inner"), "target: {written:?}");
}

/// Renaming inside the grant to a target outside it must fail: a rename needs
/// delete access on the source and add-file access on the outside directory.
#[test]
fn rename_across_the_grant_boundary_is_denied() {
    let roots = Roots::new("rename");
    // Grant first, then create the source: it must be readable and movable
    // *within* the grant, so the refusal below is the outside target.
    let container = container(&roots.granted);
    std::fs::write(roots.granted.join("inside.txt"), "inside\n").unwrap();
    let target = roots.outside.join("stolen.txt");
    let run = container.run(&request(
        &["move", "inside.txt", &target.display().to_string()],
        &roots.granted,
        15,
    ));
    assert_ne!(
        run.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        run.stdout,
        run.stderr
    );
    assert!(
        roots.granted.join("inside.txt").is_file(),
        "the contained child moved the source file"
    );
    assert!(
        !target.exists(),
        "contained child renamed a file outside the grant"
    );
}

/// Alternate data streams are still writes to the file object: denied on files
/// outside the grant, allowed on files inside it.
#[test]
fn alternate_data_streams_outside_the_grant_are_denied() {
    let roots = Roots::new("ads");
    let outside_file = roots.outside.join("target.txt");
    std::fs::write(&outside_file, "outside\n").unwrap();

    let container = container(&roots.granted);
    let denied = container.run(&request(
        &[
            "echo",
            "canary>",
            &format!("{}:ads", outside_file.display()),
        ],
        &roots.granted,
        15,
    ));
    assert_ne!(
        denied.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        denied.stdout,
        denied.stderr
    );
    assert!(
        std::fs::read(format!("{}:ads", outside_file.display())).is_err(),
        "contained child wrote an alternate data stream outside the grant"
    );

    let inside_file = roots.granted.join("inside.txt");
    std::fs::write(&inside_file, "inside\n").unwrap();
    let allowed = container.run(&request(
        &["echo", "inner>", &format!("{}:ads", inside_file.display())],
        &roots.granted,
        15,
    ));
    assert_eq!(
        allowed.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        allowed.stdout,
        allowed.stderr
    );
    let stream = std::fs::read_to_string(format!("{}:ads", inside_file.display())).unwrap();
    assert!(stream.contains("inner"), "stream: {stream:?}");
}

/// Only the three stdio handles may cross into the container. A foreign
/// inheritable handle (here: a file outside the grant) is readable by a host
/// child but must be invalid inside the container.
#[test]
fn foreign_inheritable_handles_do_not_cross_into_the_container() {
    use std::os::windows::io::AsRawHandle;

    let roots = Roots::new("handles");
    // Grant first so the copied probe inherits the container ACE; the test
    // binary's build directory is deliberately not readable to the container.
    // The bytes are written by hand because `fs::copy` also copies the source
    // file's descriptor, which would carry the build directory's DACL in.
    let container = container(&roots.granted);
    let probe = roots.granted.join("handle_probe.exe");
    std::fs::write(
        &probe,
        std::fs::read(env!("CARGO_BIN_EXE_handle_probe")).unwrap(),
    )
    .unwrap();

    let canary_path = roots.outside.join("canary.txt");
    std::fs::write(&canary_path, "CANARY-SECRET\n").unwrap();
    let canary = std::fs::File::open(&canary_path).unwrap();
    let raw = canary.as_raw_handle() as usize;
    let marked =
        unsafe { SetHandleInformation(raw as HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_ne!(marked, 0, "SetHandleInformation failed");

    // The probe writes what it can read through the handle number into a file
    // in the granted root. Probing a stale handle can hard-terminate the child
    // (STATUS_INVALID_HANDLE) instead of reporting an error, so the observable
    // outcome is the copy file, not the probe's stdout or exit code.
    let copy = |tag: &str| {
        let out = roots.granted.join(format!("{tag}-copy.txt"));
        ExecRequest {
            argv: vec![
                probe.display().to_string(),
                "copy-handle".into(),
                raw.to_string(),
                out.display().to_string(),
            ],
            cwd: roots.granted.clone(),
            timeout: Duration::from_secs(15),
            env: std::collections::BTreeMap::new(),
            max_output_bytes: 65536,
        }
    };

    // Control: the host broker inherits marked handles, so the canary bytes
    // land in the copy. This proves the probe and the inheritance mechanism.
    let host = bollo_workspace::process::run(&copy("host"));
    assert_eq!(host.exit_code, Some(0), "stderr: {:?}", host.stderr);
    let leaked = std::fs::read_to_string(roots.granted.join("host-copy.txt")).unwrap();
    assert!(
        leaked.contains("CANARY-SECRET"),
        "host control did not inherit the canary: {leaked:?}"
    );

    let _ = container.run(&copy("contained"));
    let leaked = std::fs::read(roots.granted.join("contained-copy.txt")).unwrap_or_default();
    assert!(
        !String::from_utf8_lossy(&leaked).contains("CANARY-SECRET"),
        "contained child read a file outside the grant through an inherited handle"
    );
}

/// A contained run must not leave a process tree behind: `start`ed
/// grandchildren die when the run's job object closes (normal exit) and a
/// deadline terminates the whole tree, not just the direct child.
#[test]
fn contained_runs_leave_no_surviving_process_tree() {
    let roots = Roots::new("tree");
    // Grant first so the copied probe inherits the container ACE.
    let container = container(&roots.granted);
    let probe = roots.granted.join("handle_probe.exe");
    std::fs::write(
        &probe,
        std::fs::read(env!("CARGO_BIN_EXE_handle_probe")).unwrap(),
    )
    .unwrap();
    // The survivor sleeps, then writes a marker; it is launched with `start`,
    // so it is a grandchild of the process the broker spawned. Network tools
    // are useless as delays here: the container denies their traffic and they
    // exit at once, so the delay comes from the fixture itself.
    let survivor = "start \"\" /B handle_probe.exe sleep-write 3 survivor.txt survivor";

    // Positive control: with the run still alive when the survivor writes, the
    // marker appears — so a missing marker below is the job kill, not a
    // command that never launched.
    std::fs::write(
        roots.granted.join("chain.cmd"),
        format!("@echo off\r\n{survivor}\r\nhandle_probe.exe sleep 6\r\n"),
    )
    .unwrap();
    let control = container.run(&request(&["chain.cmd"], &roots.granted, 15));
    assert_eq!(
        control.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        control.stdout,
        control.stderr
    );
    assert!(
        roots.granted.join("survivor.txt").is_file(),
        "control: the grandchild never wrote its marker, so the kill scenarios prove nothing"
    );
    std::fs::remove_file(roots.granted.join("survivor.txt")).unwrap();

    // Timeout case: the direct child also sleeps past the deadline.
    std::fs::write(
        roots.granted.join("chain.cmd"),
        format!("@echo off\r\n{survivor}\r\nhandle_probe.exe sleep 30\r\n"),
    )
    .unwrap();
    let timed_out = container.run(&request(&["chain.cmd"], &roots.granted, 1));
    assert_eq!(
        timed_out.status,
        bollo_workspace::ExecStatus::TimedOut,
        "stdout: {:?} stderr: {:?}",
        timed_out.stdout,
        timed_out.stderr
    );
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !roots.granted.join("survivor.txt").exists(),
        "a grandchild survived the deadline and wrote after the run ended"
    );

    // Normal-exit case: the same survivor must not outlive a completed run.
    std::fs::write(
        roots.granted.join("chain.cmd"),
        format!("@echo off\r\n{survivor}\r\n"),
    )
    .unwrap();
    let completed = container.run(&request(&["chain.cmd"], &roots.granted, 15));
    assert_eq!(
        completed.exit_code,
        Some(0),
        "stdout: {:?} stderr: {:?}",
        completed.stdout,
        completed.stderr
    );
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !roots.granted.join("survivor.txt").exists(),
        "a grandchild outlived a normally completed contained run"
    );
}
