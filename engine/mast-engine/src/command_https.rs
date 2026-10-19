//! Local HTTPS for host commands (ADR-0014): a dev server Mast runs on this
//! machine — a separate frontend's `vp dev` beside a Sail API — claims a
//! `.test` domain on the same proxy as the app. The proxy forwards to the
//! port the command announces in its own output, so nothing about the dev
//! server needs configuring and an auto-bumped port (5173 busy → 5174) is
//! followed rather than broken. Reading output instead of the process table
//! is what makes this the same on every platform: the `/proc` walk behind
//! host runs (ADR-0012) exists only on Linux.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use mast_contract::{
    CommandDomain, CommandPort, ErrorInfo, OperationEventKind, OperationId, PatchEvent, ProjectId,
};
use mast_project::CommandDomainRecord;

use crate::ops::OpHandle;
use crate::proxy::{DomainProbe, ProxyNetwork, ProxySite, SiteTarget};
use crate::{Engine, ProjectEntry, internal_err};

/// Drop terminal escapes: Vite prints its port in bold.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters, then one final byte in @..~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC (terminal hyperlinks): up to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// Words dev servers put beside the address they serve on: Vite, Nuxt,
/// Astro and Storybook's "Local:", webpack-dev-server's "Loopback:",
/// "listening on", artisan's "Server running on", Django's "development
/// server at", Hugo's "available at", Python's "Serving HTTP".
const ANNOUNCEMENTS: &[&str] = &[
    "local:",
    "local ",
    "loopback:",
    "listening",
    "running on",
    "running at",
    "available at",
    "serving",
    "server at",
    "started server on",
];

/// The port a dev server says it serves on, from one line of its output:
/// the first `http(s)://host:port` in a line that announces an address.
/// A URL without a port, or one in a line that is not an announcement — an
/// app logging a request to some other service — names no port of its own.
pub(crate) fn announced_port(line: &str) -> Option<u16> {
    let text = strip_ansi(line);
    let lower = text.to_ascii_lowercase();
    // Vite's LAN address repeats the same port; the Local line is enough.
    if lower.contains("network:") || !ANNOUNCEMENTS.iter().any(|w| lower.contains(w)) {
        return None;
    }
    let start = lower.find("http://").or_else(|| lower.find("https://"))?;
    let authority = text[start..].split_once("://")?.1;
    // The host: an IPv6 literal through its bracket ([::]:8000), else a
    // name — which ends wherever the URL does, even inside artisan's
    // "[http://127.0.0.1:8000]".
    let host_end = if authority.starts_with('[') {
        authority.find(']')? + 1
    } else {
        authority
            .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
            .unwrap_or(authority.len())
    };
    let digits: String = authority[host_end..]
        .strip_prefix(':')?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    let port: u16 = digits.parse().ok()?;
    (port != 0).then_some(port)
}

/// The project's command of this name, when it runs on this machine. A
/// `sail …` command runs in the app container, whose address is the app's.
pub(crate) fn host_command<'a>(
    entry: &'a ProjectEntry,
    name: &str,
) -> Option<&'a mast_contract::ProjectCommand> {
    entry
        .summary
        .commands
        .iter()
        .find(|c| c.name == name)
        .filter(|c| c.command.split_whitespace().next() != Some("sail"))
}

pub(crate) fn to_contract(claims: &[CommandDomainRecord]) -> Vec<CommandDomain> {
    claims
        .iter()
        .map(|c| CommandDomain {
            command: c.command.clone(),
            domain: c.domain.clone(),
            port: c.port,
            pinned: c.pinned,
        })
        .collect()
}

/// How the proxy serves one command claim right now.
pub(crate) fn command_site(entry: &ProjectEntry, claim: &CommandDomainRecord) -> ProxySite {
    let project = &entry.summary.name;
    let command = &claim.command;
    let target = if entry.summary.commands.iter().all(|c| c.name != *command) {
        SiteTarget::Explain(format!(
            "{command} is no longer one of {project}'s commands. Claim this domain for \
             another command in Mast's HTTPS dialog, or release it."
        ))
    } else if host_command(entry, command).is_none() {
        SiteTarget::Explain(format!(
            "{command} runs in {project}'s app container now, so it is served at the \
             app's own domain."
        ))
    } else {
        match claim.port {
            Some(port) => SiteTarget::Port(port),
            None => SiteTarget::Explain(format!(
                "{command} has not said which port it serves on yet. Run it from Mast and \
                 this address follows the one it prints, or pin a port in the HTTPS dialog."
            )),
        }
    };
    ProxySite { domain: claim.domain.clone(), target, reverb: None }
}

/// Who is asking for a domain, so their own claim does not count as taken.
pub(crate) enum Claimant {
    /// A project's app (`SetLocalDomain`), by project id.
    App(String),
    /// One of a project's commands, by project id and command name.
    Command(String, String),
}

/// Who holds `domain` besides `claimant`, described for an error message.
/// A same-project claim whose command no longer exists does not count for
/// a command claimant: claiming its domain for another command moves it,
/// which is how a renamed command gets its address back.
pub(crate) fn domain_holder(
    projects: &BTreeMap<String, ProjectEntry>,
    domain: &str,
    claimant: &Claimant,
) -> Option<String> {
    for entry in projects.values() {
        let id = entry.record.id.as_str();
        let name = &entry.summary.name;
        if entry.record.local_domain.as_deref() == Some(domain)
            && !matches!(claimant, Claimant::App(p) if p == id)
        {
            return Some(format!("{name}'s app"));
        }
        for claim in entry.record.command_domains.iter().filter(|c| c.domain == domain) {
            let own = match claimant {
                Claimant::Command(p, command) => {
                    p == id
                        && (claim.command == *command
                            || entry.summary.commands.iter().all(|c| c.name != claim.command))
                }
                Claimant::App(_) => false,
            };
            if !own {
                return Some(format!("{name}'s command {}", claim.command));
            }
        }
    }
    None
}

/// The project's claim list after `command` claims `domain` (or releases
/// its claim, with `None`). `port` pins; without it the claim follows what
/// the command announces — what it announced already, when it is running.
pub(crate) fn plan_command_claim(
    projects: &BTreeMap<String, ProjectEntry>,
    project: &str,
    command: &str,
    domain: Option<&str>,
    port: Option<u16>,
) -> Result<Vec<CommandDomainRecord>, ErrorInfo> {
    let entry = projects
        .get(project)
        .ok_or_else(|| ErrorInfo::NotFound { what: format!("project {project}") })?;
    let mut claims = entry.record.command_domains.clone();
    let Some(domain) = domain else {
        claims.retain(|c| c.command != command);
        return Ok(claims);
    };
    crate::proxy::validate_local_domain(domain)?;
    if entry.summary.commands.iter().all(|c| c.name != command) {
        return Err(ErrorInfo::NotFound { what: format!("command \"{command}\"") });
    }
    if host_command(entry, command).is_none() {
        return Err(ErrorInfo::InvalidInput {
            message: format!(
                "\"{command}\" runs in the app container (sail …) — it is reached through \
                 the app's own domain"
            ),
        });
    }
    if port == Some(0) {
        return Err(ErrorInfo::InvalidInput { message: "port 0 is not a port".into() });
    }
    let claimant = Claimant::Command(project.to_string(), command.to_string());
    if let Some(holder) = domain_holder(projects, domain, &claimant) {
        return Err(ErrorInfo::InvalidInput {
            message: format!("{domain} already belongs to {holder}"),
        });
    }
    let announced =
        entry.summary.command_ports.iter().find(|p| p.command == command).map(|p| p.port);
    let commands = &entry.summary.commands;
    let orphaned = |c: &CommandDomainRecord| commands.iter().all(|x| x.name != c.command);
    // What this command had — or, for a renamed command, what its old
    // name left on this domain.
    let previous = claims
        .iter()
        .find(|c| c.command == command)
        .or_else(|| claims.iter().find(|c| c.domain == domain && orphaned(c)));
    let next = CommandDomainRecord {
        command: command.to_string(),
        domain: domain.to_string(),
        port: port.or(announced).or_else(|| previous.filter(|c| !c.pinned).and_then(|c| c.port)),
        pinned: port.is_some(),
    };
    // A stale claim on the same domain moves here; this command's own
    // claim is replaced where it stood, so the list keeps its order.
    claims.retain(|c| c.command == command || c.domain != domain || !orphaned(c));
    match claims.iter_mut().find(|c| c.command == command) {
        Some(slot) => *slot = next,
        None => claims.push(next),
    }
    Ok(claims)
}

/// The registrable part of a local name — its last two labels. Neither
/// `.test` nor `.localhost` is on the public suffix list, so browsers apply
/// the default rule and treat the TLD itself as the suffix.
pub(crate) fn site_of(domain: &str) -> &str {
    let mut dots = domain.rmatch_indices('.');
    match (dots.next(), dots.next()) {
        (Some(_), Some((second, _))) => &domain[second + 1..],
        _ => domain,
    }
}

/// What stands between a Laravel API on `app` and a frontend on each of
/// `frontends` sharing a cookie session (Sanctum's SPA authentication).
/// Mast reads the `.env`, it does not write it: each finding says what to
/// change.
pub(crate) fn cookie_findings(
    env: &HashMap<String, String>,
    sanctum: bool,
    app: &str,
    frontends: &[&str],
) -> Vec<String> {
    let mut out = Vec::new();
    let site = site_of(app);
    for frontend in frontends {
        if site_of(frontend) != site {
            out.push(format!(
                "{app} and {frontend} are different sites, so cookies the API sets are not \
                 sent with the frontend's requests — give both one parent domain \
                 (api.example.test and web.example.test share example.test)"
            ));
        }
    }
    if !out.is_empty() || !sanctum {
        return out;
    }
    let session_domain = env
        .get("SESSION_DOMAIN")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty() && *v != "null");
    match session_domain {
        Some(value) => {
            let scope = value.trim_start_matches('.');
            let covers = |d: &str| d == scope || d.ends_with(&format!(".{scope}"));
            for domain in std::iter::once(&app).chain(frontends) {
                if !covers(domain) {
                    out.push(format!(
                        "SESSION_DOMAIN={value} in .env does not cover {domain}, so the \
                         browser drops the session cookie there — SESSION_DOMAIN=.{site} \
                         covers every address on this site"
                    ));
                }
            }
        }
        None => out.push(format!(
            "SESSION_DOMAIN is not set in .env, so the API's cookies stay on {app} and the \
             frontend cannot read XSRF-TOKEN — SESSION_DOMAIN=.{site} shares them"
        )),
    }
    let stateful: Vec<String> = env
        .get("SANCTUM_STATEFUL_DOMAINS")
        .map(|v| {
            v.split(',')
                .map(|d| {
                    let d = d.trim();
                    let d = d.split_once("://").map_or(d, |(_, rest)| rest);
                    let d = d.split('/').next().unwrap_or(d);
                    d.strip_suffix(":443").unwrap_or(d).to_ascii_lowercase()
                })
                .collect()
        })
        .unwrap_or_default();
    for frontend in frontends {
        if !stateful.iter().any(|d| d == frontend) {
            out.push(format!(
                "SANCTUM_STATEFUL_DOMAINS in .env does not list {frontend}, so Sanctum treats \
                 its requests as stateless and a login does not stick — add {frontend}"
            ));
        }
    }
    out
}

/// [`cookie_findings`] for a project's claims as they stand, when it is a
/// Laravel app with both an app domain and at least one command domain.
pub(crate) fn cookie_findings_for(engine: &Engine, project: &ProjectId) -> Vec<String> {
    let claims = {
        let st = engine.inner.state.lock().unwrap();
        st.projects.get(&project.0).and_then(|e| {
            let app = e.record.local_domain.clone()?;
            let frontends: Vec<String> =
                e.record.command_domains.iter().map(|c| c.domain.clone()).collect();
            (!frontends.is_empty()).then(|| (e.record.path.clone(), app, frontends))
        })
    };
    let Some((path, app, frontends)) = claims else { return Vec::new() };
    if !path.join("artisan").is_file() {
        return Vec::new();
    }
    let env = mast_compose::parse_env_file(&path.join(".env"));
    let sanctum = std::fs::read_to_string(path.join("composer.json"))
        .is_ok_and(|composer| composer.contains("\"laravel/sanctum\""));
    let frontends: Vec<&str> = frontends.iter().map(String::as_str).collect();
    cookie_findings(&env, sanctum, &app, &frontends)
}

/// Held while a host command runs: output lines are read for the port it
/// announces ([`Engine::note_command_port`]), and the announcement is
/// withdrawn when the run ends.
pub(crate) struct AnnounceGuard {
    engine: Engine,
    project: ProjectId,
    command: String,
}

impl AnnounceGuard {
    pub(crate) fn new(engine: &Engine, handle: &OpHandle, project: &ProjectId, command: &str) -> Self {
        *handle.announces.lock().unwrap() = Some((project.clone(), command.to_string()));
        Self { engine: engine.clone(), project: project.clone(), command: command.to_string() }
    }
}

impl Drop for AnnounceGuard {
    fn drop(&mut self) {
        self.engine.with_state(|st, events| {
            if let Some(entry) = st.projects.get_mut(&self.project.0) {
                let before = entry.summary.command_ports.len();
                entry.summary.command_ports.retain(|p| p.command != self.command);
                if entry.summary.command_ports.len() != before {
                    events.push(PatchEvent::ProjectUpdated { project: entry.summary.clone() });
                }
            }
        });
    }
}

impl Engine {
    /// A running host command announced `port`. Recorded for every client
    /// to see, and followed by the command's domain claim unless its port
    /// is pinned — persisted, with the proxy converged right away rather
    /// than at the next reconcile.
    pub(crate) fn note_command_port(&self, project: &ProjectId, command: &str, port: u16) {
        let follow = self.with_state(|st, events| {
            let entry = st.projects.get_mut(&project.0)?;
            if entry.summary.command_ports.iter().any(|p| p.command == command && p.port == port) {
                return None;
            }
            entry.summary.command_ports.retain(|p| p.command != command);
            entry.summary.command_ports.push(CommandPort { command: command.to_string(), port });
            let claim = entry
                .record
                .command_domains
                .iter_mut()
                .find(|c| c.command == command && !c.pinned && c.port != Some(port));
            let moved = claim.map(|c| c.port = Some(port)).is_some();
            if moved {
                entry.summary.command_domains = to_contract(&entry.record.command_domains);
            }
            events.push(PatchEvent::ProjectUpdated { project: entry.summary.clone() });
            moved.then(|| st.projects.values().map(|e| e.record.clone()).collect::<Vec<_>>())
        });
        let Some(records) = follow else { return };
        if self.read_only() {
            return;
        }
        if let Err(e) = self.inner.deps.store.save_projects(&records) {
            tracing::warn!("cannot persist {command}'s announced port: {e}");
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let engine = self.clone();
            runtime.spawn(async move { engine.converge_proxy().await });
        }
    }

    pub(crate) fn dispatch_set_command_domain(
        &self,
        project: ProjectId,
        command: String,
        domain: Option<String>,
        port: Option<u16>,
    ) -> Result<OperationId, ErrorInfo> {
        let domain = domain.map(|d| d.trim().to_ascii_lowercase());
        // Refused here, before an operation exists, so a dialog shows why.
        let name = {
            let st = self.inner.state.lock().unwrap();
            plan_command_claim(&st.projects, &project.0, &command, domain.as_deref(), port)?;
            st.projects[&project.0].summary.name.clone()
        };
        let (id, handle) = self.new_operation();
        *handle.project.lock().unwrap() = Some(project.clone());
        let engine = self.clone();
        tokio::spawn(async move {
            engine.emit_op(&handle, id, OperationEventKind::Started);
            let label = match &domain {
                Some(d) => format!("Enable https://{d} — {name} · {command}"),
                None => format!("Disable local domain — {name} · {command}"),
            };
            let work = crate::history::with_context(
                crate::history::CommandContext {
                    label,
                    project: Some(project.clone()),
                    operation: Some(id),
                },
                engine.set_command_domain_work(&handle, id, &project, &command, domain, port),
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

    async fn set_command_domain_work(
        &self,
        handle: &Arc<OpHandle>,
        id: OperationId,
        project: &ProjectId,
        command: &str,
        domain: Option<String>,
        port: Option<u16>,
    ) -> Result<(), ErrorInfo> {
        let out = |line: String| {
            self.emit_op(handle, id, OperationEventKind::Output { line, stderr: false });
        };
        let released = {
            let st = self.inner.state.lock().unwrap();
            st.projects.get(&project.0).and_then(|e| {
                e.record.command_domains.iter().find(|c| c.command == command).map(|c| c.domain.clone())
            })
        };

        // 1. Planned again against the state as it is now — another claim
        //    may have landed since dispatch — and persisted in the same step.
        let records = self.with_state(|st, events| {
            let claims =
                plan_command_claim(&st.projects, &project.0, command, domain.as_deref(), port)?;
            let entry = st.projects.get_mut(&project.0).expect("planned against it");
            entry.record.command_domains = claims;
            entry.summary.command_domains = to_contract(&entry.record.command_domains);
            events.push(PatchEvent::ProjectUpdated { project: entry.summary.clone() });
            Ok::<_, ErrorInfo>(st.projects.values().map(|e| e.record.clone()).collect::<Vec<_>>())
        })?;
        self.inner.deps.store.save_projects(&records).map_err(internal_err)?;

        // 2. The proxy converged on every claim, under the proxy lock.
        {
            let _proxy = self.inner.proxy_lock.lock().await;
            let sites = self.proxy_sites().await?;
            self.proxy_sync(handle, id, &sites).await?;
        }

        let Some(domain) = domain else {
            if let Some(old) = released {
                out(format!(
                    "https://{old} released from {command} — an /etc/hosts line pointing it \
                     at 127.0.0.1 is harmless and may stay"
                ));
            }
            return Ok(());
        };
        let claim = {
            let st = self.inner.state.lock().unwrap();
            st.projects[&project.0]
                .record
                .command_domains
                .iter()
                .find(|c| c.command == command)
                .cloned()
                .ok_or_else(|| internal_err("the claim vanished while it was being made"))?
        };
        match (claim.port, claim.pinned) {
            (Some(p), true) => out(format!(
                "https://{domain} → localhost:{p} is configured on the proxy (pinned)"
            )),
            (Some(p), false) => out(format!(
                "https://{domain} → localhost:{p} is configured on the proxy, and follows \
                 the port {command} announces"
            )),
            (None, _) => {}
        }

        // 3. One request through the proxy, the way a browser would make it.
        let answered = match crate::proxy::probe_domain(&domain).await {
            DomainProbe::Answers(status) => {
                out(format!("https://{domain} reaches {command} (HTTP {status})"));
                true
            }
            DomainProbe::Explained(why) => {
                out(why);
                false
            }
            DomainProbe::HostBlocked => {
                out(format!(
                    "{command} refuses the host name {domain} — add it to server.allowedHosts \
                     in its Vite config (Vite blocks names it was not told about)"
                ));
                false
            }
            DomainProbe::UpstreamDown => {
                let p = claim.port.unwrap_or_default();
                if !crate::proxy::host_port_listening(p).await {
                    out(format!(
                        "nothing answers on localhost:{p} yet — start {command} and \
                         https://{domain} reaches it"
                    ));
                } else if self.proxy_network().await == ProxyNetwork::Bridge {
                    out(format!(
                        "{command} listens on localhost:{p}, but the proxy reaches this \
                         machine through Docker's gateway, not its loopback — have it \
                         listen on every interface (Vite: server.host: true, or --host)"
                    ));
                } else {
                    out(format!(
                        "{command} listens on localhost:{p}, but the proxy cannot reach it — \
                         enable the domain again to recreate the proxy"
                    ));
                }
                false
            }
            DomainProbe::ProxySilent => {
                out("the proxy did not answer on port 443 — the container's output above says why"
                    .into());
                false
            }
        };
        for finding in cookie_findings_for(self, project) {
            out(finding);
        }
        self.report_domain_readiness(handle, id, project, &domain, answered).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dev_servers_announce_their_port() {
        for (line, port) in [
            // Vite: the port is bold when colours are on.
            ("  \u{1b}[32m➜\u{1b}[39m  \u{1b}[1mLocal\u{1b}[22m:   \u{1b}[36mhttp://localhost:\u{1b}[1m5173\u{1b}[22m/\u{1b}[39m", 5173),
            ("  ➜  Local:   http://suite.thinksolar.local:5174/", 5174),
            ("   - Local:        http://localhost:3000", 3000),
            ("  ➜ Local:    http://[::1]:3001/", 3001),
            ("┃ Local    http://localhost:4321/", 4321),
            ("<i> [webpack-dev-server] Loopback: http://localhost:8080/, http://[::1]:8080/", 8080),
            ("   INFO  Server running on [http://127.0.0.1:8000].", 8000),
            ("Serving HTTP on :: port 8000 (http://[::]:8000/) ...", 8000),
            ("* Listening on http://127.0.0.1:3000", 3000),
            ("Starting development server at http://127.0.0.1:8000/", 8000),
            ("Web Server is available at http://localhost:1313/ (bind address 127.0.0.1)", 1313),
            ("ready - started server on 0.0.0.0:3000, url: http://localhost:3000", 3000),
        ] {
            assert_eq!(announced_port(line), Some(port), "{line:?}");
        }
    }

    #[test]
    fn other_addresses_are_not_announcements() {
        for line in [
            // Vite's LAN line — the Local line already said it.
            "  ➜  Network: http://192.168.1.20:5173/",
            "  ➜  Network: use --host to expose",
            // No port of its own.
            "  ➜  Local:   https://suite.shop.test/",
            // An app talking to something else.
            "GET http://localhost:9000/bucket/object 200",
            "[vite] http proxy error: /api/user",
            "Laravel Reverb at ws://0.0.0.0:8080",
            "  VITE v7.1.3  ready in 412 ms",
        ] {
            assert_eq!(announced_port(line), None, "{line:?}");
        }
    }

    #[test]
    fn escapes_are_stripped_including_hyperlinks() {
        assert_eq!(strip_ansi("\u{1b}[1;32mok\u{1b}[0m"), "ok");
        assert_eq!(
            strip_ansi("\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{1b}\\ done"),
            "link done"
        );
    }

    #[test]
    fn sites_are_the_last_two_labels() {
        assert_eq!(site_of("api.thinksolar.test"), "thinksolar.test");
        assert_eq!(site_of("suite.thinksolar.test"), "thinksolar.test");
        assert_eq!(site_of("api-thinksolar.test"), "api-thinksolar.test");
        assert_eq!(site_of("a.b.c.localhost"), "c.localhost");
        assert_eq!(site_of("test"), "test");
    }

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn cookie_findings_name_what_keeps_a_session_from_crossing() {
        // The field case: the API moved to a flat .test name while the
        // session stayed scoped to the old .local parent.
        let findings = cookie_findings(
            &env(&[("SESSION_DOMAIN", ".thinksolar.local")]),
            true,
            "api-thinksolar.test",
            &["suite.thinksolar.test"],
        );
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].contains("different sites"), "{findings:?}");

        // Siblings, but the .env still points at the old names.
        let findings = cookie_findings(
            &env(&[
                ("SESSION_DOMAIN", ".thinksolar.local"),
                ("SANCTUM_STATEFUL_DOMAINS", "suite.thinksolar.local:5173,http://0.0.0.0:5173"),
            ]),
            true,
            "api.thinksolar.test",
            &["suite.thinksolar.test"],
        );
        assert_eq!(findings.len(), 3, "{findings:?}");
        assert!(findings.iter().all(|f| !f.contains("different sites")));
        assert!(findings[0].contains("SESSION_DOMAIN=.thinksolar.test"), "{findings:?}");
        assert!(findings[2].contains("add suite.thinksolar.test"), "{findings:?}");

        // Everything lined up: nothing to say.
        let findings = cookie_findings(
            &env(&[
                ("SESSION_DOMAIN", ".thinksolar.test"),
                ("SANCTUM_STATEFUL_DOMAINS", "https://suite.thinksolar.test,localhost:5173"),
            ]),
            true,
            "api.thinksolar.test",
            &["suite.thinksolar.test"],
        );
        assert!(findings.is_empty(), "{findings:?}");

        // No Sanctum: the .env keys are not this app's business.
        let findings = cookie_findings(&env(&[]), false, "api.shop.test", &["web.shop.test"]);
        assert!(findings.is_empty(), "{findings:?}");
        let unset = cookie_findings(&env(&[]), true, "api.shop.test", &["web.shop.test"]);
        assert!(unset[0].contains("SESSION_DOMAIN is not set"), "{unset:?}");
    }

    fn command(name: &str, line: &str) -> mast_project::ProjectCommandRecord {
        mast_project::ProjectCommandRecord {
            name: name.into(),
            command: line.into(),
            auto_start: false,
            cwd: None,
            after: None,
            ready_when: None,
            auto_restart: false,
            restart_when_changed: Vec::new(),
        }
    }

    fn claim(command: &str, domain: &str, port: Option<u16>, pinned: bool) -> CommandDomainRecord {
        CommandDomainRecord { command: command.into(), domain: domain.into(), port, pinned }
    }

    /// A project as a reconcile leaves it: `api` runs a Sail app and a
    /// frontend dev server beside it.
    fn entry(
        id: &str,
        path: &std::path::Path,
        app: Option<&str>,
        commands: Vec<mast_project::ProjectCommandRecord>,
        claims: Vec<CommandDomainRecord>,
    ) -> ProjectEntry {
        let record = mast_project::ProjectRecord {
            id: id.into(),
            path: path.to_path_buf(),
            display_name: id.into(),
            is_sail: true,
            commands,
            local_domain: app.map(String::from),
            rank: 0,
            command_domains: claims,
        };
        ProjectEntry {
            summary: crate::initial_summary(&record),
            record,
            invocation: None,
            model: None,
            redactor: crate::Redactor::default(),
            app_port: None,
            host_ports: Vec::new(),
            compose_fingerprint: None,
            manifest: crate::manifest::Manifest::default(),
        }
    }

    fn projects(entries: Vec<ProjectEntry>) -> BTreeMap<String, ProjectEntry> {
        entries.into_iter().map(|e| (e.record.id.clone(), e)).collect()
    }

    fn api(claims: Vec<CommandDomainRecord>) -> ProjectEntry {
        entry(
            "api",
            std::path::Path::new("/code/api"),
            Some("api.shop.test"),
            vec![
                command("dev", "sail artisan dev"),
                command("dev frontend", "vp dev"),
                command("docs", "vp docs:dev"),
            ],
            claims,
        )
    }

    #[test]
    fn a_host_command_claims_a_domain_and_follows_what_it_announced() {
        let mut api = api(Vec::new());
        api.summary.command_ports =
            vec![CommandPort { command: "dev frontend".into(), port: 5174 }];
        let all = projects(vec![api]);
        let claims =
            plan_command_claim(&all, "api", "dev frontend", Some("web.shop.test"), None).unwrap();
        assert_eq!(claims, vec![claim("dev frontend", "web.shop.test", Some(5174), false)]);

        // Pinned: the user's port wins over the announcement.
        let claims =
            plan_command_claim(&all, "api", "dev frontend", Some("web.shop.test"), Some(3000))
                .unwrap();
        assert_eq!(claims, vec![claim("dev frontend", "web.shop.test", Some(3000), true)]);
    }

    #[test]
    fn unpinning_keeps_the_last_announced_port_and_releasing_drops_the_claim() {
        let all = projects(vec![api(vec![
            claim("docs", "docs.shop.test", Some(5180), false),
            claim("dev frontend", "web.shop.test", Some(3000), true),
        ])]);
        // Not running, so nothing announced: a pinned port is not a
        // following one, and is dropped with the pin.
        let claims =
            plan_command_claim(&all, "api", "dev frontend", Some("web.shop.test"), None).unwrap();
        assert_eq!(claims[1], claim("dev frontend", "web.shop.test", None, false));
        // A rename of the domain keeps the claim where it stood.
        let claims =
            plan_command_claim(&all, "api", "docs", Some("guide.shop.test"), None).unwrap();
        assert_eq!(claims[0], claim("docs", "guide.shop.test", Some(5180), false));
        let claims = plan_command_claim(&all, "api", "dev frontend", None, None).unwrap();
        assert_eq!(claims, vec![claim("docs", "docs.shop.test", Some(5180), false)]);
    }

    #[test]
    fn claims_are_refused_for_what_cannot_serve_them() {
        let other = entry(
            "billing",
            std::path::Path::new("/code/billing"),
            Some("billing.test"),
            vec![command("dev", "vp dev")],
            vec![claim("dev", "pay.shop.test", None, false)],
        );
        let all = projects(vec![api(Vec::new()), other]);
        let refused = |command: &str, domain: &str, port| {
            match plan_command_claim(&all, "api", command, Some(domain), port) {
                Err(ErrorInfo::InvalidInput { message }) => message,
                Err(ErrorInfo::NotFound { what }) => what,
                other => panic!("{command} → {domain} was accepted: {other:?}"),
            }
        };
        assert!(refused("dev", "web.shop.test", None).contains("app container"));
        assert!(refused("nope", "web.shop.test", None).contains("\"nope\""));
        assert!(refused("dev frontend", "web.shop.dev", None).contains(".test"));
        assert!(refused("dev frontend", "web.shop.test", Some(0)).contains("port 0"));
        // Taken by this project's app, by another project's app, and by
        // another project's command.
        assert!(refused("dev frontend", "api.shop.test", None).contains("api's app"));
        assert!(refused("dev frontend", "billing.test", None).contains("billing's app"));
        assert!(refused("dev frontend", "pay.shop.test", None).contains("billing's command dev"));
    }

    /// A renamed command leaves its claim behind under the old name; the
    /// claim keeps the domain until someone takes it back for the project.
    #[test]
    fn an_orphaned_claim_moves_to_the_command_that_claims_its_domain() {
        let all = projects(vec![api(vec![claim("frontend", "web.shop.test", Some(5173), false)])]);
        let claims =
            plan_command_claim(&all, "api", "dev frontend", Some("web.shop.test"), None).unwrap();
        assert_eq!(claims, vec![claim("dev frontend", "web.shop.test", Some(5173), false)]);

        // Another project cannot take it, orphaned or not.
        let mut billing = api(Vec::new());
        billing.record.id = "billing".into();
        let all = projects(vec![
            api(vec![claim("frontend", "web.shop.test", Some(5173), false)]),
            billing,
        ]);
        assert!(plan_command_claim(&all, "billing", "dev frontend", Some("web.shop.test"), None)
            .is_err());
        // Nor can an app.
        assert_eq!(
            domain_holder(&all, "web.shop.test", &Claimant::App("api".into())).as_deref(),
            Some("api's command frontend")
        );
    }

    #[test]
    fn the_proxy_serves_each_claim_or_says_why_it_cannot() {
        let api = api(vec![
            claim("dev frontend", "web.shop.test", Some(5173), false),
            claim("docs", "docs.shop.test", None, false),
            claim("frontend", "old.shop.test", Some(5173), false),
            claim("dev", "dev.shop.test", Some(80), false),
        ]);
        let sites: Vec<_> =
            api.record.command_domains.iter().map(|c| command_site(&api, c)).collect();
        assert_eq!(sites[0].target, SiteTarget::Port(5173));
        let explained = |i: usize| match &sites[i].target {
            SiteTarget::Explain(why) => why.clone(),
            other => panic!("{other:?}"),
        };
        assert!(explained(1).contains("has not said which port"), "{}", explained(1));
        assert!(explained(2).contains("no longer one of api's commands"), "{}", explained(2));
        assert!(explained(3).contains("app container"), "{}", explained(3));
    }

    fn owning_engine(dir: &std::path::Path) -> Engine {
        Engine::new(
            crate::EngineConfig::default(),
            crate::EngineDeps {
                connector: Arc::new(crate::RealConnector),
                store: mast_project::MetadataStore::open(dir.join("meta")).unwrap(),
                process_env: Default::default(),
                runner: Arc::new(crate::RealLifecycleRunner),
                ownership: crate::acquire_ownership(Some(dir.join("lock"))),
            },
        )
    }

    /// The announced port moves an unpinned claim, is persisted, and
    /// reaches the Caddyfile of a proxy that exists; a pinned claim stays.
    #[tokio::test]
    async fn an_announced_port_moves_the_claim_and_the_proxy_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let engine = owning_engine(dir.path());
        let project = ProjectId("api".into());
        let entry = api(vec![
            claim("dev frontend", "web.shop.test", Some(5173), false),
            claim("docs", "docs.shop.test", Some(6006), true),
        ]);
        engine.inner.deps.store.save_projects(std::slice::from_ref(&entry.record)).unwrap();
        engine.inner.state.lock().unwrap().projects.insert("api".into(), entry);
        let caddyfile = engine.inner.deps.store.proxy_dir().join("Caddyfile");
        std::fs::create_dir_all(caddyfile.parent().unwrap()).unwrap();
        let sites = engine.proxy_sites().await.unwrap();
        std::fs::write(&caddyfile, crate::proxy::render_caddyfile(&sites)).unwrap();

        engine.note_command_port(&project, "dev frontend", 5174);
        engine.note_command_port(&project, "docs", 6007);
        let summary = engine.inner.state.lock().unwrap().projects["api"].summary.clone();
        assert_eq!(
            summary.command_ports,
            vec![
                CommandPort { command: "dev frontend".into(), port: 5174 },
                CommandPort { command: "docs".into(), port: 6007 },
            ]
        );
        assert_eq!(summary.command_domains[0].port, Some(5174));
        assert_eq!(summary.command_domains[1].port, Some(6006), "pinned stays put");
        let saved = engine.inner.deps.store.load_projects().unwrap();
        assert_eq!(saved[0].command_domains[0].port, Some(5174));

        // The convergence runs in the background; wait for its write.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let body = std::fs::read_to_string(&caddyfile).unwrap();
            if body.contains("web.shop.test {\n\treverse_proxy host.docker.internal:5174") {
                assert!(body.contains("docs.shop.test {\n\treverse_proxy host.docker.internal:6006"));
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the proxy never followed: {body}");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        // The run ends: its announcement goes with it, the claim's port stays.
        let handle = engine.new_operation().1;
        drop(AnnounceGuard::new(&engine, &handle, &project, "dev frontend"));
        let summary = engine.inner.state.lock().unwrap().projects["api"].summary.clone();
        assert_eq!(summary.command_ports, vec![CommandPort { command: "docs".into(), port: 6007 }]);
        assert_eq!(summary.command_domains[0].port, Some(5174));
    }
}
