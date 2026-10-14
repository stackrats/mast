//! Helpers for unit tests that drive a real Docker daemon. Each such test
//! checks [`docker_usable`] first and skips cleanly without one (or with
//! `MAST_SKIP_DOCKER_TESTS` set), and names its containers `mast-it-…` so the
//! integration suite's janitor sweeps any a crash leaves behind.

/// `docker <args…>`, two minutes at most.
pub(crate) async fn docker(args: &[&str]) -> mast_docker::CommandOutput {
    let argv: Vec<String> = std::iter::once("docker")
        .chain(args.iter().copied())
        .map(String::from)
        .collect();
    mast_docker::run_command(
        &argv,
        None,
        &[],
        std::time::Duration::from_secs(120),
        1024 * 1024,
    )
    .await
    .expect("docker ran")
}

pub(crate) async fn docker_usable() -> bool {
    if std::env::var_os("MAST_SKIP_DOCKER_TESTS").is_some() {
        return false;
    }
    mast_docker::run_command(
        &[
            "docker".into(),
            "info".into(),
            "--format".into(),
            "{{.OSType}}".into(),
        ],
        None,
        &[],
        std::time::Duration::from_secs(10),
        16 * 1024,
    )
    .await
    .map(|out| out.success() && out.stdout.trim() == "linux")
    .unwrap_or(false)
}

/// Removes the named container when dropped, failed assertions included.
pub(crate) struct RemoveContainer(pub(crate) String);

impl Drop for RemoveContainer {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.0])
            .output();
    }
}

/// A fresh Alpine container idling under `sleep`, removed when the guard
/// drops.
pub(crate) async fn alpine() -> (String, RemoveContainer) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let name = format!("mast-it-{nonce}");
    let guard = RemoveContainer(name.clone());
    let started = docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "alpine:latest",
        "sleep",
        "600",
    ])
    .await;
    assert!(started.success(), "{}", started.stderr);
    (name, guard)
}
