#![cfg(windows)]
//! Host read grants for contained cargo/rustc runs.
//!
//! The property under test is the one the harness depends on: a contained
//! `cargo build` behaves like the host's, reading the Rust toolchain and the
//! package caches through stable capability grants, while the credential
//! stores (`~/.cargo/credentials.toml` and the legacy `credentials`) stay
//! outside every grant and unreadable.
//!
//! Note the deliberate side effect: the first run on a host applies the
//! toolchain grants to the real trees (and the capability makes them reusable
//! afterwards), exactly like a workspace-mode run does. Test credential files
//! live in temp directories; the real stores are never written to.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollo_workspace::process::{ChildSandbox, ExecOutcome, ExecRequest};
use bollo_workspace::sandbox::toolchain_access;
use bollo_workspace::sandbox_win::AppContainer;

fn cmd() -> PathBuf {
    PathBuf::from(std::env::var("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe")
}

fn request(words: &[&str], cwd: &Path) -> ExecRequest {
    let mut argv = vec![cmd().display().to_string(), "/C".into()];
    argv.extend(words.iter().map(|word| word.to_string()));
    ExecRequest {
        argv,
        cwd: cwd.to_path_buf(),
        timeout: Duration::from_secs(180),
        env: BTreeMap::new(),
        max_output_bytes: 65536,
    }
}

/// Run a diagnostic inside the container and print it: a failing test prints
/// these lines, which is how a failure seen only on a runner is diagnosed.
fn probe(container: &AppContainer, workspace: &Path, label: &str, words: &[&str]) -> ExecOutcome {
    let outcome = container.run(&request(words, workspace));
    println!(
        "{label}: exit={:?}\n  stdout: {}\n  stderr: {}",
        outcome.exit_code,
        outcome.stdout.trim_end(),
        outcome.stderr.trim_end()
    );
    outcome
}

/// Output of a host command (stdout then stderr), for the DACL report below.
fn host_command(program: &str, args: &[&str]) -> String {
    match std::process::Command::new(program).args(args).output() {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            text.trim_end().to_string()
        }
        Err(err) => format!("{program} failed: {err}"),
    }
}

fn host_cargo() -> PathBuf {
    if let Ok(cargo) = std::env::var("CARGO") {
        let candidate = PathBuf::from(cargo);
        if candidate.is_file() {
            return candidate;
        }
    }
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|dir| dir.join("cargo.exe"))
        .find(|candidate| candidate.is_file())
        .expect("cargo.exe on PATH")
}

/// A contained `cargo build --offline` of a dependency-free crate must succeed
/// on the host toolchain and package caches, and the artifact must land in the
/// workspace. The crate is written *after* the grants, so its files inherit;
/// the toolchain files existed before and are covered by the walk.
#[test]
fn contained_cargo_builds_with_host_toolchain_grants() {
    let base = std::env::temp_dir().join(format!("bollo-cargo-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let workspace = base.join("ws");
    std::fs::create_dir_all(&workspace).unwrap();

    let mut container = AppContainer::create_workspace(&workspace).unwrap();
    let access = toolchain_access();
    assert!(
        !access.read_roots.is_empty(),
        "no host toolchain found; this test needs a Rust toolchain"
    );
    for root in &access.read_roots {
        container
            .grant_toolchain_read(root)
            .unwrap_or_else(|err| panic!("grant {}: {err}", root.display()));
    }
    for file in &access.read_files {
        container
            .grant_toolchain_read_file(file)
            .unwrap_or_else(|err| panic!("grant {}: {err}", file.display()));
    }
    container.grant_workspace_modify(&workspace).unwrap();

    // Red-run evidence, in three parts: what was granted, what the host's own
    // DACLs say the toolchain carries, and what the container can actually see
    // and run. A failure that only happens on a runner is read from these.
    println!("host cargo: {}", host_cargo().display());
    println!("grant roots: {:#?}", access.read_roots);
    println!("grant files: {:#?}", access.read_files);
    probe(
        &container,
        &workspace,
        "container PATH",
        &["echo", "%PATH%"],
    );
    probe(
        &container,
        &workspace,
        "container where rustc",
        &["where", "rustc"],
    );
    if let Some(bin) = host_cargo().parent().map(PathBuf::from) {
        let rustc = bin.join("rustc.exe");
        println!(
            "host icacls {}:\n{}",
            bin.display(),
            host_command("icacls", &[&bin.display().to_string()])
        );
        println!(
            "host icacls {}:\n{}",
            rustc.display(),
            host_command("icacls", &[&rustc.display().to_string()])
        );
        // A DACL that grants the capability is not the whole gate: a mandatory
        // integrity label above the container's level, or an image hardlinked
        // from a differently labelled tree, fails CreateProcess while leaving
        // READ_CONTROL (what `icacls` needs) intact. Report both.
        println!(
            "host labels and links:\n{}",
            host_command(
                "powershell",
                &[
                    "-NoProfile",
                    "-Command",
                    &format!(
                        "'{}','{}','{}' | ForEach-Object {{ \
                         $a = Get-Acl $_ -Audit; \
                         $l = ($a.Audit | ForEach-Object {{ $_.IdentityReference.Value \
                         + ':' + $_.AuditFlags }}) -join '; '; \
                         \"$_ -> links=$((Get-Item $_).LinkType) label[$l]\" }}",
                        rustc.display(),
                        bin.display(),
                        workspace.display()
                    )
                ]
            )
        );
        println!(
            "host hardlinks rustc:\n{}",
            host_command(
                "fsutil",
                &["hardlink", "list", &rustc.display().to_string()]
            )
        );
        probe(
            &container,
            &workspace,
            "container icacls rustc",
            &["icacls", &rustc.display().to_string()],
        );
        probe(
            &container,
            &workspace,
            "container rustc --version",
            &[&rustc.display().to_string(), "--version"],
        );
    }

    std::fs::create_dir_all(workspace.join("src")).unwrap();
    std::fs::write(
        workspace.join("Cargo.toml"),
        "[package]\nname = \"toy\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        workspace.join("src").join("lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();

    let run = container.run(&ExecRequest {
        argv: vec![
            host_cargo().display().to_string(),
            "build".into(),
            "--offline".into(),
        ],
        cwd: workspace.clone(),
        timeout: Duration::from_secs(180),
        env: BTreeMap::new(),
        max_output_bytes: 65536,
    });
    assert_eq!(
        run.exit_code,
        Some(0),
        "contained cargo build failed:\nstdout: {}\nstderr: {}",
        run.stdout,
        run.stderr
    );
    assert!(
        workspace
            .join("target")
            .join("debug")
            .join("libtoy.rlib")
            .is_file(),
        "cargo reported success but the artifact is missing"
    );

    // The toolchain grant is amortized: a second run finds the marker ACE on
    // every root and writes nothing again. (The toolchain files themselves are
    // covered by propagation from the root, which this build just proved.)
    for root in &access.read_roots {
        let outcome = container.grant_toolchain_read(root).unwrap();
        assert!(
            outcome.already_granted,
            "grant {} was not reused: {outcome:?}",
            root.display()
        );
    }
    let _ = std::fs::remove_dir_all(&base);
}

/// Credential stores must be unreadable even though they live next to granted
/// trees: exclusion is structural (no ACE ever reaches the store), because
/// Windows grants capability-SID access through allow ACEs only. The store
/// keeps sitting in the cargo home root, which is deliberately never a grant
/// root: only `bin`, `registry` and `git` are.
#[test]
fn credential_stores_stay_outside_every_grant() {
    let base = std::env::temp_dir().join(format!("bollo-cred-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let cargo_home = base.join("cargo");
    let registry = cargo_home.join("registry");
    let workspace = base.join("ws");
    std::fs::create_dir_all(&registry).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(registry.join("marker.txt"), "REGISTRY-OK\n").unwrap();
    std::fs::write(cargo_home.join("credentials.toml"), "TOKEN-SECRET\n").unwrap();
    std::fs::write(cargo_home.join("credentials"), "LEGACY-SECRET\n").unwrap();
    std::fs::write(workspace.join("source.txt"), "SOURCE-OK\n").unwrap();

    let stores = [
        cargo_home.join("credentials.toml"),
        cargo_home.join("credentials"),
    ];
    let mut container = AppContainer::create_workspace(&workspace).unwrap();
    container.grant_toolchain_read(&registry).unwrap();
    container.grant_workspace_modify(&workspace).unwrap();

    let read = |path: &Path| -> ExecOutcome {
        container.run(&request(&["type", &path.display().to_string()], &workspace))
    };

    assert!(
        read(&registry.join("marker.txt"))
            .stdout
            .contains("REGISTRY-OK"),
        "a granted cache file was not readable"
    );
    for store in &stores {
        let output = read(store);
        assert!(
            !output.stdout.contains("SECRET"),
            "credential store {} was readable from the container",
            store.display()
        );
    }
    assert!(
        read(&cargo_home).stdout.is_empty(),
        "the cargo home root itself must not be a grant root"
    );
    // Positive control: pre-existing workspace content is readable and
    // writable (the root ACE propagates to it), so the refusals above are the
    // exclusion and not a broken command line.
    assert!(
        read(&workspace.join("source.txt"))
            .stdout
            .contains("SOURCE-OK"),
        "pre-existing workspace source was not readable"
    );
    let write = container.run(&request(&["echo", "edited>", "source.txt"], &workspace));
    assert_eq!(
        write.exit_code,
        Some(0),
        "pre-existing workspace file was not writable: {}",
        write.stderr
    );
    assert!(std::fs::read_to_string(workspace.join("source.txt"))
        .unwrap()
        .contains("edited"));

    let denied_write = container.run(&request(
        &["echo", "stolen>", &stores[0].display().to_string()],
        &workspace,
    ));
    assert_ne!(
        denied_write.exit_code,
        Some(0),
        "the container wrote a credential store"
    );
    assert!(!std::fs::read_to_string(&stores[0])
        .unwrap()
        .contains("stolen"));
    let _ = std::fs::remove_dir_all(&base);
}
