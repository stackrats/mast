//! Error signatures: the recurring failure shapes of `sail up` / builds /
//! compose, matched against streamed operation output so a failed operation
//! can end with a plain-language explanation instead of a wall of scrollback.
//!
//! Every entry corresponds to a documented wave of laravel/sail issues.
//! Needles are exact substrings from real tool output — prefer missing an
//! exotic variant over ever mislabeling a failure.

pub struct ErrorSignature {
    pub id: &'static str,
    /// Any of these substrings in an output line marks the signature. A
    /// needle may contain one `*`, matching any run of characters between
    /// its halves — for the tool output that embeds a varying number in the
    /// middle of an otherwise exact sentence ("Port 5173 is already in use").
    pub needles: &'static [&'static str],
    /// What actually went wrong, in the user's terms.
    pub explanation: &'static str,
    /// What to do about it.
    pub advice: &'static str,
    /// A repair id (see [`crate::repair_spec`]) that addresses this failure
    /// directly — powers the Fix button on a failed operation. `None` when
    /// the remedy is not a one-click change.
    pub repair: Option<&'static str>,
}

/// The repair argument this signature's matched line carries, when the
/// repair id needs one (e.g. the missing network's name).
pub fn extract_repair_arg(sig: &ErrorSignature, line: &str) -> Option<String> {
    match sig.id {
        // "network mast-shared declared as external, but could not be found"
        "external-network-missing" => {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let at = tokens.iter().position(|t| *t == "network")?;
            let name = tokens.get(at + 1)?.trim_matches(['"', '\'', '`']);
            (!name.is_empty()).then(|| name.to_string())
        }
        // "container 9765fe… is not connected to the network thinksolar_api_net"
        // — the network name follows the LAST "network" token. When the line
        // also names the container, pack it as a second word: the repair can
        // then dispose of a container that still EXISTS but compose cannot
        // disconnect (a leftover from the same project name in another
        // directory — outside Mast's own orphan view by design).
        "stale-network-endpoint" => {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            let at = tokens.iter().rposition(|t| *t == "network")?;
            let name = tokens.get(at + 1)?.trim_matches(['"', '\'', '`', '.', ':']);
            if name.is_empty() {
                return None;
            }
            let container = tokens
                .iter()
                .position(|t| *t == "container")
                .and_then(|at| tokens.get(at + 1))
                .map(|t| t.trim_matches(['"', '\'', '`', '.', ':']))
                .filter(|t| t.len() >= 12 && t.chars().all(|c| c.is_ascii_hexdigit()));
            Some(match container {
                Some(container) => format!("{name} {container}"),
                None => name.to_string(),
            })
        }
        _ => None,
    }
}

pub const SIGNATURES: &[ErrorSignature] = &[
    ErrorSignature {
        id: "gpg-keyserver",
        needles: &["gpg: keyserver receive failed", "gpg: no valid OpenPGP data found"],
        explanation: "a GPG keyserver or PHP-PPA outage upstream — the classic cause of \
                      fresh `sail build` failures, worldwide and temporary",
        advice: "this is not your project's fault; retry later, and if it keeps failing \
                 rebuild without cache so a poisoned layer is not reused",
        repair: None,
    },
    ErrorSignature {
        id: "apt-breakage",
        needles: &["Hash Sum mismatch", "is not signed", "does not have a Release file"],
        explanation: "an upstream apt repository is broken or mid-publish (Ubuntu/PPA/\
                      nodesource churn hits every fresh Sail build at once)",
        advice: "retry later; if it persists, rebuild without cache",
        repair: None,
    },
    ErrorSignature {
        id: "image-missing",
        needles: &["manifest unknown", "pull access denied", "repository does not exist"],
        explanation: "the image tag does not exist on the registry (a typo, a removed tag, \
                      or an image that has not been published yet)",
        advice: "check the image name and tag; pick a tag the registry actually offers",
        repair: None,
    },
    ErrorSignature {
        id: "build-segfault",
        needles: &["Segmentation fault"],
        explanation: "a crash inside the image build — historically a Docker/QEMU \
                      regression or a broken upstream package, not project code",
        advice: "update Docker, then rebuild without cache; if it persists, check the \
                 base image's issue tracker",
        repair: None,
    },
    ErrorSignature {
        id: "mysql-root-user",
        needles: &["MYSQL_USER=\"root\"", "MYSQL_USER=root"],
        explanation: "the MySQL image refuses MYSQL_USER=root — DB_USERNAME=root cannot \
                      be used to initialize the container",
        advice: "set DB_USERNAME to a non-root name in .env (root exists anyway)",
        repair: None,
    },
    // Must precede "port-taken": a Node `listen EADDRINUSE: address already
    // in use` line contains that signature's needle too, and the remedies
    // differ — this conflict is inside the container, where moving the
    // project's HOST ports cannot reach.
    ErrorSignature {
        id: "dev-port-held",
        needles: &["Port * is already in use", "EADDRINUSE"],
        explanation: "the port this dev server needs is already held in its own network \
                      namespace — for a containerized dev command, almost always by a \
                      previous copy of the same command still running: cancelling it only \
                      stops the host-side client, and the processes it started inside the \
                      container live on",
        advice: "stop the leftover dev-server processes, then start the command once — a \
                 stacked second copy also duplicates every worker pane it manages",
        repair: Some(crate::REPAIR_STOP_STALE_DEV),
    },
    ErrorSignature {
        id: "port-taken",
        needles: &["address already in use", "port is already allocated", "Ports are not available"],
        explanation: "another program on this machine already holds a host port this \
                      project publishes",
        advice: "run Diagnostics — Mast can move this project's ports; if the holder is \
                 not a Mast project, stop it or change the port in .env",
        repair: Some(crate::REPAIR_REASSIGN_PORTS),
    },
    ErrorSignature {
        id: "disk-full",
        needles: &["no space left on device"],
        explanation: "Docker's data disk is full",
        advice: "reclaim space (`docker system prune`, old images/volumes) — Diagnostics \
                 shows how much is free",
        repair: None,
    },
    ErrorSignature {
        id: "external-network-missing",
        needles: &["declared as external, but could not be found"],
        explanation: "the compose file marks a network `external`, and that network does \
                      not exist on the daemon",
        advice: "run Diagnostics — creating the missing network is a one-click repair",
        repair: Some(crate::REPAIR_CREATE_NETWORK),
    },
    ErrorSignature {
        id: "vendor-missing",
        needles: &["vendor/autoload.php"],
        explanation: "the PHP dependencies have never been installed in this checkout — \
                      vendor/ is missing, so nothing Laravel can start",
        advice: "run composer install — Mast can do it in a container, no local PHP needed",
        repair: Some(crate::REPAIR_COMPOSER_INSTALL),
    },
    ErrorSignature {
        id: "node-modules-missing",
        needles: &["multiplex: not found", "concurrently: not found", "vite: not found"],
        explanation: "the frontend dependencies are not installed (node_modules is \
                      missing), so the dev script's runner cannot be found",
        advice: "install them inside the app container, so native modules are built \
                 against the runtime that loads them",
        repair: Some(crate::REPAIR_NODE_INSTALL),
    },
    ErrorSignature {
        id: "stale-network-endpoint",
        needles: &["is not connected to the network"],
        explanation: "the project network still holds an endpoint record for a container \
                      that no longer exists (the residue of a force-removed container), \
                      so compose cannot remove or re-sync the network — every down/up \
                      that touches it fails",
        advice: "clearing the stale endpoint record is a one-click repair; the network \
                 itself is recreated by the next start",
        repair: Some(crate::REPAIR_DISCONNECT_STALE),
    },
    ErrorSignature {
        id: "stale-container-network",
        needles: &["failed to set up container networking: network"],
        explanation: "the mirror image of a stale endpoint: this container was created \
                      against a project network that has since been removed and recreated, \
                      so it still points at a network id the daemon no longer has — every \
                      start of it fails on the lookup",
        // The daemon names the dead network by id, never the service, so
        // there is nothing to hand `recreate-service` — offering a Fix here
        // would only fail asking for a service name.
        advice: "recreate the container rather than restarting it — a restart reuses the \
                 same stale record; a whole-project rebuild, or removing that container \
                 and starting again, re-attaches it to the live network",
        repair: None,
    },
    ErrorSignature {
        id: "node-modules-foreign-store",
        needles: &[
            "ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY",
            "Aborted removal of modules directory due to no TTY",
        ],
        explanation: "node_modules was installed from the other side of the bind mount — \
                      the store it links into belongs to the host, and the container \
                      cannot see it (or the reverse). The package manager will not reuse \
                      a tree whose links it cannot follow: it wants to replace the whole \
                      directory, stops to ask, and finds no terminal to ask at. What \
                      breaks in practice is Vite, so the site keeps serving and only hot \
                      reload goes",
        advice: "the installed files work on both sides — only pnpm's pre-run store \
                 check fails, so switching that check off is a one-click repair; the \
                 side that installs stays responsible for `pnpm install` after \
                 dependency changes",
        repair: Some(crate::REPAIR_PNPM_VERIFY_OFF),
    },
    ErrorSignature {
        id: "dependency-unhealthy",
        needles: &["dependency failed to start", "is unhealthy"],
        explanation: "a service this one depends on started but never became healthy",
        advice: "open that service's logs (its last words are kept in Captures if it \
                 died) — the root cause is there, not here",
        repair: None,
    },
    ErrorSignature {
        id: "compose-dotted-name",
        needles: &["only \"[a-zA-Z0-9_-]+\" are allowed", "invalid name; only [a-zA-Z0-9_-]+"],
        explanation: "this compose/Bake version rejects dotted service names like Sail's \
                      `laravel.test`",
        advice: "set COMPOSE_BAKE=false, upgrade compose (2.37.1+), or rename the service",
        repair: None,
    },
    ErrorSignature {
        id: "db-version-mismatch",
        needles: &[
            "Unsupported redo log format",
            "database files are incompatible with server",
            "Cannot boot server version",
        ],
        explanation: "the database's data volume was written by a different major version \
                      than the image now running",
        advice: "run Diagnostics — the db-volume-version check explains the safe way out",
        repair: None,
    },
    ErrorSignature {
        id: "docker-daemon-down",
        needles: &[
            "Cannot connect to the Docker daemon",
            "Docker or Podman is not running",
            "Docker is not running",
        ],
        explanation: "the Docker daemon is unreachable (stopped, or your user cannot use \
                      its socket)",
        advice: "run Diagnostics — it tells a stopped daemon apart from a permissions \
                 problem",
        repair: None,
    },
    ErrorSignature {
        id: "host-dns-split-brain",
        needles: &[
            "php_network_getaddresses: getaddrinfo",
            "could not translate host name",
        ],
        explanation: "a process on the HOST is using container hostnames (DB_HOST=mysql \
                      only resolves inside the compose network) — typically artisan run \
                      outside sail, an IDE plugin, or host cron",
        advice: "run it through sail (`sail artisan …`), or point host-side tools at \
                 127.0.0.1 with the FORWARD_*_PORT ports",
        repair: None,
    },
    ErrorSignature {
        id: "env-var-missing",
        needles: &["required variable", "is missing a value"],
        explanation: "the compose file requires an environment variable that neither \
                      .env nor the environment provides",
        advice: "set the named variable in .env",
        repair: None,
    },
];

/// The first signature a line matches, if any.
pub fn classify_line(line: &str) -> Option<&'static ErrorSignature> {
    SIGNATURES
        .iter()
        .find(|sig| sig.needles.iter().any(|needle| needle_matches(line, needle)))
}

/// Plain substring containment, except that one `*` in the needle matches
/// any run of characters between its halves. Every occurrence of the prefix
/// is tried, so text before the real match cannot hide it.
fn needle_matches(line: &str, needle: &str) -> bool {
    match needle.split_once('*') {
        None => line.contains(needle),
        Some((prefix, suffix)) => line
            .match_indices(prefix)
            .any(|(at, _)| line[at + prefix.len()..].contains(suffix)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_world_lines_classify_to_the_right_signature() {
        let cases = [
            ("gpg: keyserver receive failed: No name", "gpg-keyserver"),
            ("W: GPG error: ... The repository ... is not signed.", "apt-breakage"),
            ("E: Failed to fetch ... Hash Sum mismatch", "apt-breakage"),
            ("manifest unknown: manifest unknown", "image-missing"),
            ("Segmentation fault (core dumped)", "build-segfault"),
            ("[ERROR] [Entrypoint]: MYSQL_USER=\"root\", MYSQL_PASSWORD cannot be used", "mysql-root-user"),
            ("Error starting userland proxy: listen tcp4 0.0.0.0:80: bind: address already in use", "port-taken"),
            ("Bind for 0.0.0.0:3306 failed: port is already allocated", "port-taken"),
            // Vite with laravel-vite-plugin's strictPort, verbatim from a
            // crash-looping `vp dev` whose port a leftover stack still held.
            ("Error: Port 5178 is already in use", "dev-port-held"),
            // The Node shape of the same conflict: EADDRINUSE outranks
            // port-taken's "address already in use", because this bind
            // failed inside the container, not at the docker port publish.
            ("Error: listen EADDRINUSE: address already in use :::5178", "dev-port-held"),
            ("write /var/lib/docker/tmp: no space left on device", "disk-full"),
            ("network mast-shared declared as external, but could not be found", "external-network-missing"),
            ("Error response from daemon: failed to set up container networking: network 8b642960cbae7f54c2fb658dfb37a600da5364f2f2fa45038538c38c7cd0fed3 not found", "stale-container-network"),
            ("[ERR_PNPM_ABORTED_REMOVE_MODULES_DIR_NO_TTY] Aborted removal of modules directory due to no TTY", "node-modules-foreign-store"),
            ("dependency failed to start: container app-mysql-1 is unhealthy", "dependency-unhealthy"),
            ("invalid name; only [a-zA-Z0-9_-]+ are allowed", "compose-dotted-name"),
            ("[InnoDB] Unsupported redo log format (v6)", "db-version-mismatch"),
            ("FATAL:  database files are incompatible with server", "db-version-mismatch"),
            ("Cannot connect to the Docker daemon at unix:///var/run/docker.sock", "docker-daemon-down"),
            ("error while interpolating services.app.image: required variable MISSING is missing a value", "env-var-missing"),
            ("SQLSTATE[HY000] [2002] php_network_getaddresses: getaddrinfo for mysql failed", "host-dns-split-brain"),
            ("could not translate host name \"pgsql\" to address", "host-dns-split-brain"),
        ];
        for (line, expected) in cases {
            let sig = classify_line(line).unwrap_or_else(|| panic!("unclassified: {line}"));
            assert_eq!(sig.id, expected, "line: {line}");
        }
    }

    #[test]
    fn benign_output_stays_unclassified() {
        for line in [
            "Pulling mysql (mysql:8.4)...",
            " ✔ Container app-redis-1  Started",
            "Layer already exists",
            "npm warn deprecated glob@7",
            "Compiled successfully in 1.2s",
            "[+] Running 3/3",
        ] {
            assert!(classify_line(line).is_none(), "false positive: {line}");
        }
    }

    /// The wildcard needle is why the daemon's container-NAME conflict — a
    /// real failure with a different remedy — does not read as a port
    /// conflict: "is already in use" alone would have matched it.
    #[test]
    fn a_container_name_conflict_is_not_a_dev_port_conflict() {
        let line = "Error response from daemon: Conflict. The container name \"/mysql\" \
                    is already in use by container \"3fae2a...\". You have to remove (or \
                    rename) that container to be able to reuse that name.";
        assert!(classify_line(line).is_none(), "mislabeled: {line}");
        // And the halves only match in order — a suffix seen before the
        // prefix is not the sentence the needle spells.
        assert!(!needle_matches("is already in use — Port", "Port * is already in use"));
        assert!(needle_matches(
            "Ports ok; retrying. Error: Port 5178 is already in use",
            "Port * is already in use"
        ));
    }

    #[test]
    fn repair_args_extract_from_the_matched_line() {
        let net = classify_line("network mast-shared declared as external, but could not be found")
            .unwrap();
        assert_eq!(net.repair, Some(crate::REPAIR_CREATE_NETWORK));
        assert_eq!(
            extract_repair_arg(net, "network mast-shared declared as external, but could not be found")
                .as_deref(),
            Some("mast-shared")
        );
        // Quoted variants (newer compose wording) strip cleanly.
        assert_eq!(
            extract_repair_arg(net, "network \"acme-net\" declared as external, but could not be found")
                .as_deref(),
            Some("acme-net")
        );

        let port = classify_line("bind: address already in use").unwrap();
        assert_eq!(port.repair, Some(crate::REPAIR_REASSIGN_PORTS));
        assert_eq!(extract_repair_arg(port, "bind: address already in use"), None);

        // The in-container twin needs no arg: the repair's target is the
        // project's app service, not anything named on the line.
        let dev = classify_line("Error: Port 5178 is already in use").unwrap();
        assert_eq!(dev.repair, Some(crate::REPAIR_STOP_STALE_DEV));
        assert_eq!(extract_repair_arg(dev, "Error: Port 5178 is already in use"), None);

        // The fresh-project stalls map to their containerized installs.
        let vendor = classify_line(
            "Failed to open stream: No such file or directory in /var/www/html/vendor/autoload.php",
        )
        .unwrap();
        assert_eq!(vendor.repair, Some(crate::REPAIR_COMPOSER_INSTALL));
        let node = classify_line("sh: 1: multiplex: not found").unwrap();
        assert_eq!(node.repair, Some(crate::REPAIR_NODE_INSTALL));
        assert_eq!(classify_line("sh: 1: vite: not found").unwrap().id, node.id);

        // The exact daemon wording seen live: the network name follows the
        // LAST "network" token; the container id is packed as a second word
        // so the repair can dispose of a leftover that still exists.
        let line = "Error response from daemon: container 9765fe4e8e62cd621b90d7dbbc92a16d \
                    is not connected to the network thinksolar_api_net";
        let stale = classify_line(line).unwrap();
        assert_eq!(stale.repair, Some(crate::REPAIR_DISCONNECT_STALE));
        assert_eq!(
            extract_repair_arg(stale, line).as_deref(),
            Some("thinksolar_api_net 9765fe4e8e62cd621b90d7dbbc92a16d")
        );
        // Without a container id on the line, the network stands alone.
        let bare = "network thinksolar_api_net has an endpoint that is not connected \
                    to the network thinksolar_api_net";
        assert_eq!(extract_repair_arg(stale, bare).as_deref(), Some("thinksolar_api_net"));
    }

    #[test]
    fn signature_ids_are_unique() {
        let mut ids: Vec<_> = SIGNATURES.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(len, ids.len());
    }
}
