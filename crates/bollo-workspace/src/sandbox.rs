//! Sandbox capability probing.
//!
//! Claims must be backed by an executed probe. When enforcement is unavailable
//! the probe reports `false` and the policy layer refuses `workspace_auto`
//! instead of silently running on the host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollo_protocol::vocab::{Platform, SandboxCapabilities};

use crate::process::{run, ChildSandbox, ExecRequest, ExecStatus};

pub fn probe() -> SandboxCapabilities {
    match Platform::current() {
        Platform::Linux => probe_linux(),
        Platform::Macos => SandboxCapabilities {
            platform: Platform::Macos,
            filesystem_containment: false,
            network_denied: false,
            backend: "macOS beta: no validated sandbox backend in this build".into(),
        },
        Platform::Windows => probe_windows(),
        Platform::Other => SandboxCapabilities {
            platform: Platform::Other,
            filesystem_containment: false,
            network_denied: false,
            backend: "unsupported platform".into(),
        },
    }
}

/// Executed probe used where enforcement actually matters (workspace-mode
/// startup and `bollo doctor`): on Windows it runs the AppContainer containment
/// check and reports the measured flags. Containment is only reported when the
/// check passed *and* this build routes brokered children through the container
/// (see [`create_workspace_sandbox`]); otherwise the flags stay false and
/// workspace mode is refused.
pub fn verified_probe() -> SandboxCapabilities {
    match Platform::current() {
        Platform::Windows => verified_probe_windows(),
        _ => probe(),
    }
}

/// Host paths a contained `cargo`/`rustc` run must be able to read, and the
/// credential stores that must stay unreadable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainAccess {
    /// Directory trees granted read+execute to the shared toolchain
    /// capability: the Rust toolchain and the cargo binary/cache trees.
    pub read_roots: Vec<PathBuf>,
    /// Individual files granted read+execute: cargo's `config.toml`, which is a
    /// configuration file (not a credential store) that cargo refuses to build
    /// with when it cannot read it.
    pub read_files: Vec<PathBuf>,
    /// Credential stores (`.cargo/credentials.toml` and the legacy
    /// `credentials`). They are kept *outside* every granted root by
    /// construction, which is what keeps them unreadable: measured on Windows,
    /// a capability SID gets access through allow ACEs only, so a deny ACE does
    /// not override an inherited one. A store that would fall inside a granted
    /// root makes sandbox creation refuse the run (`credential_conflict`).
    pub excluded_files: Vec<PathBuf>,
}

impl ToolchainAccess {
    /// The first credential store that falls inside `root`, if any. Callers
    /// refuse the run rather than grant a tree that contains a store: the
    /// grant would otherwise make the store readable through inheritance.
    pub fn credential_conflict(&self, root: &Path) -> Option<&Path> {
        let mut prefix = normalized(root);
        if !prefix.is_empty() {
            prefix.push('\\');
        }
        self.excluded_files
            .iter()
            .find(|store| normalized(store).starts_with(&prefix))
            .map(PathBuf::as_path)
    }
}

/// Case-insensitive, separator-trimmed form for containment comparisons:
/// Windows path identity is case-insensitive, and a directory prefix must not
/// match a sibling whose name merely starts with the same text.
fn normalized(path: &Path) -> String {
    let mut text = path.to_string_lossy().replace('/', "\\").to_lowercase();
    while text.ends_with('\\') {
        text.pop();
    }
    text
}

/// Create the host enforcement sandbox for `workspace`, when the host needs one
/// and can provide it. Windows returns an AppContainer whose stable capability
/// grants carry read access to the host toolchain and modify access to the
/// canonical workspace; other platforms return `None`, leaving their existing
/// broker behavior unchanged.
///
/// Any grant failure is fatal: a run whose toolchain cannot be made readable is
/// refused rather than launched to fail later with a confusing access error.
pub fn create_workspace_sandbox(workspace: &Path) -> Result<Option<Box<dyn ChildSandbox>>, String> {
    #[cfg(windows)]
    {
        let access = toolchain_access();
        // Fail closed when a credential store would end up inside a granted
        // root (for example a CARGO_HOME that lives inside the workspace
        // itself): the grant would make the store readable, and a deny ACE
        // cannot take it back.
        for root in &access.read_roots {
            if let Some(store) = access.credential_conflict(root) {
                return Err(format!(
                    "credential store {} lies inside the toolchain grant {}; refusing workspace mode",
                    store.display(),
                    root.display()
                ));
            }
        }
        if let Some(store) = access.credential_conflict(workspace) {
            return Err(format!(
                "credential store {} lies inside the workspace; refusing workspace mode",
                store.display()
            ));
        }
        let mut container = crate::sandbox_win::AppContainer::create_workspace(workspace)?;
        for root in &access.read_roots {
            container
                .grant_toolchain_read(root)
                .map_err(|err| format!("toolchain read grant on {}: {err}", root.display()))?;
        }
        for file in &access.read_files {
            container
                .grant_toolchain_read_file(file)
                .map_err(|err| format!("toolchain read grant on {}: {err}", file.display()))?;
        }
        container
            .grant_workspace_modify(workspace)
            .map_err(|err| format!("workspace modify grant on {}: {err}", workspace.display()))?;
        Ok(Some(Box::new(container)))
    }
    #[cfg(not(windows))]
    {
        let _ = workspace;
        Ok(None)
    }
}

/// Discover the host toolchain locations a contained build must read. Paths are
/// environment-derived on purpose: `RUSTUP_HOME`/`CARGO_HOME` when set,
/// otherwise the standard per-user locations. A container never inherits the
/// host environment, so the default locations are covered next to the
/// effective ones, and the credential stores under both are excluded. Missing
/// trees are skipped: there is nothing to grant.
///
/// The cargo home itself is deliberately never a grant root: only its
/// `bin`, `registry` and `git` subtrees are, so `credentials.toml` sits outside
/// every grant. `config.toml` is granted as a single file.
pub fn toolchain_access() -> ToolchainAccess {
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    toolchain_access_from(
        env_path("RUSTUP_HOME"),
        env_path("CARGO_HOME"),
        profile.as_ref().map(|home| home.join(".rustup")),
        profile.as_ref().map(|home| home.join(".cargo")),
    )
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn toolchain_access_from(
    rustup_home: Option<PathBuf>,
    cargo_home: Option<PathBuf>,
    default_rustup: Option<PathBuf>,
    default_cargo: Option<PathBuf>,
) -> ToolchainAccess {
    let mut read_roots: Vec<PathBuf> = Vec::new();
    let mut read_files: Vec<PathBuf> = Vec::new();
    let mut excluded_files: Vec<PathBuf> = Vec::new();
    // The rustup home (toolchains, settings, downloads) holds no credential
    // store, so it is granted whole.
    for root in [rustup_home, default_rustup].into_iter().flatten() {
        if root.is_dir() && !read_roots.contains(&root) {
            read_roots.push(root);
        }
    }
    let mut cargo_homes: Vec<PathBuf> = Vec::new();
    for home in [cargo_home, default_cargo].into_iter().flatten() {
        if !cargo_homes.contains(&home) {
            cargo_homes.push(home);
        }
    }
    for home in cargo_homes {
        for subtree in ["bin", "registry", "git"] {
            let root = home.join(subtree);
            if root.is_dir() && !read_roots.contains(&root) {
                read_roots.push(root);
            }
        }
        let config = home.join("config.toml");
        if config.is_file() && !read_files.contains(&config) {
            read_files.push(config);
        }
        for name in ["credentials.toml", "credentials"] {
            let file = home.join(name);
            if file.is_file() && !excluded_files.contains(&file) {
                excluded_files.push(file);
            }
        }
    }
    ToolchainAccess {
        read_roots,
        read_files,
        excluded_files,
    }
}

fn probe_linux() -> SandboxCapabilities {
    let Some(bwrap) = find_on_path("bwrap") else {
        return SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: false,
            network_denied: false,
            backend: "bubblewrap not found; no verified enforcement".into(),
        };
    };
    // A real probe: mount namespace + read-only root + network unshare. The
    // full guarantee matrix (write denial, symlink/rename resistance,
    // protected state) is a Phase-0 obligation, so the note stays explicit.
    let request = ExecRequest {
        argv: vec![
            bwrap,
            "--unshare-net".into(),
            "--ro-bind".into(),
            "/".into(),
            "/".into(),
            "--".into(),
            "/bin/true".into(),
        ],
        cwd: PathBuf::from("/"),
        timeout: Duration::from_secs(5),
        env: BTreeMap::new(),
        max_output_bytes: 4096,
    };
    let outcome = run(&request);
    if outcome.status == ExecStatus::Exited && outcome.exit_code == Some(0) {
        SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: true,
            network_denied: true,
            backend: "bubblewrap (basic namespace probe passed)".into(),
        }
    } else {
        SandboxCapabilities {
            platform: Platform::Linux,
            filesystem_containment: false,
            network_denied: false,
            backend: format!(
                "bubblewrap probe failed: {:?} exit={:?}",
                outcome.status, outcome.exit_code
            ),
        }
    }
}

#[cfg(windows)]
fn probe_windows() -> SandboxCapabilities {
    SandboxCapabilities {
        platform: Platform::Windows,
        // The cheap probe makes no claim; workspace-mode startup replaces this
        // with `verified_probe()` before validation, so a run never proceeds on
        // an unexecuted claim.
        filesystem_containment: false,
        network_denied: false,
        backend: "Windows AppContainer: substrate present; workspace mode verifies it with an \
                 executed check (`bollo doctor` shows the evidence)"
            .into(),
    }
}

#[cfg(windows)]
fn verified_probe_windows() -> SandboxCapabilities {
    match crate::sandbox_win::selfcheck_containment() {
        Ok(report)
            if report.write_inside_ok && report.write_outside_denied && report.network_denied =>
        {
            SandboxCapabilities {
                platform: Platform::Windows,
                filesystem_containment: true,
                network_denied: true,
                backend: "Windows AppContainer (executed check passed: writes outside granted \
                         roots denied; outbound network including loopback denied; brokered \
                         children are launched inside the container)"
                    .into(),
            }
        }
        Ok(report) => SandboxCapabilities {
            platform: Platform::Windows,
            filesystem_containment: false,
            network_denied: false,
            backend: format!(
                "Windows AppContainer: executed check failed ({}); refusing workspace mode",
                report.detail
            ),
        },
        Err(err) => SandboxCapabilities {
            platform: Platform::Windows,
            filesystem_containment: false,
            network_denied: false,
            backend: format!(
                "Windows AppContainer: mechanism unavailable ({err}); refusing workspace mode"
            ),
        },
    }
}

#[cfg(not(windows))]
fn probe_windows() -> SandboxCapabilities {
    SandboxCapabilities {
        platform: Platform::Windows,
        filesystem_containment: false,
        network_denied: false,
        backend: "Windows probe is not reachable on this host platform".into(),
    }
}

#[cfg(not(windows))]
fn verified_probe_windows() -> SandboxCapabilities {
    probe_windows()
}

pub(crate) fn find_on_path(binary: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchain_access_grants_caches_and_excludes_credentials() {
        let base =
            std::env::temp_dir().join(format!("bollo-toolchain-access-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let rustup = base.join("rustup");
        let cargo = base.join("cargo");
        let override_cargo = base.join("override-cargo");
        for home in [&rustup, &cargo, &override_cargo] {
            std::fs::create_dir_all(home).unwrap();
        }
        for home in [&cargo, &override_cargo] {
            for subtree in ["bin", "registry", "git"] {
                std::fs::create_dir_all(home.join(subtree)).unwrap();
            }
            std::fs::write(home.join("config.toml"), "# config\n").unwrap();
            std::fs::write(home.join("credentials.toml"), "token = \"secret\"\n").unwrap();
            std::fs::write(home.join("credentials"), "legacy\n").unwrap();
        }

        let access = toolchain_access_from(
            Some(rustup.clone()),
            Some(cargo.clone()),
            Some(base.join("absent-rustup")),
            Some(base.join("absent-cargo")),
        );
        assert_eq!(
            access.read_roots,
            vec![
                rustup.clone(),
                cargo.join("bin"),
                cargo.join("registry"),
                cargo.join("git"),
            ]
        );
        assert_eq!(access.read_files, vec![cargo.join("config.toml")]);
        assert_eq!(
            access.excluded_files,
            vec![cargo.join("credentials.toml"), cargo.join("credentials")]
        );
        // The exclusion is structural: no credential store may sit inside a
        // granted root, because Windows grants capability-SID access through
        // allow ACEs only (a deny ACE does not override an inherited one).
        for excluded in &access.excluded_files {
            assert!(
                access
                    .read_roots
                    .iter()
                    .all(|root| !excluded.starts_with(root)),
                "{} is inside a granted root",
                excluded.display()
            );
        }

        // A store inside a granted root is a refusal, not a grant: the root
        // ACE would make the store readable through inheritance.
        assert!(access.credential_conflict(&rustup).is_none());
        assert!(access
            .credential_conflict(&cargo.join("registry"))
            .is_none());
        assert_eq!(
            access.credential_conflict(&cargo),
            Some(cargo.join("credentials.toml").as_path())
        );
        assert!(access.credential_conflict(&base).is_some());
        // Containment is case-insensitive and prefix-boundary aware.
        assert!(access
            .credential_conflict(&PathBuf::from(cargo.to_string_lossy().to_uppercase()))
            .is_some());
        assert!(access.credential_conflict(&base.join("carg")).is_none());

        // An environment override is covered next to the default locations,
        // and credentials are excluded under both.
        let overridden = toolchain_access_from(
            None,
            Some(override_cargo.clone()),
            None,
            Some(cargo.clone()),
        );
        assert!(overridden.read_roots.contains(&override_cargo.join("bin")));
        assert!(overridden.read_roots.contains(&cargo.join("registry")));
        assert!(overridden
            .excluded_files
            .contains(&override_cargo.join("credentials.toml")));
        assert!(overridden
            .excluded_files
            .contains(&cargo.join("credentials")));

        // Missing trees are skipped: there is nothing to grant.
        let absent = toolchain_access_from(
            Some(base.join("absent-rustup")),
            Some(base.join("absent-cargo")),
            None,
            None,
        );
        assert!(absent.read_roots.is_empty());
        assert!(absent.read_files.is_empty());
        assert!(absent.excluded_files.is_empty());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn probe_reports_current_platform_and_never_overclaims() {
        let caps = probe();
        assert_eq!(caps.platform, Platform::current());
        if cfg!(windows) {
            assert!(!caps.filesystem_containment);
            assert!(!caps.network_denied);
            assert!(
                caps.backend.contains("Windows AppContainer"),
                "backend must carry executed mechanism evidence: {}",
                caps.backend
            );
        }
        // The probe result must be internally consistent.
        if caps.supports_workspace_auto() {
            assert!(caps.filesystem_containment && caps.network_denied);
        }
        // The verified probe executes the mechanism check on Windows (and on
        // Linux runs the bubblewrap probe). Whatever it reports must be
        // internally consistent and never a partial claim.
        let verified = verified_probe();
        assert_eq!(verified.platform, Platform::current());
        if cfg!(windows) {
            assert!(
                verified.backend.contains("Windows AppContainer"),
                "verified probe must carry executed evidence: {}",
                verified.backend
            );
        }
        if verified.supports_workspace_auto() {
            assert!(verified.filesystem_containment && verified.network_denied);
        }
    }
}
