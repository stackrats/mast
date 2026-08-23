//! Service catalog (plan M7): the standard Sail companion services as
//! previewed, transactional compose edits. Definitions mirror laravel/sail's
//! stubs. Removal is three-way: base (absent) → ours (what add rendered) →
//! current; if the current block no longer matches what we would have added,
//! we refuse instead of destroying user customizations.

use mast_yaml_edit::{Edit, key};
use saphyr::{LoadableYamlNode, Yaml};

use crate::network::mapping_get;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CatalogDef {
    pub id: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    /// Compose service key this installs (same as `id` today).
    pub service_key: &'static str,
    /// Image names (sans tag/registry) that identify this software however
    /// the user named the service — installed-state must not assume our key.
    pub image_stems: &'static [&'static str],
    /// Functional role ("cache", "database", "object-storage", …): entries
    /// sharing a role usually conflict (ports, env) if both are added.
    pub role: &'static str,
    /// Service block body, indentation relative to the block level.
    pub lines: &'static [&'static str],
    /// Top-level named volumes the service needs (each rendered as
    /// `name: {driver: local}`).
    pub volumes: &'static [&'static str],
    /// `.env` updates applied alongside the add (listed in the preview).
    pub env_sets: &'static [(&'static str, &'static str)],
    /// Images earlier Mast releases installed for this entry. Three-way
    /// removal accepts these as "ours" too, so a service added before an
    /// image bump stays cleanly removable.
    pub legacy_images: &'static [&'static str],
}

pub const CATALOG: &[CatalogDef] = &[
    CatalogDef {
        id: "redis",
        title: "Redis",
        description: "In-memory cache / queue / session store.",
        service_key: "redis",
        image_stems: &["redis"],
        role: "cache",
        lines: &[
            "image: 'redis:alpine'",
            "ports:",
            "  - '${FORWARD_REDIS_PORT:-6379}:6379'",
            "volumes:",
            "  - 'sail-redis:/data'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - redis-cli",
            "    - ping",
            "  retries: 3",
            "  timeout: 5s",
        ],
        volumes: &["sail-redis"],
        env_sets: &[("REDIS_HOST", "redis")],
        legacy_images: &[],
    },
    CatalogDef {
        id: "mailpit",
        title: "Mailpit",
        description: "Local mail catcher with a web UI on port 8025.",
        service_key: "mailpit",
        image_stems: &["axllent/mailpit"],
        role: "mail",
        lines: &[
            "image: 'axllent/mailpit:latest'",
            "ports:",
            "  - '${FORWARD_MAILPIT_PORT:-1025}:1025'",
            "  - '${FORWARD_MAILPIT_DASHBOARD_PORT:-8025}:8025'",
        ],
        volumes: &[],
        env_sets: &[
            ("MAIL_MAILER", "smtp"),
            ("MAIL_HOST", "mailpit"),
            ("MAIL_PORT", "1025"),
        ],
        legacy_images: &[],
    },
    CatalogDef {
        id: "meilisearch",
        title: "Meilisearch",
        description: "Full-text search engine (Laravel Scout driver).",
        service_key: "meilisearch",
        image_stems: &["getmeili/meilisearch"],
        role: "search",
        lines: &[
            "image: 'getmeili/meilisearch:latest'",
            "ports:",
            "  - '${FORWARD_MEILISEARCH_PORT:-7700}:7700'",
            "environment:",
            "  MEILI_NO_ANALYTICS: '${MEILISEARCH_NO_ANALYTICS:-false}'",
            "volumes:",
            "  - 'sail-meilisearch:/meili_data'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - wget",
            "    - '--no-verbose'",
            "    - '--spider'",
            "    - 'http://127.0.0.1:7700/health'",
            "  retries: 3",
            "  timeout: 5s",
        ],
        volumes: &["sail-meilisearch"],
        env_sets: &[
            ("SCOUT_DRIVER", "meilisearch"),
            ("MEILISEARCH_HOST", "http://meilisearch:7700"),
        ],
        legacy_images: &[],
    },
    CatalogDef {
        id: "minio",
        title: "MinIO",
        description: "S3-compatible object storage (console on port 8900).",
        service_key: "minio",
        image_stems: &["minio/minio"],
        role: "object-storage",
        lines: &[
            "image: 'minio/minio:latest'",
            "ports:",
            "  - '${FORWARD_MINIO_PORT:-9000}:9000'",
            "  - '${FORWARD_MINIO_CONSOLE_PORT:-8900}:8900'",
            "environment:",
            "  MINIO_ROOT_USER: sail",
            "  MINIO_ROOT_PASSWORD: password",
            "volumes:",
            "  - 'sail-minio:/data'",
            "command: 'minio server /data --console-address \":8900\"'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - mc",
            "    - ready",
            "    - local",
            "  retries: 3",
            "  timeout: 5s",
        ],
        volumes: &["sail-minio"],
        env_sets: &[
            ("FILESYSTEM_DISK", "s3"),
            ("AWS_ACCESS_KEY_ID", "sail"),
            ("AWS_SECRET_ACCESS_KEY", "password"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("AWS_BUCKET", "local"),
            ("AWS_ENDPOINT", "http://minio:9000"),
            // The endpoint only resolves inside the compose network; without
            // a browser-reachable AWS_URL, Storage::url() hands the host a
            // dead minio:9000 link (the classic MinIO split-brain).
            ("AWS_URL", "http://localhost:9000/local"),
            ("AWS_USE_PATH_STYLE_ENDPOINT", "true"),
        ],
        legacy_images: &[],
    },
    CatalogDef {
        id: "pgsql",
        title: "PostgreSQL",
        description: "Postgres 17 database (DB_* env pointed at it on add).",
        service_key: "pgsql",
        image_stems: &["postgres"],
        role: "database",
        lines: &[
            "image: 'postgres:17'",
            "ports:",
            "  - '${FORWARD_DB_PORT:-5432}:5432'",
            "environment:",
            "  PGPASSWORD: '${DB_PASSWORD:-secret}'",
            "  POSTGRES_DB: '${DB_DATABASE}'",
            "  POSTGRES_USER: '${DB_USERNAME}'",
            "  POSTGRES_PASSWORD: '${DB_PASSWORD:-secret}'",
            "volumes:",
            "  - 'sail-pgsql:/var/lib/postgresql/data'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - pg_isready",
            "    - '-q'",
            "    - '-d'",
            "    - '${DB_DATABASE}'",
            "    - '-U'",
            "    - '${DB_USERNAME}'",
            "  retries: 3",
            "  timeout: 5s",
        ],
        volumes: &["sail-pgsql"],
        env_sets: &[
            ("DB_CONNECTION", "pgsql"),
            ("DB_HOST", "pgsql"),
            ("DB_PORT", "5432"),
        ],
        legacy_images: &[],
    },
    CatalogDef {
        id: "mariadb",
        title: "MariaDB",
        description: "MariaDB 11 database (DB_* env pointed at it on add).",
        service_key: "mariadb",
        image_stems: &["mariadb"],
        role: "database",
        lines: &[
            "image: 'mariadb:11'",
            "ports:",
            "  - '${FORWARD_DB_PORT:-3306}:3306'",
            "environment:",
            "  MYSQL_ROOT_PASSWORD: '${DB_PASSWORD}'",
            "  MYSQL_ROOT_HOST: '%'",
            "  MYSQL_DATABASE: '${DB_DATABASE}'",
            "  MYSQL_USER: '${DB_USERNAME}'",
            "  MYSQL_PASSWORD: '${DB_PASSWORD}'",
            "  MYSQL_ALLOW_EMPTY_PASSWORD: 'yes'",
            "volumes:",
            "  - 'sail-mariadb:/var/lib/mysql'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - healthcheck.sh",
            "    - '--connect'",
            "    - '--innodb_initialized'",
            "  retries: 3",
            "  timeout: 5s",
        ],
        volumes: &["sail-mariadb"],
        env_sets: &[
            ("DB_CONNECTION", "mariadb"),
            ("DB_HOST", "mariadb"),
            ("DB_PORT", "3306"),
        ],
        legacy_images: &[],
    },
    CatalogDef {
        id: "mysql",
        title: "MySQL",
        description: "MySQL 8.4 database (DB_* env pointed at it on add).",
        service_key: "mysql",
        image_stems: &["mysql", "mysql/mysql-server"],
        role: "database",
        lines: &[
            // mysql/mysql-server was abandoned upstream (laravel/sail#829);
            // Sail's own stub moved to mysql:8.4 (laravel/sail#834).
            "image: 'mysql:8.4'",
            "ports:",
            "  - '${FORWARD_DB_PORT:-3306}:3306'",
            "environment:",
            "  MYSQL_ROOT_PASSWORD: '${DB_PASSWORD}'",
            "  MYSQL_ROOT_HOST: '%'",
            "  MYSQL_DATABASE: '${DB_DATABASE}'",
            "  MYSQL_USER: '${DB_USERNAME}'",
            "  MYSQL_PASSWORD: '${DB_PASSWORD}'",
            "  MYSQL_ALLOW_EMPTY_PASSWORD: '1'",
            "volumes:",
            "  - 'sail-mysql:/var/lib/mysql'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - mysqladmin",
            "    - ping",
            "    - '-p${DB_PASSWORD}'",
            "  retries: 3",
            "  timeout: 5s",
        ],
        volumes: &["sail-mysql"],
        env_sets: &[
            ("DB_CONNECTION", "mysql"),
            ("DB_HOST", "mysql"),
            ("DB_PORT", "3306"),
        ],
        legacy_images: &["mysql/mysql-server:8.0"],
    },
    CatalogDef {
        id: "typesense",
        title: "Typesense",
        description: "Typo-tolerant search engine (Laravel Scout driver).",
        service_key: "typesense",
        image_stems: &["typesense/typesense"],
        role: "search",
        lines: &[
            "image: 'typesense/typesense:27.1'",
            "ports:",
            "  - '${FORWARD_TYPESENSE_PORT:-8108}:8108'",
            "environment:",
            "  TYPESENSE_DATA_DIR: '/typesense-data'",
            "  TYPESENSE_API_KEY: '${TYPESENSE_API_KEY:-xyz}'",
            "  TYPESENSE_ENABLE_CORS: '${TYPESENSE_ENABLE_CORS:-true}'",
            "volumes:",
            "  - 'sail-typesense:/typesense-data'",
            "healthcheck:",
            "  test:",
            "    - CMD",
            "    - wget",
            "    - '--no-verbose'",
            "    - '--spider'",
            "    - 'http://localhost:8108/health'",
            "  retries: 5",
            "  timeout: 7s",
        ],
        volumes: &["sail-typesense"],
        env_sets: &[
            ("SCOUT_DRIVER", "typesense"),
            ("TYPESENSE_HOST", "typesense"),
            ("TYPESENSE_PORT", "8108"),
            ("TYPESENSE_PROTOCOL", "http"),
        ],
        legacy_images: &[],
    },
    CatalogDef {
        id: "rustfs",
        title: "RustFS",
        description: "S3-compatible object storage in Rust (console on port 9001).",
        service_key: "rustfs",
        image_stems: &["rustfs/rustfs"],
        role: "object-storage",
        lines: &[
            "image: 'rustfs/rustfs:latest'",
            "ports:",
            "  - '${FORWARD_RUSTFS_PORT:-9000}:9000'",
            "  - '${FORWARD_RUSTFS_CONSOLE_PORT:-9001}:9001'",
            "environment:",
            "  RUSTFS_ACCESS_KEY: sail",
            "  RUSTFS_SECRET_KEY: password",
            "volumes:",
            "  - 'sail-rustfs:/data'",
        ],
        volumes: &["sail-rustfs"],
        env_sets: &[
            ("FILESYSTEM_DISK", "s3"),
            ("AWS_ACCESS_KEY_ID", "sail"),
            ("AWS_SECRET_ACCESS_KEY", "password"),
            ("AWS_DEFAULT_REGION", "us-east-1"),
            ("AWS_BUCKET", "local"),
            ("AWS_ENDPOINT", "http://rustfs:9000"),
            // Same split-brain as minio: signed for the network, served to
            // the browser.
            ("AWS_URL", "http://localhost:9000/local"),
            ("AWS_USE_PATH_STYLE_ENDPOINT", "true"),
        ],
        legacy_images: &[],
    },
];

/// Does `image` run this catalog entry's software?
pub fn def_matches_image(def: &CatalogDef, image: &str) -> bool {
    def.image_stems.iter().any(|stem| image_matches(image, stem))
}

pub fn catalog_def(id: &str) -> Option<&'static CatalogDef> {
    CATALOG.iter().find(|d| d.id == id)
}

/// The image this entry installs, unquoted — `None` for a service built
/// rather than pulled.
pub fn def_image(def: &CatalogDef) -> Option<&'static str> {
    def.lines
        .iter()
        .find_map(|line| line.strip_prefix("image:"))
        .map(|image| image.trim().trim_matches('\'').trim_matches('"'))
}

/// Does `image` run the catalog service's software, whatever the tag or
/// registry? ("redis:alpine", "bitnami/redis:7", "docker.io/axllent/mailpit"
/// all match their stems.)
pub fn image_matches(image: &str, stem: &str) -> bool {
    let name = match image.rsplit_once(':') {
        // Strip the tag, but not a registry port ("host:5000/img").
        Some((name, tag)) if !tag.contains('/') => name,
        _ => image,
    };
    name == stem || name.ends_with(&format!("/{stem}"))
}

#[derive(Debug, Clone)]
pub struct CatalogPlan {
    pub edits: Vec<Edit>,
    pub summary: Vec<String>,
}

/// The service block `add` renders, including the adaptive `networks` entry —
/// also the "ours" side of the three-way removal comparison.
fn service_lines(def: &CatalogDef, sail_network: bool) -> Vec<String> {
    let mut lines: Vec<String> = def.lines.iter().map(|s| s.to_string()).collect();
    if sail_network {
        lines.push("networks:".into());
        lines.push("  - sail".into());
    }
    lines
}

fn has_sail_network(root: &Yaml) -> bool {
    mapping_get(root, "networks").is_some_and(|nets| matches!(nets, Yaml::Mapping(m) if m
        .iter()
        .any(|(k, _)| matches!(k, Yaml::Value(saphyr::Scalar::String(s)) if s == "sail"))))
}

/// Parse a rendered service block into a Yaml subtree (the "ours" document of
/// the three-way comparison).
fn block_subtree(service_key: &str, lines: &[String]) -> Result<Yaml<'static>, String> {
    let mut doc = format!("{service_key}:\n");
    for line in lines {
        doc.push_str("  ");
        doc.push_str(line);
        doc.push('\n');
    }
    let parsed = Yaml::load_from_str(&doc).map_err(|e| e.to_string())?;
    let root = parsed.into_iter().next().ok_or("empty rendered block")?;
    let Yaml::Mapping(m) = root else { return Err("rendered block is not a mapping".into()) };
    m.into_iter().next().map(|(_, v)| v).ok_or_else(|| "rendered block is empty".into())
}

fn expected_subtree(def: &CatalogDef, sail_network: bool) -> Result<Yaml<'static>, String> {
    block_subtree(def.service_key, &service_lines(def, sail_network))
}

/// Does the current block match what add would render — with today's image or
/// one this entry installed in an earlier release?
fn matches_ours(current: &Yaml, def: &CatalogDef, sail_network: bool) -> Result<bool, String> {
    if *current == expected_subtree(def, sail_network)? {
        return Ok(true);
    }
    let lines = service_lines(def, sail_network);
    Ok(def.legacy_images.iter().any(|legacy| {
        let substituted: Vec<String> = lines
            .iter()
            .map(|l| {
                if l.starts_with("image:") { format!("image: '{legacy}'") } else { l.clone() }
            })
            .collect();
        block_subtree(def.service_key, &substituted).is_ok_and(|expected| *current == expected)
    }))
}

pub fn plan_catalog_add(source: &str, def: &CatalogDef) -> Result<CatalogPlan, String> {
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let Yaml::Mapping(_) = services else {
        return Err("services is not a mapping".into());
    };
    if mapping_get(services, def.service_key).is_some() {
        return Err(format!("service \"{}\" already exists in this file", def.service_key));
    }

    let sail_network = has_sail_network(root);
    let mut edits = vec![Edit::InsertMapBlock {
        path: vec![key("services")],
        key: def.service_key.to_string(),
        lines: service_lines(def, sail_network),
    }];
    let mut summary = vec![format!("add service {}", def.service_key)];
    if sail_network {
        summary.push("joins the file's sail network".into());
    }

    push_volume_edits(root, def.volumes, &mut edits, &mut summary)?;

    for (env_key, value) in def.env_sets {
        summary.push(format!("set {env_key}={value} in .env"));
    }
    Ok(CatalogPlan { edits, summary })
}


/// A user-described service (M8.5): the minimal compose shape people
/// actually add by hand — image, host ports, one data volume, a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomService {
    pub name: String,
    pub image: String,
    /// "host:container" pairs.
    pub ports: Vec<String>,
    /// Container path persisted into a named volume `{name}-data`.
    pub volume: Option<String>,
    pub command: Option<String>,
}

fn valid_single_quoted(value: &str) -> bool {
    !value.contains('\'') && !value.contains('\n') && !value.trim().is_empty()
}

/// Plan adding a user-described service: same adaptive sail-network and
/// volumes handling as catalog adds; strict validation because every field
/// lands inside the yaml we write.
pub fn plan_custom_add(source: &str, custom: &CustomService) -> Result<CatalogPlan, String> {
    let name = custom.name.trim();
    let valid_name = !name.is_empty()
        && name.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !valid_name {
        return Err("service name must be lowercase letters, digits, - or _".into());
    }
    let image = custom.image.trim();
    if !valid_single_quoted(image) || image.contains(char::is_whitespace) {
        return Err("image must be a plain reference like redis:7 or ghcr.io/acme/tool:latest".into());
    }
    for port in &custom.ports {
        let ok = port.split_once(':').is_some_and(|(host, container)| {
            host.parse::<u16>().is_ok() && container.parse::<u16>().is_ok()
        });
        if !ok {
            return Err(format!("port \"{port}\" must be host:container (e.g. 8080:80)"));
        }
    }
    if let Some(volume) = &custom.volume
        && (!volume.starts_with('/') || !valid_single_quoted(volume))
    {
        return Err("volume must be an absolute container path (e.g. /data)".into());
    }
    if let Some(command) = &custom.command
        && !valid_single_quoted(command)
    {
        return Err("command must not contain quotes or newlines".into());
    }

    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    if mapping_get(services, name).is_some() {
        return Err(format!("service \"{name}\" already exists in this file"));
    }

    let volume_name = format!("{name}-data");
    let mut lines: Vec<String> = vec![format!("image: '{image}'")];
    if !custom.ports.is_empty() {
        lines.push("ports:".into());
        for port in &custom.ports {
            lines.push(format!("  - '{port}'"));
        }
    }
    if let Some(volume) = &custom.volume {
        lines.push("volumes:".into());
        lines.push(format!("  - '{volume_name}:{volume}'"));
    }
    if let Some(command) = &custom.command {
        lines.push(format!("command: '{}'", command.trim()));
    }
    if has_sail_network(root) {
        lines.push("networks:".into());
        lines.push("  - sail".into());
    }

    let mut edits =
        vec![Edit::InsertMapBlock { path: vec![key("services")], key: name.to_string(), lines }];
    let mut summary = vec![format!("add service {name} ({image})")];
    if custom.volume.is_some() {
        push_volume_edits(root, &[volume_name.as_str()], &mut edits, &mut summary)?;
    }
    summary.push("remove later from the service chip's menu or the catalog (as-is removal)".into());
    Ok(CatalogPlan { edits, summary })
}

/// Remove ANY service by its key, exactly as it stands — for services Mast
/// did not add (no three-way baseline exists). Volumes and env are left
/// untouched; the write transaction's `compose config` gate refuses removals
/// that break the file (e.g. a remaining `depends_on` reference).
pub fn plan_service_remove(source: &str, service: &str) -> Result<CatalogPlan, String> {
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let Yaml::Mapping(map) = services else { return Err("services is not a mapping".into()) };
    if mapping_get(services, service).is_none() {
        return Err(format!("service \"{service}\" is not in this file"));
    }
    if map.len() <= 1 {
        return Err("refusing to remove the last service in the file".into());
    }
    Ok(CatalogPlan {
        edits: vec![Edit::RemoveKey { path: vec![key("services"), key(service)] }],
        summary: vec![
            format!("remove service {service} exactly as it is"),
            "this service was not added by Mast, so no unchanged-since-add check applies".into(),
            "named volumes, networks and .env values are left untouched".into(),
        ],
    })
}


fn push_volume_edits<S: AsRef<str>>(
    root: &Yaml,
    volumes: &[S],
    edits: &mut Vec<Edit>,
    summary: &mut Vec<String>,
) -> Result<(), String> {
    let existing = mapping_get(root, "volumes");
    for volume in volumes {
        let volume = volume.as_ref();
        match existing {
            Some(Yaml::Mapping(m)) => {
                let exists = m.iter().any(|(k, _)| {
                    matches!(k, Yaml::Value(saphyr::Scalar::String(s)) if s == volume)
                });
                if !exists {
                    edits.push(Edit::InsertMapBlock {
                        path: vec![key("volumes")],
                        key: volume.to_string(),
                        lines: vec!["driver: local".into()],
                    });
                    summary.push(format!("declare volume {volume}"));
                }
            }
            Some(_) => return Err("volumes is not a mapping".into()),
            None => {
                edits.push(Edit::InsertMapBlock {
                    path: vec![],
                    key: "volumes".to_string(),
                    lines: vec![format!("{volume}:"), "  driver: local".into()],
                });
                summary.push(format!("declare volumes block with {volume}"));
            }
        }
    }
    Ok(())
}

/// Three-way removal: only remove the service if its current block still
/// matches what add would render (byte-order-insensitive semantic equality).
pub fn plan_catalog_remove(source: &str, def: &CatalogDef) -> Result<CatalogPlan, String> {
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let current = mapping_get(services, def.service_key)
        .ok_or_else(|| format!("service \"{}\" is not in this file", def.service_key))?;

    let sail_network = has_sail_network(root);
    if !matches_ours(current, def, sail_network)? {
        return Err(format!(
            "service \"{}\" has been customized since it was added — refusing to remove it \
             automatically; edit the compose file directly",
            def.service_key
        ));
    }

    let mut edits =
        vec![Edit::RemoveKey { path: vec![key("services"), key(def.service_key.to_string().as_str())] }];
    let mut summary = vec![format!("remove service {} (unchanged since add)", def.service_key)];

    if let Some(Yaml::Mapping(volumes)) = mapping_get(root, "volumes") {
        let volume_keys: Vec<String> = volumes
            .iter()
            .filter_map(|(k, _)| match k {
                Yaml::Value(saphyr::Scalar::String(s)) => Some(s.to_string()),
                _ => None,
            })
            .collect();
        let ours: Vec<&str> =
            def.volumes.iter().copied().filter(|v| volume_keys.iter().any(|k| k == v)).collect();
        if !ours.is_empty() {
            if volume_keys.len() == ours.len() {
                // Removing them all would leave an empty mapping; drop the
                // whole volumes block instead.
                edits.push(Edit::RemoveKey { path: vec![key("volumes")] });
                summary.push("remove the volumes block (only ours remained)".into());
            } else {
                for volume in ours {
                    edits.push(Edit::RemoveKey { path: vec![key("volumes"), key(volume)] });
                    summary.push(format!("remove volume {volume}"));
                }
            }
        }
    }
    summary.push("named volumes on disk and .env values are left untouched".into());
    Ok(CatalogPlan { edits, summary })
}

#[cfg(test)]
mod tests {
    use super::*;
    use mast_yaml_edit::apply_all;

    const SAIL_DOC: &str = "services:\n  laravel.test:\n    image: sail-8.4/app\n    networks:\n      - sail\nnetworks:\n  sail:\n    driver: bridge\n";
    const PLAIN_DOC: &str = "services:\n  app:\n    image: alpine\n";

    fn def(id: &str) -> &'static CatalogDef {
        catalog_def(id).unwrap()
    }

    #[test]
    fn add_redis_to_sail_file_joins_sail_network_and_validates() {
        let plan = plan_catalog_add(SAIL_DOC, def("redis")).unwrap();
        let out = apply_all(SAIL_DOC, &plan.edits).unwrap();
        assert!(out.contains("  redis:\n    image: 'redis:alpine'"));
        assert!(out.contains("    networks:\n      - sail\n"));
        assert!(out.contains("volumes:\n  sail-redis:\n    driver: local\n"));
        // Still parses and the original content survives byte-exact regions.
        assert!(out.starts_with("services:\n  laravel.test:\n"));
        assert!(plan.summary.iter().any(|s| s.contains("REDIS_HOST=redis")));
    }

    #[test]
    fn add_to_plain_file_omits_sail_network() {
        let plan = plan_catalog_add(PLAIN_DOC, def("redis")).unwrap();
        let out = apply_all(PLAIN_DOC, &plan.edits).unwrap();
        assert!(!out.contains("networks:"));
        assert!(out.contains("  redis:\n    image: 'redis:alpine'"));
    }

    #[test]
    fn add_refuses_existing_service() {
        let doc = "services:\n  redis:\n    image: redis\n";
        assert!(plan_catalog_add(doc, def("redis")).unwrap_err().contains("already exists"));
    }

    #[test]
    fn add_then_remove_restores_exact_bytes() {
        for id in [
            "redis",
            "mailpit",
            "meilisearch",
            "minio",
            "pgsql",
            "mariadb",
            "mysql",
            "rustfs",
            "typesense",
        ] {
            let plan = plan_catalog_add(SAIL_DOC, def(id)).unwrap();
            let added = apply_all(SAIL_DOC, &plan.edits).unwrap();
            let removal = plan_catalog_remove(&added, def(id)).unwrap();
            let restored = apply_all(&added, &removal.edits).unwrap();
            assert_eq!(restored, SAIL_DOC, "three-way removal must restore bytes for {id}");
        }
    }

    /// A mysql service added by a release that installed
    /// `mysql/mysql-server:8.0` must stay removable after the image bump to
    /// `mysql:8.4` — the legacy image is still "ours", not a customization.
    #[test]
    fn remove_accepts_a_legacy_image_install() {
        let plan = plan_catalog_add(SAIL_DOC, def("mysql")).unwrap();
        let added = apply_all(SAIL_DOC, &plan.edits).unwrap();
        let old_install = added.replace("image: 'mysql:8.4'", "image: 'mysql/mysql-server:8.0'");
        assert_ne!(old_install, added, "the substitution must have applied");
        let removal = plan_catalog_remove(&old_install, def("mysql")).unwrap();
        let restored = apply_all(&old_install, &removal.edits).unwrap();
        assert_eq!(restored, SAIL_DOC);
    }

    /// Object-storage entries must hand the browser a usable URL alongside
    /// the in-network endpoint, or Storage::url() links at minio:9000 die on
    /// the host (the MinIO split-brain).
    #[test]
    fn object_storage_entries_set_a_browser_facing_aws_url() {
        for id in ["minio", "rustfs"] {
            let d = def(id);
            let url = d.env_sets.iter().find(|(k, _)| *k == "AWS_URL");
            assert_eq!(
                url,
                Some(&("AWS_URL", "http://localhost:9000/local")),
                "{id} must set AWS_URL"
            );
            assert!(d.env_sets.iter().any(|(k, _)| *k == "AWS_ENDPOINT"), "{id}");
        }
    }

    #[test]
    fn remove_refuses_customized_service() {
        let plan = plan_catalog_add(SAIL_DOC, def("redis")).unwrap();
        let added = apply_all(SAIL_DOC, &plan.edits).unwrap();
        // The user pins the image afterwards — three-way comparison must
        // refuse to destroy that.
        let drifted = added.replace("image: 'redis:alpine'", "image: 'redis:7.2-alpine'");
        let err = plan_catalog_remove(&drifted, def("redis")).unwrap_err();
        assert!(err.contains("customized"), "{err}");
    }

    #[test]
    fn remove_keeps_shared_volumes_block_when_other_volumes_exist() {
        let doc = "services:\n  app:\n    image: alpine\nvolumes:\n  user-data:\n    driver: local\n";
        let plan = plan_catalog_add(doc, def("redis")).unwrap();
        let added = apply_all(doc, &plan.edits).unwrap();
        let removal = plan_catalog_remove(&added, def("redis")).unwrap();
        let restored = apply_all(&added, &removal.edits).unwrap();
        assert_eq!(restored, doc);
        assert!(restored.contains("user-data"));
    }


    #[test]
    fn custom_service_add_roundtrips_and_validates() {
        let custom = CustomService {
            name: "tools".into(),
            image: "ghcr.io/acme/tools:1.2".into(),
            ports: vec!["8081:80".into()],
            volume: Some("/data".into()),
            command: Some("serve --all".into()),
        };
        let plan = plan_custom_add(SAIL_DOC, &custom).unwrap();
        let out = apply_all(SAIL_DOC, &plan.edits).unwrap();
        assert!(out.contains("  tools:\n    image: 'ghcr.io/acme/tools:1.2'"));
        assert!(out.contains("      - '8081:80'"));
        assert!(out.contains("      - 'tools-data:/data'"));
        assert!(out.contains("    command: 'serve --all'"));
        assert!(out.contains("    networks:\n      - sail"));
        assert!(out.contains("  tools-data:\n    driver: local"));

        // Generic removal restores the file byte-exactly (volume block too).
        let removal = plan_service_remove(&out, "tools").unwrap();
        let removed = apply_all(&out, &removal.edits).unwrap();
        assert!(!removed.contains("tools:"));

        for (broken, needle) in [
            (CustomService { name: "Bad Name".into(), ..custom.clone() }, "lowercase"),
            (CustomService { image: "img with spaces".into(), ..custom.clone() }, "plain reference"),
            (CustomService { ports: vec!["eighty:80".into()], ..custom.clone() }, "host:container"),
            (CustomService { volume: Some("data".into()), ..custom.clone() }, "absolute"),
            (CustomService { command: Some("x' ; rm".into()), ..custom.clone() }, "quotes"),
        ] {
            let err = plan_custom_add(SAIL_DOC, &broken).unwrap_err();
            assert!(err.contains(needle), "{err}");
        }
    }

    #[test]
    fn generic_service_remove_takes_the_block_as_is_and_guards_the_last_service() {
        let doc = "services:\n  app:\n    image: alpine\n  thinksolar-redis:\n    image: 'redis:alpine'\n    ports:\n      - '6380:6379'\n";
        let plan = plan_service_remove(doc, "thinksolar-redis").unwrap();
        let out = apply_all(doc, &plan.edits).unwrap();
        assert_eq!(out, "services:\n  app:\n    image: alpine\n");
        assert!(plan.summary.iter().any(|s| s.contains("not added by Mast")));

        assert!(plan_service_remove(doc, "missing").unwrap_err().contains("not in this file"));
        let only = "services:\n  app:\n    image: alpine\n";
        assert!(plan_service_remove(only, "app").unwrap_err().contains("last service"));
    }

    #[test]
    fn image_matching_ignores_tags_registries_and_service_names() {
        assert!(image_matches("redis:alpine", "redis"));
        assert!(image_matches("bitnami/redis:7.2", "redis"));
        assert!(image_matches("axllent/mailpit:latest", "axllent/mailpit"));
        assert!(image_matches("docker.io/axllent/mailpit", "axllent/mailpit"));
        assert!(image_matches("postgres:17", "postgres"));
        assert!(!image_matches("mariadb:11", "postgres"));
        assert!(!image_matches("redisinsight:latest", "redis"));
    }

    #[test]
    fn every_catalog_entry_renders_parseable_yaml() {
        for def in CATALOG {
            let plan = plan_catalog_add(PLAIN_DOC, def).unwrap();
            let out = apply_all(PLAIN_DOC, &plan.edits).unwrap();
            assert!(Yaml::load_from_str(&out).is_ok(), "{} renders invalid yaml", def.id);
        }
    }
}
