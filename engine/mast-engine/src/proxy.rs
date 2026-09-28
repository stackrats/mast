//! Local HTTPS domains: one shared Caddy container (`mast-proxy`) terminates
//! TLS for every project that claims a `.test` address and forwards to the
//! app's published localhost port. Certificates come from Caddy's internal
//! CA (`local_certs`), so nothing touches a public ACME endpoint and no
//! traffic leaves the machine. The two host-side steps Mast must not do
//! silently — the `/etc/hosts` line and trusting that CA — are emitted as
//! high-risk FixAvailable repairs on the enable operation instead of being
//! folded into it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mast_compose::ResolvedModel;
use mast_contract::{ErrorInfo, OperationEventKind, OperationId, PatchEvent, ProjectId};
use mast_diagnostics::{REPAIR_HOSTS_ENTRY, REPAIR_INSTALL_CERTUTIL, REPAIR_TRUST_PROXY_CA};
use mast_docker::run_command;

use crate::ops::OpHandle;
use crate::{Engine, internal_err};

pub(crate) const PROXY_CONTAINER: &str = "mast-proxy";
/// Where Caddy's internal CA keeps its root inside the container.
pub(crate) const CA_IN_CONTAINER: &str =
    "mast-proxy:/data/caddy/pki/authorities/local/root.crt";
/// The nickname the CA is filed under in `~/.pki/nssdb` — written by the
/// trust repair, read back by the NSS gap probe.
pub(crate) const NSS_NICKNAME: &str = "Mast local HTTPS (Caddy)";

const PROXY_IMAGE: &str = "caddy:2-alpine";
/// Generous: the first enable pulls the caddy image.
const DOCKER_TIMEOUT: Duration = Duration::from_secs(180);
/// A reload is a config swap, not a pull — and the reconcile waits on it.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(20);
const OUTPUT_CAP: usize = 256 * 1024;

/// The platform's hosts file — what the add-hosts-entry repair appends to
/// and what the manual "open it yourself" button shows.
pub(crate) fn hosts_file_path() -> &'static str {
    if cfg!(windows) { r"C:\Windows\System32\drivers\etc\hosts" } else { "/etc/hosts" }
}

/// `pkexec sh -c <script>` — polkit elevation, the Linux path.
fn pkexec_argv(script: &str) -> Vec<String> {
    ["pkexec", "sh", "-c", script].map(String::from).into()
}

/// `osascript … with administrator privileges` — the macOS password prompt.
/// The script is embedded in an AppleScript string literal, so backslashes
/// and quotes must be escaped for THAT layer.
fn osascript_admin_argv(script: &str) -> Vec<String> {
    let escaped = script.replace('\\', "\\\\").replace('"', "\\\"");
    vec![
        "osascript".into(),
        "-e".into(),
        format!("do shell script \"{escaped}\" with administrator privileges"),
    ]
}

/// The elevated shell for this platform. Both builders stay compiled on
/// every platform so the mac branch cannot rot unnoticed on Linux builds.
pub(crate) fn privileged_shell_argv(script: &str) -> Vec<String> {
    if cfg!(target_os = "macos") { osascript_admin_argv(script) } else { pkexec_argv(script) }
}

/// Windows elevation: UAC can only launch a program, not a shell string, so
/// the privileged work lives in a generated `.cmd` that an elevated console
/// runs. `-Wait -PassThru` + `exit $p.ExitCode` propagate the script's
/// errorlevel (a declined prompt raises in PowerShell and exits non-zero);
/// callers still verify the effect afterwards — trust the state, not the
/// exit code.
pub(crate) fn windows_elevated_cmd(lines: &[String]) -> std::io::Result<Vec<String>> {
    let dir = std::env::temp_dir().join("mast-elevate");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("mast-elevated.cmd");
    std::fs::write(&path, format!("@echo off\r\n{}\r\n", lines.join("\r\n")))?;
    // PowerShell single-quoted string: double any apostrophe in the path.
    let file = path.to_string_lossy().replace('\'', "''");
    Ok(vec![
        "powershell".into(),
        "-NoProfile".into(),
        "-Command".into(),
        format!(
            "$p = Start-Process -FilePath '{file}' -Verb RunAs -Wait -PassThru \
             -WindowStyle Hidden; exit $p.ExitCode"
        ),
    ])
}

/// Is a binary reachable through `PATH`?
pub(crate) fn on_path(binary: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else { return false };
    std::env::split_paths(&path).any(|dir| dir.join(binary).is_file())
}

/// The elevated one-liner that installs the NSS tools (certutil) with this
/// machine's package manager, plus the package's name for the messages.
/// `None` when no manager Mast knows is on `PATH` — Linux only; on macOS
/// the keychain already covers Chromium-family browsers.
pub(crate) fn certutil_install_script() -> Option<(&'static str, String)> {
    if cfg!(target_os = "macos") {
        return None;
    }
    [
        ("apt-get", "libnss3-tools", "apt-get install -y libnss3-tools"),
        ("dnf", "nss-tools", "dnf install -y nss-tools"),
        ("yum", "nss-tools", "yum install -y nss-tools"),
        ("pacman", "nss", "pacman -S --noconfirm nss"),
        ("zypper", "mozilla-nss-tools", "zypper --non-interactive install mozilla-nss-tools"),
    ]
    .into_iter()
    .find(|(manager, _, _)| on_path(manager))
    .map(|(_, package, script)| (package, script.to_string()))
}

/// How the elevation prompt is described to the user before they consent.
pub(crate) fn elevation_note() -> &'static str {
    if cfg!(target_os = "macos") {
        "asks for your password (macOS administrator privileges)"
    } else if cfg!(windows) {
        "asks for elevation via UAC (the Windows administrator prompt)"
    } else {
        "asks for elevation via polkit (pkexec)"
    }
}

/// The shapes a domain may take before it goes anywhere near `/etc/hosts`
/// or a root shell: lowercase letters, digits and hyphens, ending in a TLD
/// that can never collide with the public DNS (`.test` is reserved by RFC
/// 6761; `.localhost` resolves locally by convention).
pub(crate) fn validate_local_domain(domain: &str) -> Result<(), ErrorInfo> {
    let suffix_ok = domain.ends_with(".test") || domain.ends_with(".localhost");
    let shape_ok = domain.len() <= 253
        && domain.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        });
    if !suffix_ok || !shape_ok {
        return Err(ErrorInfo::InvalidInput {
            message: format!(
                "\"{domain}\" cannot be a local domain — lowercase letters, digits and \
                 hyphens, ending in .test or .localhost (e.g. myapp.test)"
            ),
        });
    }
    Ok(())
}

/// Does an `/etc/hosts` body already resolve `domain`? Token-wise, so
/// `myapp.test` does not match `other-myapp.test` or a commented line.
pub(crate) fn hosts_resolves(hosts: &str, domain: &str) -> bool {
    hosts.lines().any(|line| {
        let line = line.split('#').next().unwrap_or("");
        line.split_whitespace().skip(1).any(|name| name.eq_ignore_ascii_case(domain))
    })
}

/// One claimed domain as the proxy serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProxySite {
    pub domain: String,
    /// The host port the app publishes: `APP_PORT`, Sail's own default of
    /// 80 when unset — the same convention `sail up` publishes.
    pub app_port: u16,
    /// Reverb's WebSocket server, for a project that runs it.
    pub reverb: Option<ReverbEndpoint>,
}

/// Where a project's Reverb server listens, and where the host reaches it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReverbEndpoint {
    /// `REVERB_SERVER_PORT` inside the app container (Reverb's default 8080).
    pub listen_port: u16,
    /// The host port the app service publishes it on. `None` when it is not
    /// published — Sail's stub publishes only APP_PORT and VITE_PORT, so the
    /// mapping is the project's to add, and until then the proxy has nowhere
    /// to send the socket.
    pub host_port: Option<u16>,
}

/// Read what the proxy needs from one claimed project. `model` is the
/// resolved compose model — the only place that knows which host port a
/// container port is published on.
fn proxy_site(domain: String, path: &Path, model: Option<&ResolvedModel>) -> ProxySite {
    let env = mast_compose::parse_env_file(&path.join(".env"));
    let port =
        |key: &str, default: u16| env.get(key).and_then(|v| v.trim().parse().ok()).unwrap_or(default);
    let composer = std::fs::read_to_string(path.join("composer.json")).ok();
    let runs_reverb = mast_laravel::processes::detect_processes(composer.as_deref(), &env)
        .iter()
        .any(|def| def.id == "reverb");
    let reverb = runs_reverb.then(|| {
        let listen_port = port("REVERB_SERVER_PORT", 8080);
        // Mast runs Reverb in the app container, so that service's mapping
        // is the one that counts — another service publishing 8080 is
        // serving something else.
        let app_service = env.get("APP_SERVICE").map(String::as_str).unwrap_or("laravel.test");
        let host_port = model
            .and_then(|m| m.services.iter().find(|s| s.name == app_service))
            .and_then(|s| s.port_targets.iter().find(|(_, target)| *target == listen_port))
            .map(|(host, _)| *host);
        ReverbEndpoint { listen_port, host_port }
    });
    ProxySite { domain, app_port: port("APP_PORT", 80), reverb }
}

fn render_caddyfile(sites: &[ProxySite]) -> String {
    let mut out = String::from(
        "# Generated by Mast — rewritten whenever a local domain or the ports it \
         forwards to change, do not edit.\n\
         {\n\tlocal_certs\n\tskip_install_trust\n}\n",
    );
    for site in sites {
        out.push_str(&format!("\n{} {{\n", site.domain));
        if let Some(port) = site.reverb.and_then(|r| r.host_port) {
            // Only the upgrade goes to Reverb: `/app/…` is an ordinary route
            // prefix in plenty of apps, and those pages must still reach the
            // app. Caddy sorts the matched proxy ahead of the catch-all.
            out.push_str(&format!(
                "\t@reverb {{\n\t\tpath /app/*\n\t\theader Connection *Upgrade*\n\t\t\
                 header Upgrade websocket\n\t}}\n\treverse_proxy @reverb \
                 host.docker.internal:{port}\n"
            ));
        }
        out.push_str(&format!("\treverse_proxy host.docker.internal:{}\n}}\n", site.app_port));
    }
    out
}

/// Reload the running proxy from its mounted Caddyfile.
fn reload_argv() -> Vec<String> {
    ["docker", "exec", PROXY_CONTAINER, "caddy", "reload", "--config", "/etc/caddy/Caddyfile"]
        .map(String::from)
        .into()
}

impl Engine {
    pub(crate) fn dispatch_set_local_domain(
        &self,
        project: ProjectId,
        domain: Option<String>,
    ) -> Result<OperationId, ErrorInfo> {
        let domain = match domain {
            Some(d) => {
                let d = d.trim().to_ascii_lowercase();
                validate_local_domain(&d)?;
                Some(d)
            }
            None => None,
        };
        let name = {
            let st = self.inner.state.lock().unwrap();
            let entry = st
                .projects
                .get(&project.0)
                .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
            if let Some(d) = &domain
                && let Some(other) = st.projects.values().find(|e| {
                    e.record.id != project.0 && e.record.local_domain.as_deref() == Some(d)
                })
            {
                return Err(ErrorInfo::InvalidInput {
                    message: format!("{d} already belongs to {}", other.summary.name),
                });
            }
            entry.summary.name.clone()
        };

        let (id, handle) = self.new_operation();
        let engine = self.clone();
        tokio::spawn(async move {
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let label = match &domain {
                Some(d) => format!("Enable https://{d} — {name}"),
                None => format!("Disable local domain — {name}"),
            };
            let work = crate::history::with_context(
                crate::history::CommandContext {
                    label,
                    project: Some(project.clone()),
                    operation: Some(id),
                },
                engine.set_local_domain_work(&handle, id, &project, domain),
            );
            match work.await {
                Ok(()) => engine.emit_op(&handle, id, OperationEventKind::Completed),
                Err(e) => {
                    engine.flush_signature_explanations(&handle, id, Some(&project));
                    engine.emit_op(&handle, id, OperationEventKind::Failed { error: e.to_string() });
                }
            }
        });
        Ok(id)
    }

    async fn set_local_domain_work(
        &self,
        handle: &Arc<OpHandle>,
        id: OperationId,
        project: &ProjectId,
        domain: Option<String>,
    ) -> Result<(), ErrorInfo> {
        let out = |line: String| {
            self.emit_op(handle, id, OperationEventKind::Output { line, stderr: false });
        };

        // 1. Persist the claim; the summary carries it from here on.
        let records = self.with_state(|st, events| {
            if let Some(entry) = st.projects.get_mut(&project.0) {
                entry.record.local_domain = domain.clone();
                entry.summary.local_domain = domain.clone();
                events.push(PatchEvent::ProjectUpdated { project: entry.summary.clone() });
            }
            st.projects.values().map(|e| e.record.clone()).collect::<Vec<_>>()
        });
        self.inner.deps.store.save_projects(&records).map_err(internal_err)?;

        // 2. Every claimed domain, with the ports its app publishes today,
        // 3. and the proxy converged on that set — under the proxy lock, so
        //    a reconcile cannot write the set this claim is replacing.
        let sites = {
            let _proxy = self.inner.proxy_lock.lock().await;
            let sites = self.proxy_sites().await?;
            self.proxy_sync(handle, id, &sites).await?;
            sites
        };

        // 4. What Mast will not do behind the user's back, offered as
        //    previewed repairs instead.
        let Some(domain) = domain else {
            out("domain released — an /etc/hosts line pointing it at 127.0.0.1 is \
                 harmless and may stay"
                .into());
            return Ok(());
        };
        let site = sites.iter().find(|s| s.domain == domain);
        let port = site.map(|s| s.app_port).unwrap_or(80);
        out(format!("https://{domain} → localhost:{port} is configured on the proxy"));
        match site.and_then(|s| s.reverb) {
            Some(ReverbEndpoint { host_port: Some(host_port), .. }) => out(format!(
                "Reverb's WebSocket upgrades on wss://{domain}/app → localhost:{host_port}"
            )),
            Some(ReverbEndpoint { listen_port, host_port: None }) => out(format!(
                "this project runs Reverb, but its app service does not publish \
                 REVERB_SERVER_PORT ({listen_port}) to the host, so wss://{domain}/app has \
                 nowhere to go — add a ports entry such as \
                 '${{FORWARD_REVERB_PORT:-8080}}:${{REVERB_SERVER_PORT:-8080}}' and the proxy \
                 picks it up on its own"
            )),
            None => {}
        }
        if let Some(ca) = self.export_proxy_ca().await {
            out(format!(
                "root certificate exported to {} — import that file wherever system \
                 trust does not reach (Firefox, curl --cacert, NODE_EXTRA_CA_CERTS)",
                ca.path
            ));
        }

        let hosts_path = hosts_file_path();
        let hosts = tokio::fs::read_to_string(hosts_path).await.unwrap_or_default();
        if !hosts_resolves(&hosts, &domain) {
            out(format!(
                "{hosts_path} does not resolve {domain} yet — the browser cannot find \
                 the proxy until it does"
            ));
            self.offer_fix(handle, id, project, REPAIR_HOSTS_ENTRY, Some(&domain));
        }
        let trusted = self.proxy_ca_trusted().await;
        if !trusted {
            out("the proxy's certificate authority is not trusted yet — browsers will \
                 warn until it is (one-time step, shared by every local domain)"
                .into());
            self.offer_fix(handle, id, project, REPAIR_TRUST_PROXY_CA, None);
        }
        // System trust alone leaves Chromium-family browsers (Chrome,
        // Vivaldi, Brave, Edge) warning on Linux — they read NSS instead.
        let nss_gap = if trusted { self.proxy_nss_gap().await } else { None };
        match nss_gap {
            Some(mast_diagnostics::NssTrustGap::CertutilMissing) => {
                out("the system store trusts the certificate authority, but \
                     Chromium-family browsers (Chrome, Vivaldi, Brave, Edge) read \
                     ~/.pki/nssdb and certutil is not installed — Fix installs the NSS \
                     tools and finishes the job"
                    .into());
                self.offer_fix(handle, id, project, REPAIR_INSTALL_CERTUTIL, None);
            }
            Some(mast_diagnostics::NssTrustGap::CaMissing) => {
                out("the system store trusts the certificate authority, but the NSS \
                     store Chromium-family browsers read does not yet — Fix adds it \
                     (restart the browser afterwards)"
                    .into());
                self.offer_fix(handle, id, project, REPAIR_TRUST_PROXY_CA, None);
            }
            None => {}
        }
        if hosts_resolves(&hosts, &domain) && trusted && nss_gap.is_none() {
            out(format!("open https://{domain} — everything is in place"));
        }
        Ok(())
    }

    /// Is the proxy CA already trusted on this machine? Linux answers from
    /// the file the trust repair installs; macOS asks the keychain to verify
    /// the exported root (`security verify-cert`), because there is no
    /// marker file to look for.
    ///
    /// Linux carries a second, separate question: Chromium-family browsers
    /// read the NSS user store, not this one — [`proxy_nss_gap`] answers it.
    pub(crate) async fn proxy_ca_trusted(&self) -> bool {
        if cfg!(target_os = "macos") {
            let crt = self.inner.deps.store.proxy_dir().join("root.crt");
            if !crt.is_file() {
                return false;
            }
            let argv: Vec<String> =
                ["security", "verify-cert", "-c", &crt.to_string_lossy()]
                    .map(String::from)
                    .into();
            return run_command(&argv, None, &[], Duration::from_secs(10), OUTPUT_CAP)
                .await
                .ok()
                .is_some_and(|o| o.success());
        }
        std::path::Path::new("/usr/local/share/ca-certificates/mast-proxy.crt").exists()
            || std::path::Path::new("/etc/pki/ca-trust/source/anchors/mast-proxy.crt").exists()
    }

    /// The Chromium half of Linux trust: Chrome, Vivaldi, Brave and Edge
    /// read the NSS user store (`~/.pki/nssdb`), not the system store the
    /// trust repair fills first — a user can do everything asked and still
    /// meet ERR_CERT_AUTHORITY_INVALID. `None` = no gap (or macOS, where
    /// the keychain covers those browsers).
    pub(crate) async fn proxy_nss_gap(&self) -> Option<mast_diagnostics::NssTrustGap> {
        if cfg!(target_os = "macos") {
            return None;
        }
        let home = std::env::var_os("HOME")?;
        let db = format!("sql:{}", std::path::PathBuf::from(home).join(".pki/nssdb").display());
        let argv: Vec<String> =
            ["certutil", "-d", db.as_str(), "-L", "-n", NSS_NICKNAME].map(String::from).into();
        match run_command(&argv, None, &[], Duration::from_secs(10), OUTPUT_CAP).await {
            Ok(out) if out.success() => None,
            // certutil ran and the nickname is absent (a fresh or missing
            // database answers the same way).
            Ok(_) => Some(mast_diagnostics::NssTrustGap::CaMissing),
            // The spawn itself failed: certutil is not installed.
            Err(_) => Some(mast_diagnostics::NssTrustGap::CertutilMissing),
        }
    }

    /// Export the proxy CA's root certificate to the data dir and return it
    /// for manual trust (Firefox's import dialog, `curl --cacert`,
    /// `NODE_EXTRA_CA_CERTS`). Freshly copied from the container when it is
    /// up — retried briefly, because Caddy mints the CA on first boot — with
    /// a previously exported file as the fallback. `None` when neither
    /// exists yet.
    pub async fn export_proxy_ca(&self) -> Option<mast_contract::ProxyCa> {
        let dir = self.inner.deps.store.proxy_dir();
        tokio::fs::create_dir_all(&dir).await.ok()?;
        let crt = dir.join("root.crt");
        let cp: Vec<String> = ["docker", "cp", CA_IN_CONTAINER, &crt.to_string_lossy()]
            .map(String::from)
            .into();
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            match run_command(&cp, None, &[], Duration::from_secs(10), OUTPUT_CAP).await {
                Ok(o) if o.success() => break,
                _ => {}
            }
        }
        let pem = tokio::fs::read_to_string(&crt).await.ok()?;
        Some(mast_contract::ProxyCa { path: crt.to_string_lossy().into_owned(), pem })
    }

    fn offer_fix(
        &self,
        handle: &Arc<OpHandle>,
        id: OperationId,
        project: &ProjectId,
        repair: &str,
        arg: Option<&str>,
    ) {
        if let Some(spec) = mast_diagnostics::repair_spec(repair, arg) {
            self.emit_op(
                handle,
                id,
                OperationEventKind::FixAvailable {
                    repair: crate::diagnostics::offer_to_contract(spec),
                    project: project.clone(),
                },
            );
        }
    }

    /// Every claimed domain as the proxy should serve it now. Ordered by
    /// project id, so an unchanged set renders byte-identically.
    async fn proxy_sites(&self) -> Result<Vec<ProxySite>, ErrorInfo> {
        let claims: Vec<(String, PathBuf, Option<ResolvedModel>)> = {
            let st = self.inner.state.lock().unwrap();
            st.projects
                .values()
                .filter_map(|e| {
                    let domain = e.record.local_domain.clone()?;
                    Some((domain, e.record.path.clone(), e.model.clone()))
                })
                .collect()
        };
        tokio::task::spawn_blocking(move || {
            claims
                .into_iter()
                .map(|(domain, path, model)| proxy_site(domain, &path, model.as_ref()))
                .collect()
        })
        .await
        .map_err(internal_err)
    }

    /// Keep a running proxy in step with the projects it serves. Otherwise
    /// the Caddyfile only changes when a domain is claimed or released, so a
    /// moved APP_PORT, or a Reverb port published after the domain was
    /// enabled, goes unserved until the next toggle. Runs after every
    /// reconcile and does nothing unless the rendered file would change. It
    /// never starts or removes the container — that stays with the enable
    /// operation, where its output has somewhere to go.
    pub(crate) async fn converge_proxy(&self) {
        let Some(_proxy) = self.refresh_caddyfile().await else { return };
        // A stopped proxy refuses the exec, and reads the file when it starts.
        match run_command(&reload_argv(), None, &[], RELOAD_TIMEOUT, OUTPUT_CAP).await {
            Ok(o) if o.success() => {}
            Ok(o) => tracing::warn!("proxy reload failed: {}", o.stderr.trim()),
            Err(e) => tracing::warn!("proxy reload failed: {e}"),
        }
    }

    /// Rewrite the Caddyfile if what it should say has changed. Returns the
    /// proxy lock, still held for the reload, when it did. Only the owning
    /// engine writes, only a proxy that exists is followed, and an enable in
    /// flight wins — it is converging on newer state already.
    async fn refresh_caddyfile(&self) -> Option<tokio::sync::MutexGuard<'_, ()>> {
        if self.read_only() {
            return None;
        }
        let proxy = self.inner.proxy_lock.try_lock().ok()?;
        let caddyfile = self.inner.deps.store.proxy_dir().join("Caddyfile");
        // No file: no domain was ever enabled here, so there is no proxy.
        let current = tokio::fs::read_to_string(&caddyfile).await.ok()?;
        let sites = self.proxy_sites().await.ok()?;
        // Releasing the last domain removed the proxy; nothing to follow.
        if sites.is_empty() {
            return None;
        }
        let body = render_caddyfile(&sites);
        if body == current {
            return None;
        }
        if let Err(e) = tokio::fs::write(&caddyfile, &body).await {
            tracing::warn!("cannot rewrite {}: {e}", caddyfile.display());
            return None;
        }
        Some(proxy)
    }

    /// Make the running proxy match `sites`: rewrite the Caddyfile, then
    /// start/reload/remove the container as the set demands. Failure lines
    /// stream through `emit_op`, so a taken port 80/443 gets the same
    /// signature explanation as any other operation.
    async fn proxy_sync(
        &self,
        handle: &Arc<OpHandle>,
        id: OperationId,
        sites: &[ProxySite],
    ) -> Result<(), ErrorInfo> {
        let out = |line: String| {
            self.emit_op(handle, id, OperationEventKind::Output { line, stderr: false });
        };
        let dir = self.inner.deps.store.proxy_dir();
        let caddyfile = dir.join("Caddyfile");
        let body = render_caddyfile(sites);
        let write_to = caddyfile.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            std::fs::create_dir_all(write_to.parent().unwrap())?;
            std::fs::write(&write_to, body)
        })
        .await
        .map_err(internal_err)?
        .map_err(internal_err)?;

        if sites.is_empty() {
            let rm: Vec<String> =
                ["docker", "rm", "-f", PROXY_CONTAINER].map(String::from).into();
            let _ = run_command(&rm, None, &[], DOCKER_TIMEOUT, OUTPUT_CAP).await;
            out("no local domains remain — the proxy container was removed".into());
            return Ok(());
        }

        let inspect: Vec<String> =
            ["docker", "inspect", "-f", "{{.State.Running}}", PROXY_CONTAINER]
                .map(String::from)
                .into();
        let running = run_command(&inspect, None, &[], DOCKER_TIMEOUT, OUTPUT_CAP)
            .await
            .ok()
            .filter(|o| o.success())
            .map(|o| o.stdout.trim() == "true");

        if running == Some(true) {
            let result = run_command(&reload_argv(), None, &[], DOCKER_TIMEOUT, OUTPUT_CAP)
                .await
                .map_err(internal_err)?;
            if result.success() {
                out("proxy reloaded with the new domain set".into());
                return Ok(());
            }
            // A reload can fail when the container predates this config —
            // fall through and recreate it from scratch.
            out("reload failed — recreating the proxy container".into());
        }

        // The proxy needs host 80/443 for itself. When a RUNNING Mast
        // project already publishes one of them (a pre-:8000 bootstrap
        // publishing 80, say), the docker run below is doomed with "port is
        // already allocated" — and the Fix must target the HOLDER project's
        // ports, not the project whose domain is being enabled.
        let holder: Option<(mast_contract::ProjectId, String, u16)> = {
            let st = self.inner.state.lock().unwrap();
            st.projects
                .values()
                .filter(|e| e.summary.status != mast_contract::ProjectStatus::Stopped)
                .find_map(|e| {
                    e.host_ports
                        .iter()
                        .find(|(_, p)| *p == 80 || *p == 443)
                        .map(|(_, p)| (e.summary.id.clone(), e.summary.name.clone(), *p))
                })
        };
        if let Some((holder_id, holder_name, port)) = holder {
            out(format!(
                "{holder_name} publishes host port {port}, which the proxy itself needs \
                 — the Fix below moves {holder_name}'s ports (APP_PORT and friends in \
                 its .env), then enable the domain again"
            ));
            if let Some(spec) =
                mast_diagnostics::repair_spec(mast_diagnostics::REPAIR_REASSIGN_PORTS, None)
            {
                self.emit_op(
                    handle,
                    id,
                    OperationEventKind::FixAvailable {
                        repair: crate::diagnostics::offer_to_contract(spec),
                        project: holder_id,
                    },
                );
            }
            return Err(ErrorInfo::Conflict {
                message: format!(
                    "the proxy needs host port {port}, but {holder_name} publishes it"
                ),
            });
        }
        let rm: Vec<String> = ["docker", "rm", "-f", PROXY_CONTAINER].map(String::from).into();
        let _ = run_command(&rm, None, &[], DOCKER_TIMEOUT, OUTPUT_CAP).await;
        // The old proxy is gone; anything still listening on 80/443 is an
        // outside squatter the docker run below cannot beat. Name each
        // platform's usual suspect instead of letting docker's "port is
        // already allocated" do the explaining. Both address families — a
        // v6-only listener blocks the bind just as hard.
        for port in [80u16, 443] {
            let connect = |addr: &'static str| async move {
                tokio::time::timeout(
                    std::time::Duration::from_millis(600),
                    tokio::net::TcpStream::connect((addr, port)),
                )
                .await
                .map(|r| r.is_ok())
                .unwrap_or(false)
            };
            if connect("127.0.0.1").await || connect("::1").await {
                let hint = if cfg!(windows) {
                    "on Windows this is often IIS (World Wide Web Publishing Service) or \
                     another web stack — an elevated `netstat -abno` names the holder"
                } else if cfg!(target_os = "macos") {
                    "often a local Apache/nginx — `sudo lsof -iTCP:80 -sTCP:LISTEN` names \
                     the holder"
                } else {
                    "often a local Apache/nginx — `sudo ss -tlnp` names the holder"
                };
                out(format!(
                    "something outside Mast is listening on port {port}, which the proxy \
                     needs — {hint}; stop or reconfigure it, then enable the domain again"
                ));
                return Err(ErrorInfo::Conflict {
                    message: format!("port {port} is held by a program outside Mast"),
                });
            }
        }
        out(format!(
            "starting {PROXY_CONTAINER} ({PROXY_IMAGE}) on ports 80/443 — first run \
             pulls the image"
        ));
        let caddyfile_mount = format!("{}:/etc/caddy/Caddyfile:ro", caddyfile.display());
        let argv: Vec<String> = [
            "docker",
            "run",
            "-d",
            "--name",
            PROXY_CONTAINER,
            "--restart",
            "unless-stopped",
            "-p",
            "80:80",
            "-p",
            "443:443",
            "--add-host",
            "host.docker.internal:host-gateway",
            "-v",
            caddyfile_mount.as_str(),
            "-v",
            "mast-proxy-data:/data",
            PROXY_IMAGE,
        ]
        .map(String::from)
        .into();
        self.run_streamed_command(
            handle,
            id,
            &argv,
            None,
            &crate::Redactor::default(),
            DOCKER_TIMEOUT,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The UAC wrapper is pure argv+file construction, so its shape is
    /// provable everywhere: the generated .cmd carries the lines verbatim
    /// and the PowerShell command propagates the exit code.
    #[test]
    fn windows_elevation_wraps_a_generated_cmd_with_exit_propagation() {
        let argv = windows_elevated_cmd(&[
            "echo 127.0.0.1\thoo.test\t# mast local domain>>C:\\hosts".to_string(),
            "ipconfig /flushdns".to_string(),
        ])
        .unwrap();
        assert_eq!(argv[0], "powershell");
        assert!(argv[3].contains("-Verb RunAs"), "{}", argv[3]);
        assert!(argv[3].contains("exit $p.ExitCode"), "{}", argv[3]);
        let cmd = std::env::temp_dir().join("mast-elevate/mast-elevated.cmd");
        let body = std::fs::read_to_string(cmd).unwrap();
        assert!(body.starts_with("@echo off\r\n"), "{body}");
        assert!(body.contains("hoo.test"), "{body}");
        assert!(body.contains("ipconfig /flushdns"), "{body}");
    }

    #[test]
    fn domain_validation_admits_test_names_and_nothing_odd() {
        assert!(validate_local_domain("myapp.test").is_ok());
        assert!(validate_local_domain("my-app.localhost").is_ok());
        assert!(validate_local_domain("a.b.test").is_ok());
        for bad in [
            "myapp.dev",       // real TLD — would shadow public DNS
            "MyApp.test",      // normalized before validation, never accepted raw
            ".test",           // empty label
            "my_app.test",     // underscore
            "-app.test",       // leading hyphen
            "app.test; rm -rf", // anything shell-ish
            "test",            // no reserved suffix
        ] {
            assert!(validate_local_domain(bad).is_err(), "accepted: {bad}");
        }
    }

    #[test]
    fn privileged_argv_builders_escape_for_their_layer() {
        assert_eq!(
            pkexec_argv("printf 'x' >> /etc/hosts"),
            vec!["pkexec", "sh", "-c", "printf 'x' >> /etc/hosts"]
        );
        let mac = osascript_admin_argv("install -m 644 'a \"b\"' /dest && refresh");
        assert_eq!(mac[0], "osascript");
        assert_eq!(mac[1], "-e");
        // The shell script rides inside an AppleScript string literal: its
        // quotes must arrive escaped, and the wrapper must close cleanly.
        assert_eq!(
            mac[2],
            "do shell script \"install -m 644 'a \\\"b\\\"' /dest && refresh\" \
             with administrator privileges"
        );
    }

    #[test]
    fn hosts_matching_is_token_wise() {
        let hosts = "127.0.0.1\tlocalhost\n127.0.0.1 myapp.test # mast\n# 127.0.0.1 off.test\n";
        assert!(hosts_resolves(hosts, "myapp.test"));
        assert!(hosts_resolves(hosts, "MYAPP.TEST"));
        assert!(!hosts_resolves(hosts, "app.test"));
        assert!(!hosts_resolves(hosts, "off.test"), "commented lines resolve nothing");
        assert!(!hosts_resolves("myapp.test extra", "myapp.test"), "first token is the address");
    }

    #[test]
    fn caddyfile_lists_every_claim_under_the_internal_ca() {
        let site =
            |domain: &str, app_port| ProxySite { domain: domain.into(), app_port, reverb: None };
        let body = render_caddyfile(&[site("storefront.test", 8082), site("billing.test", 80)]);
        assert!(body.contains("local_certs"));
        assert!(body.contains("skip_install_trust"));
        assert!(body.contains("storefront.test {\n\treverse_proxy host.docker.internal:8082"));
        assert!(body.contains("billing.test {\n\treverse_proxy host.docker.internal:80"));
    }

    /// Only the WebSocket upgrade goes to Reverb — `/app/…` is an ordinary
    /// route prefix in plenty of apps — and a Reverb the host cannot reach
    /// gets no route rather than one into a closed port.
    #[test]
    fn reverb_upgrades_get_their_own_route() {
        let body = render_caddyfile(&[
            ProxySite {
                domain: "chat.test".into(),
                app_port: 8087,
                reverb: Some(ReverbEndpoint { listen_port: 8080, host_port: Some(8088) }),
            },
            ProxySite {
                domain: "billing.test".into(),
                app_port: 80,
                reverb: Some(ReverbEndpoint { listen_port: 8080, host_port: None }),
            },
        ]);
        assert!(
            body.contains(
                "chat.test {\n\t@reverb {\n\t\tpath /app/*\n\t\theader Connection *Upgrade*\n\t\t\
                 header Upgrade websocket\n\t}\n\treverse_proxy @reverb host.docker.internal:8088\n\t\
                 reverse_proxy host.docker.internal:8087\n}"
            ),
            "{body}"
        );
        assert!(body.contains("billing.test {\n\treverse_proxy host.docker.internal:80\n}"), "{body}");
        assert_eq!(body.matches("@reverb {").count(), 1, "{body}");
    }

    fn model_publishing(service: &str, port_targets: Vec<(u16, u16)>) -> ResolvedModel {
        ResolvedModel {
            name: "app".into(),
            services: vec![mast_compose::ResolvedService {
                name: service.into(),
                image: None,
                aliases: Vec::new(),
                published_ports: port_targets.iter().map(|(host, _)| *host).collect(),
                volumes: Vec::new(),
                port_targets,
            }],
            external_networks: Vec::new(),
        }
    }

    /// The route follows the app service's published mapping, so moving
    /// the forwarded port moves the route with it.
    #[test]
    fn reverb_routes_to_the_port_the_app_service_publishes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".env"),
            "APP_PORT=8087\nAPP_SERVICE=chat.test\nBROADCAST_CONNECTION=reverb\n\
             REVERB_SERVER_PORT=8080\n",
        )
        .unwrap();
        let model = model_publishing("chat.test", vec![(8087, 80), (8088, 8080)]);
        let site = proxy_site("chat.test".into(), dir.path(), Some(&model));
        assert_eq!(site.app_port, 8087);
        assert_eq!(site.reverb, Some(ReverbEndpoint { listen_port: 8080, host_port: Some(8088) }));

        // Not published: Reverb is known, the host port is not.
        let model = model_publishing("chat.test", vec![(8087, 80)]);
        let site = proxy_site("chat.test".into(), dir.path(), Some(&model));
        assert_eq!(site.reverb, Some(ReverbEndpoint { listen_port: 8080, host_port: None }));
    }

    fn engine_in(dir: &Path, ownership: crate::Ownership) -> Engine {
        Engine::new(
            crate::EngineConfig::default(),
            crate::EngineDeps {
                connector: Arc::new(crate::RealConnector),
                store: mast_project::MetadataStore::open(dir.join("meta")).unwrap(),
                process_env: Default::default(),
                runner: Arc::new(crate::RealLifecycleRunner),
                ownership,
            },
        )
    }

    /// Put a project that claims `domain` into the engine's state, as a
    /// reconcile would have left it.
    fn claim(engine: &Engine, path: &Path, domain: &str, model: ResolvedModel) {
        let record = mast_project::ProjectRecord {
            id: "chat".into(),
            path: path.to_path_buf(),
            display_name: "chat".into(),
            is_sail: true,
            commands: Vec::new(),
            local_domain: Some(domain.into()),
            rank: 0,
        };
        let entry = crate::ProjectEntry {
            summary: crate::initial_summary(&record),
            record,
            invocation: None,
            model: Some(model),
            redactor: crate::Redactor::default(),
            app_port: None,
            host_ports: Vec::new(),
            compose_fingerprint: None,
            manifest: crate::manifest::Manifest::default(),
        };
        engine.inner.state.lock().unwrap().projects.insert(entry.record.id.clone(), entry);
    }

    /// A Reverb port published after the domain was enabled reaches the
    /// Caddyfile on the next reconcile, and an unchanged set leaves the file
    /// — and the running proxy — alone.
    #[tokio::test]
    async fn the_caddyfile_follows_ports_published_after_the_domain_was_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("chat");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(".env"),
            "APP_PORT=8087\nAPP_SERVICE=chat.test\nBROADCAST_CONNECTION=reverb\n",
        )
        .unwrap();
        let engine =
            engine_in(dir.path(), crate::acquire_ownership(Some(dir.path().join("lock"))));
        claim(&engine, &project, "chat.test", model_publishing("chat.test", vec![(8087, 80)]));
        // What the enable wrote, before Reverb's port was published.
        let caddyfile = engine.inner.deps.store.proxy_dir().join("Caddyfile");
        std::fs::create_dir_all(caddyfile.parent().unwrap()).unwrap();
        std::fs::write(&caddyfile, render_caddyfile(&engine.proxy_sites().await.unwrap()))
            .unwrap();
        assert!(engine.refresh_caddyfile().await.is_none(), "an unchanged set must not rewrite");

        engine.inner.state.lock().unwrap().projects.get_mut("chat").unwrap().model =
            Some(model_publishing("chat.test", vec![(8087, 80), (8088, 8080)]));
        assert!(engine.refresh_caddyfile().await.is_some());
        let body = std::fs::read_to_string(&caddyfile).unwrap();
        assert!(body.contains("reverse_proxy @reverb host.docker.internal:8088"), "{body}");
        assert!(engine.refresh_caddyfile().await.is_none(), "converged means quiet");
    }

    /// Following is not creating: no Caddyfile means no proxy to follow, and
    /// an engine that does not own mutation never writes one.
    #[tokio::test]
    async fn only_an_owning_engine_follows_a_proxy_that_exists() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("chat");
        std::fs::create_dir_all(&project).unwrap();
        let model = model_publishing("laravel.test", vec![(80, 80)]);

        let owner = engine_in(dir.path(), crate::acquire_ownership(Some(dir.path().join("lock"))));
        claim(&owner, &project, "chat.test", model.clone());
        let caddyfile = owner.inner.deps.store.proxy_dir().join("Caddyfile");
        assert!(owner.refresh_caddyfile().await.is_none());
        assert!(!caddyfile.exists(), "no domain was ever enabled — nothing to create");

        std::fs::create_dir_all(caddyfile.parent().unwrap()).unwrap();
        std::fs::write(&caddyfile, "stale").unwrap();
        let spectator = engine_in(dir.path(), crate::Ownership::Spectator);
        claim(&spectator, &project, "chat.test", model);
        assert!(spectator.refresh_caddyfile().await.is_none());
        assert_eq!(std::fs::read_to_string(&caddyfile).unwrap(), "stale");
    }

    #[test]
    fn a_project_without_reverb_gets_no_socket_route_even_when_8080_is_published() {
        // No .env at all: Sail's defaults.
        let dir = tempfile::tempdir().unwrap();
        let model = model_publishing("laravel.test", vec![(80, 80), (8080, 8080)]);
        let site = proxy_site("shop.test".into(), dir.path(), Some(&model));
        assert_eq!(site, ProxySite { domain: "shop.test".into(), app_port: 80, reverb: None });
    }
}
