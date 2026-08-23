//! Error signatures: the recurring failure shapes of `sail up` / builds /
//! compose, matched against streamed operation output so a failed operation
//! can end with a plain-language explanation instead of a wall of scrollback.
//!
//! Every entry corresponds to a documented wave of laravel/sail issues.
//! Needles are exact substrings from real tool output — prefer missing an
//! exotic variant over ever mislabeling a failure.

pub struct ErrorSignature {
    pub id: &'static str,
    /// Any of these substrings in an output line marks the signature.
    pub needles: &'static [&'static str],
    /// What actually went wrong, in the user's terms.
    pub explanation: &'static str,
    /// What to do about it.
    pub advice: &'static str,
}

pub const SIGNATURES: &[ErrorSignature] = &[
    ErrorSignature {
        id: "gpg-keyserver",
        needles: &["gpg: keyserver receive failed", "gpg: no valid OpenPGP data found"],
        explanation: "a GPG keyserver or PHP-PPA outage upstream — the classic cause of \
                      fresh `sail build` failures, worldwide and temporary",
        advice: "this is not your project's fault; retry later, and if it keeps failing \
                 rebuild without cache so a poisoned layer is not reused",
    },
    ErrorSignature {
        id: "apt-breakage",
        needles: &["Hash Sum mismatch", "is not signed", "does not have a Release file"],
        explanation: "an upstream apt repository is broken or mid-publish (Ubuntu/PPA/\
                      nodesource churn hits every fresh Sail build at once)",
        advice: "retry later; if it persists, rebuild without cache",
    },
    ErrorSignature {
        id: "image-missing",
        needles: &["manifest unknown", "pull access denied", "repository does not exist"],
        explanation: "the image tag does not exist on the registry (a typo, a removed tag, \
                      or an image that has not been published yet)",
        advice: "check the image name and tag; pick a tag the registry actually offers",
    },
    ErrorSignature {
        id: "build-segfault",
        needles: &["Segmentation fault"],
        explanation: "a crash inside the image build — historically a Docker/QEMU \
                      regression or a broken upstream package, not project code",
        advice: "update Docker, then rebuild without cache; if it persists, check the \
                 base image's issue tracker",
    },
    ErrorSignature {
        id: "mysql-root-user",
        needles: &["MYSQL_USER=\"root\"", "MYSQL_USER=root"],
        explanation: "the MySQL image refuses MYSQL_USER=root — DB_USERNAME=root cannot \
                      be used to initialize the container",
        advice: "set DB_USERNAME to a non-root name in .env (root exists anyway)",
    },
    ErrorSignature {
        id: "port-taken",
        needles: &["address already in use", "port is already allocated", "Ports are not available"],
        explanation: "another program on this machine already holds a host port this \
                      project publishes",
        advice: "run Diagnostics — Mast can move this project's ports; if the holder is \
                 not a Mast project, stop it or change the port in .env",
    },
    ErrorSignature {
        id: "disk-full",
        needles: &["no space left on device"],
        explanation: "Docker's data disk is full",
        advice: "reclaim space (`docker system prune`, old images/volumes) — Diagnostics \
                 shows how much is free",
    },
    ErrorSignature {
        id: "external-network-missing",
        needles: &["declared as external, but could not be found"],
        explanation: "the compose file marks a network `external`, and that network does \
                      not exist on the daemon",
        advice: "run Diagnostics — creating the missing network is a one-click repair",
    },
    ErrorSignature {
        id: "dependency-unhealthy",
        needles: &["dependency failed to start", "is unhealthy"],
        explanation: "a service this one depends on started but never became healthy",
        advice: "open that service's logs (its last words are kept in Captures if it \
                 died) — the root cause is there, not here",
    },
    ErrorSignature {
        id: "compose-dotted-name",
        needles: &["only \"[a-zA-Z0-9_-]+\" are allowed", "invalid name; only [a-zA-Z0-9_-]+"],
        explanation: "this compose/Bake version rejects dotted service names like Sail's \
                      `laravel.test`",
        advice: "set COMPOSE_BAKE=false, upgrade compose (2.37.1+), or rename the service",
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
    },
    ErrorSignature {
        id: "env-var-missing",
        needles: &["required variable", "is missing a value"],
        explanation: "the compose file requires an environment variable that neither \
                      .env nor the environment provides",
        advice: "set the named variable in .env",
    },
];

/// The first signature a line matches, if any.
pub fn classify_line(line: &str) -> Option<&'static ErrorSignature> {
    SIGNATURES
        .iter()
        .find(|sig| sig.needles.iter().any(|needle| line.contains(needle)))
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
            ("write /var/lib/docker/tmp: no space left on device", "disk-full"),
            ("network mast-shared declared as external, but could not be found", "external-network-missing"),
            ("dependency failed to start: container app-mysql-1 is unhealthy", "dependency-unhealthy"),
            ("invalid name; only [a-zA-Z0-9_-]+ are allowed", "compose-dotted-name"),
            ("[InnoDB] Unsupported redo log format (v6)", "db-version-mismatch"),
            ("FATAL:  database files are incompatible with server", "db-version-mismatch"),
            ("Cannot connect to the Docker daemon at unix:///var/run/docker.sock", "docker-daemon-down"),
            ("error while interpolating services.app.image: required variable MISSING is missing a value", "env-var-missing"),
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

    #[test]
    fn signature_ids_are_unique() {
        let mut ids: Vec<_> = SIGNATURES.iter().map(|s| s.id).collect();
        ids.sort_unstable();
        let len = ids.len();
        ids.dedup();
        assert_eq!(len, ids.len());
    }
}
