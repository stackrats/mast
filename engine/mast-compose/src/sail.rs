//! Sail app-service build shapes: `build.context` pointing into
//! `vendor/laravel/sail/runtimes/<series>` (or a published `./docker/<series>`)
//! paired with an `image: sail-<series>/app` tag. Changing PHP versions means
//! changing BOTH — users routinely change one, rebuild nothing, and chase a
//! phantom PHP for an afternoon (laravel/sail#442 and friends).

use saphyr::{LoadableYamlNode, Scalar, Yaml};

use crate::network::mapping_get;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SailBuild {
    pub service: String,
    /// `build` as a string, or `build.context` from the mapping form.
    pub context: Option<String>,
    pub image: Option<String>,
}

fn scalar_string(node: &Yaml) -> Option<String> {
    match node {
        Yaml::Value(Scalar::String(s)) => Some(s.to_string()),
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
            Some(SailBuild { service, context, image })
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

#[cfg(test)]
mod tests {
    use super::*;

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
