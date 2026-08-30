//! Sail app-service build shapes: `build.context` pointing into
//! `vendor/laravel/sail/runtimes/<series>` (or a published `./docker/<series>`)
//! paired with an `image: sail-<series>/app` tag. Changing PHP versions means
//! changing BOTH — users routinely change one, rebuild nothing, and chase a
//! phantom PHP for an afternoon (laravel/sail#442 and friends).

use mast_yaml_edit::{Edit, key};
use saphyr::{LoadableYamlNode, Scalar, Yaml};

use crate::network::mapping_get;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SailBuild {
    pub service: String,
    /// `build` as a string, or `build.context` from the mapping form.
    pub context: Option<String>,
    pub image: Option<String>,
    /// `build.args.NODE_VERSION`, when the file overrides the runtime
    /// Dockerfile's default.
    pub node_arg: Option<String>,
    /// `build.args.PHP_EXTENSIONS` — the space-separated extension list
    /// Sail's runtimes install on top of the base set (laravel/sail#879).
    pub php_extensions_arg: Option<String>,
    /// `build` uses the mapping form — the only shape that can carry args.
    pub build_is_mapping: bool,
}

fn scalar_string(node: &Yaml) -> Option<String> {
    match node {
        Yaml::Value(Scalar::String(s)) => Some(s.to_string()),
        Yaml::Value(Scalar::Integer(i)) => Some(i.to_string()),
        _ => None,
    }
}

/// Every service in one compose file's source that declares a `build`,
/// with its context and image. Source-level on purpose: the resolved model
/// absolutizes paths and hides which file spells the context.
pub fn sail_builds(source: &str) -> Vec<SailBuild> {
    let Ok(docs) = Yaml::load_from_str(source) else { return Vec::new() };
    let Some(root) = docs.first() else { return Vec::new() };
    let Some(Yaml::Mapping(services)) = mapping_get(root, "services") else {
        return Vec::new();
    };
    services
        .iter()
        .filter_map(|(key, def)| {
            let service = match key {
                Yaml::Value(Scalar::String(s)) => s.to_string(),
                _ => return None,
            };
            let build = mapping_get(def, "build")?;
            let context = scalar_string(build)
                .or_else(|| mapping_get(build, "context").and_then(scalar_string));
            let image = mapping_get(def, "image").and_then(scalar_string);
            let node_arg = mapping_get(build, "args")
                .and_then(|args| mapping_get(args, "NODE_VERSION"))
                .and_then(scalar_string);
            let php_extensions_arg = mapping_get(build, "args")
                .and_then(|args| mapping_get(args, "PHP_EXTENSIONS"))
                .and_then(scalar_string);
            let build_is_mapping = matches!(build, Yaml::Mapping(_));
            Some(SailBuild {
                service,
                context,
                image,
                node_arg,
                php_extensions_arg,
                build_is_mapping,
            })
        })
        .collect()
}

fn series_like(text: &str) -> bool {
    let mut parts = text.split('.');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(major), Some(minor), None)
            if !major.is_empty()
                && !minor.is_empty()
                && major.chars().all(|c| c.is_ascii_digit())
                && minor.chars().all(|c| c.is_ascii_digit())
    )
}

/// The PHP series a Sail-shaped build context pins:
/// `./vendor/laravel/sail/runtimes/8.4` or a published `./docker/8.4`.
pub fn runtime_series(context: &str) -> Option<String> {
    let tail = context.trim_end_matches('/').rsplit('/').next()?;
    let parent_is_sail = context.contains("vendor/laravel/sail/runtimes")
        || context.trim_end_matches('/').trim_end_matches(tail).ends_with("docker/");
    (parent_is_sail && series_like(tail)).then(|| tail.to_string())
}

/// The PHP series a Sail image tag pins: `sail-8.4/app`.
pub fn image_series(image: &str) -> Option<String> {
    let rest = image.strip_prefix("sail-")?;
    let (series, tail) = rest.split_once('/')?;
    (tail == "app" && series_like(series)).then(|| series.to_string())
}

/// Plan pinning `build.args.NODE_VERSION` — Sail's documented way to change
/// the Node the runtime image installs (the Dockerfile's ARG default rules
/// otherwise). Handles every stub shape: an existing NODE_VERSION is
/// updated, an existing `args` mapping gains the key, a mapping build
/// without `args` gains the block. The rare string-form `build:` is refused
/// rather than restructured.
pub fn plan_set_node_version(
    source: &str,
    service: &str,
    major: &str,
) -> Result<(Vec<Edit>, Vec<String>), String> {
    if major.is_empty() || !major.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("\"{major}\" is not a Node major like 22"));
    }
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let svc = mapping_get(services, service)
        .ok_or_else(|| format!("service \"{service}\" is not in this file"))?;
    let build = mapping_get(svc, "build")
        .ok_or_else(|| format!("service \"{service}\" has no build: to pin Node in"))?;
    if !matches!(build, Yaml::Mapping(_)) {
        return Err(format!(
            "service \"{service}\" uses the short `build: <path>` form — convert it to the \
             mapping form (context/args) to pin a Node version"
        ));
    }
    let value = format!("'{major}'");
    let summary = vec![format!("{service}: build arg NODE_VERSION -> {major}")];
    let edit = match mapping_get(build, "args") {
        Some(args) if mapping_get(args, "NODE_VERSION").is_some() => Edit::SetScalar {
            path: vec![
                key("services"),
                key(service),
                key("build"),
                key("args"),
                key("NODE_VERSION"),
            ],
            value,
        },
        Some(_) => Edit::InsertMapKey {
            path: vec![key("services"), key(service), key("build"), key("args")],
            key: "NODE_VERSION".to_string(),
            value,
        },
        None => Edit::InsertMapBlock {
            path: vec![key("services"), key(service), key("build")],
            key: "args".to_string(),
            lines: vec![format!("NODE_VERSION: {value}")],
        },
    };
    Ok((vec![edit], summary))
}

/// Whether a vendored runtime Dockerfile understands `PHP_EXTENSIONS`.
///
/// Sail only grew the arg in June 2026 (laravel/sail#879). Docker does not
/// fail on a build arg the Dockerfile never declares — it warns and carries
/// on — so a project whose runtimes were published before that would take
/// the edit, rebuild for several minutes and install nothing. Read the
/// Dockerfile and refuse instead.
pub fn runtime_supports_php_extensions(dockerfile: &str) -> bool {
    dockerfile.lines().any(|line| {
        let line = line.trim_start();
        line.strip_prefix("ARG ").is_some_and(|rest| {
            let name = rest.trim_start();
            name.starts_with("PHP_EXTENSIONS")
                && name["PHP_EXTENSIONS".len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == '=' || c.is_whitespace())
        })
    })
}

/// One extension name, as it appears between `php8.4-` and nothing else.
///
/// The runtime expands the list *unquoted* (`for ext in $PHP_EXTENSIONS`),
/// so every name reaches a shell as a bare word. Anything outside the apt
/// package-name alphabet is refused rather than escaped: a space would
/// silently split into two packages, and the rest is how a value like
/// `redis; rm -rf /` would become a command.
fn valid_extension(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '+' | '-'))
}

/// Plan pinning `build.args.PHP_EXTENSIONS` — the list Sail's runtime
/// installs as `php<series>-<name>` on top of its base set. Same stub
/// shapes as [`plan_set_node_version`]; an empty list clears the arg to
/// `''`, which the Dockerfile's `if [ -n ... ]` reads as "install nothing".
pub fn plan_set_php_extensions(
    source: &str,
    service: &str,
    extensions: &[String],
) -> Result<(Vec<Edit>, Vec<String>), String> {
    let mut wanted: Vec<String> = Vec::new();
    for ext in extensions {
        let ext = ext.trim();
        if !valid_extension(ext) {
            return Err(format!(
                "\"{ext}\" is not a PHP extension package name (lowercase letters, digits, \
                 and . + - only)"
            ));
        }
        if !wanted.iter().any(|e| e == ext) {
            wanted.push(ext.to_string());
        }
    }
    let joined = wanted.join(" ");
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let svc = mapping_get(services, service)
        .ok_or_else(|| format!("service \"{service}\" is not in this file"))?;
    let build = mapping_get(svc, "build")
        .ok_or_else(|| format!("service \"{service}\" has no build: to install extensions in"))?;
    if !matches!(build, Yaml::Mapping(_)) {
        return Err(format!(
            "service \"{service}\" uses the short `build: <path>` form — convert it to the \
             mapping form (context/args) to install extensions"
        ));
    }
    let value = format!("'{joined}'");
    let summary = vec![if wanted.is_empty() {
        format!("{service}: build arg PHP_EXTENSIONS cleared")
    } else {
        format!("{service}: build arg PHP_EXTENSIONS -> {joined}")
    }];
    let edit = match mapping_get(build, "args") {
        Some(args) if mapping_get(args, "PHP_EXTENSIONS").is_some() => Edit::SetScalar {
            path: vec![
                key("services"),
                key(service),
                key("build"),
                key("args"),
                key("PHP_EXTENSIONS"),
            ],
            value,
        },
        Some(_) => Edit::InsertMapKey {
            path: vec![key("services"), key(service), key("build"), key("args")],
            key: "PHP_EXTENSIONS".to_string(),
            value,
        },
        None => Edit::InsertMapBlock {
            path: vec![key("services"), key(service), key("build")],
            key: "args".to_string(),
            lines: vec![format!("PHP_EXTENSIONS: {value}")],
        },
    };
    Ok((vec![edit], summary))
}

/// Plan adding the `host.docker.internal:host-gateway` mapping to a service
/// — what Sail's current stub ships and older published files lack, and
/// without which Xdebug's default `client_host` resolves to nothing on
/// Linux. Refuses when the service already has `extra_hosts` (merging into
/// an existing list is the user's call, not a blind append).
pub fn plan_add_host_gateway(
    source: &str,
    service: &str,
) -> Result<(Vec<Edit>, Vec<String>), String> {
    let docs = Yaml::load_from_str(source).map_err(|e| e.to_string())?;
    let root = docs.first().ok_or("empty compose file")?;
    let services = mapping_get(root, "services").ok_or("no services mapping in this file")?;
    let svc = mapping_get(services, service)
        .ok_or_else(|| format!("service \"{service}\" is not in this file"))?;
    if mapping_get(svc, "extra_hosts").is_some() {
        return Err(format!(
            "service \"{service}\" already has extra_hosts — add \
             'host.docker.internal:host-gateway' to that list yourself"
        ));
    }
    Ok((
        vec![Edit::InsertMapBlock {
            path: vec![key("services"), key(service)],
            key: "extra_hosts".to_string(),
            lines: vec!["- 'host.docker.internal:host-gateway'".to_string()],
        }],
        vec![
            format!("add to service {service}:"),
            "  extra_hosts:".into(),
            "    - 'host.docker.internal:host-gateway'".into(),
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn php_extensions_plan_covers_every_stub_shape() {
        let stock = "services:\n  laravel.test:\n    build:\n      context: './vendor/laravel/sail/runtimes/8.4'\n      args:\n        WWWGROUP: '${WWWGROUP}'\n    image: 'sail-8.4/app'\n";
        let (edits, summary) =
            plan_set_php_extensions(stock, "laravel.test", &["redis".into(), "intl".into()])
                .unwrap();
        let out = mast_yaml_edit::apply_all(stock, &edits).unwrap();
        assert!(out.contains("PHP_EXTENSIONS: 'redis intl'"), "{out}");
        assert!(out.contains("WWWGROUP: '${WWWGROUP}'"), "existing args survive: {out}");
        assert!(Yaml::load_from_str(&out).is_ok());
        assert!(summary[0].contains("PHP_EXTENSIONS -> redis intl"));
        assert_eq!(
            sail_builds(&out)[0].php_extensions_arg.as_deref(),
            Some("redis intl"),
            "the parser reads back what the plan wrote"
        );

        // Re-setting replaces the whole list rather than appending to it.
        let redone = mast_yaml_edit::apply_all(
            &out,
            &plan_set_php_extensions(&out, "laravel.test", &["imagick".into()]).unwrap().0,
        )
        .unwrap();
        assert!(redone.contains("PHP_EXTENSIONS: 'imagick'"), "{redone}");
        assert!(!redone.contains("redis"), "{redone}");

        // No args mapping: the block is created.
        let bare = "services:\n  laravel.test:\n    build:\n      context: './docker/8.4'\n";
        let out = mast_yaml_edit::apply_all(
            bare,
            &plan_set_php_extensions(bare, "laravel.test", &["gd".into()]).unwrap().0,
        )
        .unwrap();
        assert!(out.contains("args:") && out.contains("PHP_EXTENSIONS: 'gd'"), "{out}");
        assert!(Yaml::load_from_str(&out).is_ok());

        // An empty list clears the arg — the Dockerfile reads '' as "none".
        let cleared = mast_yaml_edit::apply_all(
            &out,
            &plan_set_php_extensions(&out, "laravel.test", &[]).unwrap().0,
        )
        .unwrap();
        assert!(cleared.contains("PHP_EXTENSIONS: ''"), "{cleared}");

        // The short build form cannot carry args and is refused, not rewritten.
        let short = "services:\n  laravel.test:\n    build: './docker/8.4'\n";
        assert!(plan_set_php_extensions(short, "laravel.test", &["redis".into()]).is_err());
    }

    /// The runtime expands this list unquoted, so a name that is not a bare
    /// apt package suffix must never reach the file.
    #[test]
    fn php_extension_names_outside_the_package_alphabet_are_refused() {
        let stock = "services:\n  laravel.test:\n    build:\n      context: './docker/8.4'\n      args:\n        WWWGROUP: '${WWWGROUP}'\n";
        for bad in [
            "redis; rm -rf /",
            "redis intl",
            "$(whoami)",
            "`id`",
            "redis\nintl",
            "Redis",
            "-leading",
            "",
            "redis'",
        ] {
            assert!(
                plan_set_php_extensions(stock, "laravel.test", &[bad.to_string()]).is_err(),
                "{bad:?} must be refused"
            );
        }
        // The ones that genuinely are package names still pass.
        for good in ["redis", "intl", "gd", "imagick", "pgsql", "xdebug", "amqp", "ds"] {
            assert!(
                plan_set_php_extensions(stock, "laravel.test", &[good.to_string()]).is_ok(),
                "{good} must be accepted"
            );
        }
    }

    #[test]
    fn extension_support_is_read_off_the_runtime_dockerfile() {
        assert!(runtime_supports_php_extensions(
            "FROM ubuntu:24.04\nARG NODE_VERSION=24\nARG PHP_EXTENSIONS=\"\"\nWORKDIR /var/www\n"
        ));
        assert!(runtime_supports_php_extensions("ARG PHP_EXTENSIONS\n"));
        // Pre-#879 runtimes: the arg is simply not there.
        assert!(!runtime_supports_php_extensions(
            "FROM ubuntu:24.04\nARG NODE_VERSION=24\nARG POSTGRES_VERSION=18\n"
        ));
        // A near-miss name must not read as support.
        assert!(!runtime_supports_php_extensions("ARG PHP_EXTENSIONS_EXTRA=1\n"));
    }

    #[test]
    fn node_version_plan_covers_every_stub_shape() {
        // The stock stub: args exists (WWWGROUP), NODE_VERSION does not.
        let stock = "services:\n  laravel.test:\n    build:\n      context: './vendor/laravel/sail/runtimes/8.4'\n      dockerfile: Dockerfile\n      args:\n        WWWGROUP: '${WWWGROUP}'\n    image: 'sail-8.4/app'\n";
        let (edits, summary) = plan_set_node_version(stock, "laravel.test", "20").unwrap();
        let out = mast_yaml_edit::apply_all(stock, &edits).unwrap();
        assert!(out.contains("NODE_VERSION: '20'"), "{out}");
        assert!(out.contains("WWWGROUP: '${WWWGROUP}'"), "existing args survive: {out}");
        assert!(Yaml::load_from_str(&out).is_ok());
        assert!(summary[0].contains("NODE_VERSION -> 20"));
        assert_eq!(
            sail_builds(&out)[0].node_arg.as_deref(),
            Some("20"),
            "the parser reads back what the plan wrote"
        );

        // Already pinned: the value updates in place.
        let repinned = mast_yaml_edit::apply_all(
            &out,
            &plan_set_node_version(&out, "laravel.test", "22").unwrap().0,
        )
        .unwrap();
        assert!(repinned.contains("NODE_VERSION: '22'"), "{repinned}");
        assert!(!repinned.contains("NODE_VERSION: '20'"), "{repinned}");

        // No args mapping at all: the block is created.
        let bare = "services:\n  laravel.test:\n    build:\n      context: './docker/8.4'\n";
        let out = mast_yaml_edit::apply_all(
            bare,
            &plan_set_node_version(bare, "laravel.test", "24").unwrap().0,
        )
        .unwrap();
        assert!(out.contains("args:"), "{out}");
        assert!(out.contains("NODE_VERSION: '24'"), "{out}");
        assert!(Yaml::load_from_str(&out).is_ok());

        // Short-form build and garbage majors are refused.
        let short = "services:\n  laravel.test:\n    build: ./docker/8.4\n";
        assert!(plan_set_node_version(short, "laravel.test", "20").unwrap_err().contains("short"));
        assert!(plan_set_node_version(stock, "laravel.test", "v20").is_err());
        assert!(plan_set_node_version(stock, "nope", "20").unwrap_err().contains("not in this file"));
    }

    #[test]
    fn host_gateway_plan_inserts_and_refuses_existing() {
        let source = "services:\n  laravel.test:\n    image: 'sail-8.4/app'\n    ports:\n      - '80:80'\n  redis:\n    image: 'redis:alpine'\n";
        let (edits, summary) = plan_add_host_gateway(source, "laravel.test").unwrap();
        let out = mast_yaml_edit::apply_all(source, &edits).unwrap();
        assert!(
            out.contains(
                "    extra_hosts:\n      - 'host.docker.internal:host-gateway'"
            ),
            "{out}"
        );
        assert!(Yaml::load_from_str(&out).is_ok());
        assert!(summary.iter().any(|s| s.contains("host-gateway")));

        let already = out;
        let err = plan_add_host_gateway(&already, "laravel.test").unwrap_err();
        assert!(err.contains("already has extra_hosts"), "{err}");
        assert!(plan_add_host_gateway(&already, "nope").unwrap_err().contains("not in this file"));
    }

    #[test]
    fn builds_parse_both_forms_and_skip_imageless_pulls() {
        let source = "services:\n  laravel.test:\n    build:\n      context: './vendor/laravel/sail/runtimes/8.4'\n      dockerfile: Dockerfile\n    image: 'sail-8.4/app'\n  worker:\n    build: ./docker/8.3\n  redis:\n    image: 'redis:alpine'\n";
        let builds = sail_builds(source);
        assert_eq!(builds.len(), 2, "{builds:?}");
        assert_eq!(builds[0].service, "laravel.test");
        assert_eq!(builds[0].context.as_deref(), Some("./vendor/laravel/sail/runtimes/8.4"));
        assert_eq!(builds[0].image.as_deref(), Some("sail-8.4/app"));
        assert_eq!(builds[1].service, "worker");
        assert_eq!(builds[1].context.as_deref(), Some("./docker/8.3"));
        assert_eq!(builds[1].image, None);
    }

    #[test]
    fn series_extraction_is_strict() {
        assert_eq!(runtime_series("./vendor/laravel/sail/runtimes/8.4").as_deref(), Some("8.4"));
        assert_eq!(runtime_series("./docker/8.3").as_deref(), Some("8.3"));
        assert_eq!(runtime_series("./docker/8.3/").as_deref(), Some("8.3"));
        assert_eq!(runtime_series("./docker/php"), None);
        assert_eq!(runtime_series("./frontend"), None, "not a sail shape");
        assert_eq!(runtime_series("./vendor/laravel/sail/runtimes/8"), None);

        assert_eq!(image_series("sail-8.4/app").as_deref(), Some("8.4"));
        assert_eq!(image_series("sail-8.10/app").as_deref(), Some("8.10"));
        assert_eq!(image_series("sail-8.4/worker"), None);
        assert_eq!(image_series("mysql:8.4"), None);
    }
}
