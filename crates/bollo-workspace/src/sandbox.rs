//! Sandbox capability probing.
//!
//! Claims must be backed by an executed probe. When enforcement is unavailable
//! the probe reports `false` and the policy layer refuses `workspace_auto`
//! instead of silently running on the host.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use bollo_protocol::vocab::{Platform, SandboxCapabilities};

use crate::process::{run, ExecRequest, ExecStatus};

pub fn probe() -> SandboxCapabilities {
    match Platform::current() {
        Platform::Linux => probe_linux(),
        Platform::Macos => SandboxCapabilities {
            platform: Platform::Macos,
            filesystem_containment: false,
            network_denied: false,
            backend: "macOS beta: no validated sandbox backend in this build".into(),
        },
        Platform::Windows => SandboxCapabilities {
            platform: Platform::Windows,
            filesystem_containment: false,
            network_denied: false,
            backend: "native Windows: not an MVP enforcement platform; host mode only".into(),
        },
        Platform::Other => SandboxCapabilities {
            platform: Platform::Other,
            filesystem_containment: false,
            network_denied: false,
            backend: "unsupported platform".into(),
        },
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

fn find_on_path(binary: &str) -> Option<String> {
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
    fn probe_reports_current_platform_and_never_overclaims() {
        let caps = probe();
        assert_eq!(caps.platform, Platform::current());
        if cfg!(windows) {
            assert!(!caps.filesystem_containment);
            assert!(!caps.network_denied);
            assert!(caps.backend.contains("host mode") || caps.backend.contains("Windows"));
        }
        // The probe result must be internally consistent.
        if caps.supports_workspace_auto() {
            assert!(caps.filesystem_containment && caps.network_denied);
        }
    }
}
