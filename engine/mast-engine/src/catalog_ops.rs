//! Service catalog (M7): previewed transactional compose edits plus the
//! documented `.env` updates; removal is three-way (or as-is for services
//! Mast did not add).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use mast_compose::ComposeInvocation;
use mast_contract::{
    CatalogEntry, ErrorInfo, FileEditPreview, OperationEventKind, OperationId, ProjectId,
};
use mast_project::RegistryTagRecord;

use crate::ops::OpHandle;
use crate::{Engine, env_write_error, internal_err};

/// How long cached registry tags stay usable before a refresh is scheduled.
/// Release lines appear on the order of weeks; a day keeps the list current
/// without making the dialog chatty.
const TAG_TTL_SECS: u64 = 24 * 60 * 60;

/// Tags to offer for `image`: cached registry data when we have it, the static
/// fallback otherwise, always including the tag the service actually runs and
/// the one the catalog installs.
///
/// The two guarantees matter for different reasons. Without the running tag
/// the picker opens on a value absent from its own options and renders blank —
/// which is what happened to a project on `mariadb:11.4` when the hand-written
/// table listed only `12, 11, 10.11`. Without the catalog's own tag,
/// `redis:alpine` disappears the moment real registry data arrives, because
/// `alpine` is not version-shaped.
fn offered_versions(
    image: &str,
    catalog_image: Option<&str>,
    cached: &HashMap<String, RegistryTagRecord>,
) -> Vec<String> {
    let (repo, tag) = mast_compose::versions::split_image(image);
    let from_registry = mast_registry::docker_hub_path(repo)
        .and_then(|key| cached.get(&key))
        .map(|record| record.versions.clone());
    // Only fall back to the static table when the registry has told us
    // nothing yet — an empty *fetched* list is a real answer ("this repo
    // publishes only latest") and must not resurrect stale guesses.
    let base = from_registry.unwrap_or_else(|| {
        mast_compose::versions::versions_for(image).iter().map(|t| (*t).to_string()).collect()
    });
    let catalog_tag = catalog_image
        .filter(|ci| {
            // Only when it is the same repo — the catalog's mysql-server tag
            // is not pullable for Sail's mysql.
            mast_compose::versions::split_image(ci).0 == repo
        })
        .and_then(|ci| mast_compose::versions::split_image(ci).1);
    let must_include: Vec<&str> = tag.into_iter().chain(catalog_tag).collect();
    let offered = mast_registry::offered_versions(&base, &must_include);
    // A lone tag is not a choice; offer no dropdown rather than a dead one.
    if offered.len() < 2 { Vec::new() } else { offered }
}

impl Engine {
    /// Context needed to plan/apply a network attach for one member.
    /// Invocation + the file catalog edits target (the first invocation file,
    /// same choice as network attach).
    pub(crate) fn catalog_context(
        &self,
        project: &ProjectId,
    ) -> Result<(ComposeInvocation, PathBuf), ErrorInfo> {
        let st = self.inner.state.lock().unwrap();
        let entry = st
            .projects
            .get(&project.0)
            .ok_or(ErrorInfo::NotFound { what: format!("project {}", project.0) })?;
        let invocation = entry.invocation.clone().ok_or_else(|| ErrorInfo::InvalidInput {
            message: "project not resolved yet".into(),
        })?;
        let file = invocation
            .files
            .first()
            .map(|f| f.path.clone())
            .ok_or(ErrorInfo::Internal { message: "invocation has no files".into() })?;
        Ok((invocation, file))
    }

    /// The service catalog with per-project installed flags. Installed is
    /// matched by image across the whole resolved model — users name their
    /// services freely (`thinksolar-redis`), and a second redis would be a
    /// bug, not a feature. Removable only when the service key is ours.
    pub async fn catalog(&self, project: &ProjectId) -> Result<Vec<CatalogEntry>, ErrorInfo> {
        let (_invocation, file) = self.catalog_context(project)?;
        let named_images: Vec<(String, String)> = {
            let st = self.inner.state.lock().unwrap();
            st.projects
                .get(&project.0)
                .and_then(|e| e.model.as_ref())
                .map(|m| {
                    m.services
                        .iter()
                        .filter_map(|s| s.image.clone().map(|i| (s.name.clone(), i)))
                        .collect()
                })
                .unwrap_or_default()
        };
        let inner = self.inner.clone();
        let entries: Vec<CatalogEntry> = tokio::task::spawn_blocking(move || {
            let source = std::fs::read_to_string(&file)
                .map_err(internal_err)?;
            // A corrupt or missing cache is not worth failing the dialog over;
            // the static fallback covers it and the refresh below rewrites it.
            let cached = inner.deps.store.load_registry_tags().unwrap_or_default();
            let declared = mast_compose::declared_service_keys(&source);
            Ok(mast_compose::catalog::CATALOG
                .iter()
                .map(|def| {
                    let removable = declared.iter().any(|s| s == def.service_key);
                    let image_matches: Vec<&String> = named_images
                        .iter()
                        .filter(|(_, image)| mast_compose::catalog::def_matches_image(def, image))
                        .map(|(name, _)| name)
                        .collect();
                    let installed = removable || !image_matches.is_empty();
                    // Generic removal edits the first invocation file, so
                    // only offer a target actually declared in it.
                    let installed_service = if removable {
                        Some(def.service_key.to_string())
                    } else {
                        image_matches
                            .iter()
                            .find(|name| declared.iter().any(|d| &d == *name))
                            .map(|name| (*name).clone())
                    };
                    // Same-role coverage: another entry's software is already
                    // running this role (rustfs covers object storage, so
                    // MinIO's Add would conflict).
                    let role_covered_by = if installed {
                        None
                    } else {
                        mast_compose::catalog::CATALOG
                            .iter()
                            .filter(|other| other.id != def.id && other.role == def.role)
                            .find_map(|other| {
                                named_images
                                    .iter()
                                    .find(|(_, i)| {
                                        mast_compose::catalog::def_matches_image(other, i)
                                    })
                                    .map(|(_, i)| i.clone())
                            })
                    };
                    // Read the image from the file rather than the resolved
                    // model: the file is what a retag rewrites, and the model
                    // still shows the old tag until the next reconcile. Falls
                    // back to the model for services declared elsewhere.
                    let installed_image = installed_service.as_ref().and_then(|name| {
                        let path = vec![
                            mast_yaml_edit::key("services"),
                            mast_yaml_edit::key(name),
                            mast_yaml_edit::key("image"),
                        ];
                        mast_yaml_edit::get_scalar(&source, &path)
                            .map(|raw| {
                                raw.trim().trim_matches('\'').trim_matches('"').to_string()
                            })
                            .filter(|image| !image.is_empty())
                            .or_else(|| {
                                named_images
                                    .iter()
                                    .find(|(service, _)| service == name)
                                    .map(|(_, image)| image.clone())
                            })
                    });
                    let versions = installed_image
                        .as_deref()
                        .map(|image| {
                            offered_versions(
                                image,
                                mast_compose::catalog::def_image(def),
                                &cached,
                            )
                        })
                        .unwrap_or_default();
                    CatalogEntry {
                        id: def.id.to_string(),
                        title: def.title.to_string(),
                        description: def.description.to_string(),
                        installed,
                        removable,
                        installed_service,
                        role_covered_by,
                        installed_image,
                        versions,
                    }
                })
                .collect())
        })
        .await
        .map_err(internal_err)??;
        self.refresh_registry_tags(&entries);
        Ok(entries)
    }

    /// Bring the tag cache up to date in the background.
    ///
    /// Deliberately fire-and-forget: [`Engine::catalog`] has already returned,
    /// so a slow or unreachable registry costs the person nothing and a
    /// failure leaves the last good answer in place. The fresher list lands in
    /// the cache and shows up the next time the dialog opens.
    fn refresh_registry_tags(&self, entries: &[CatalogEntry]) {
        if !self.inner.config.registry_refresh {
            return;
        }
        let stale: Vec<(String, String)> = {
            let cached = self.inner.deps.store.load_registry_tags().unwrap_or_default();
            let now = mast_project::now_unix();
            let mut seen: Vec<String> = Vec::new();
            entries
                .iter()
                .filter_map(|entry| entry.installed_image.as_deref())
                .filter_map(|image| {
                    let (repo, _) = mast_compose::versions::split_image(image);
                    mast_registry::docker_hub_path(repo).map(|key| (repo.to_string(), key))
                })
                .filter(|(_, key)| {
                    cached.get(key).is_none_or(|r| r.is_stale(now, TAG_TTL_SECS))
                        && !seen.contains(key)
                        && { seen.push(key.clone()); true }
                })
                .collect()
        };
        if stale.is_empty() {
            return;
        }
        let inner = self.inner.clone();
        tokio::spawn(async move {
            for (repo, key) in stale {
                match mast_registry::fetch_versions(&repo).await {
                    Ok(versions) => {
                        let record = RegistryTagRecord {
                            versions,
                            fetched_unix: mast_project::now_unix(),
                        };
                        if let Err(e) = inner.deps.store.save_registry_tags(&key, record) {
                            tracing::warn!(%repo, error = %e, "could not cache registry tags");
                        }
                    }
                    // Offline is the common case here, not an incident.
                    Err(e) => tracing::debug!(%repo, error = %e, "registry tag refresh failed"),
                }
            }
        });
    }

    pub async fn catalog_preview(
        &self,
        project: &ProjectId,
        service: &str,
        remove: bool,
    ) -> Result<FileEditPreview, ErrorInfo> {
        let (_invocation, file) = self.catalog_context(project)?;
        let def = mast_compose::catalog::catalog_def(service)
            .ok_or_else(|| ErrorInfo::InvalidInput { message: format!("unknown service {service}") })?;
        tokio::task::spawn_blocking(move || {
            let before = std::fs::read_to_string(&file)
                .map_err(internal_err)?;
            let plan = if remove {
                mast_compose::catalog::plan_catalog_remove(&before, def)
            } else {
                mast_compose::catalog::plan_catalog_add(&before, def)
            }
            .map_err(|message| ErrorInfo::InvalidInput { message })?;
            let after = mast_yaml_edit::apply_all(&before, &plan.edits)
                .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
            Ok(FileEditPreview {
                file: file.to_string_lossy().into_owned(),
                before,
                after,
                summary: plan.summary,
                no_op: false,
            })
        })
        .await
        .map_err(internal_err)?
    }

    /// Preview removing ANY service by its compose key (apply via
    /// `Action::RemoveService`).
    pub async fn service_remove_preview(
        &self,
        project: &ProjectId,
        service: &str,
    ) -> Result<FileEditPreview, ErrorInfo> {
        let (_invocation, file) = self.catalog_context(project)?;
        let service = service.to_string();
        tokio::task::spawn_blocking(move || {
            let before = std::fs::read_to_string(&file)
                .map_err(internal_err)?;
            let plan = mast_compose::catalog::plan_service_remove(&before, &service)
                .map_err(|message| ErrorInfo::InvalidInput { message })?;
            let after = mast_yaml_edit::apply_all(&before, &plan.edits)
                .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
            Ok(FileEditPreview {
                file: file.to_string_lossy().into_owned(),
                before,
                after,
                summary: plan.summary,
                no_op: false,
            })
        })
        .await
        .map_err(internal_err)?
    }

    /// Preview retagging a service's image (apply via
    /// `Action::SetServiceImage`).
    pub async fn service_image_preview(
        &self,
        project: &ProjectId,
        service: &str,
        image: &str,
    ) -> Result<FileEditPreview, ErrorInfo> {
        let (_invocation, file) = self.catalog_context(project)?;
        // Version guard: retagging a database against its existing volume is
        // how Sail's own stub bump walked users into crash-loops.
        let guard = self.retag_version_verdict(project, service, image).await;
        let (service, image) = (service.to_string(), image.to_string());
        tokio::task::spawn_blocking(move || {
            let before = std::fs::read_to_string(&file).map_err(internal_err)?;
            let path =
                vec![mast_yaml_edit::key("services"), mast_yaml_edit::key(&service),
                     mast_yaml_edit::key("image")];
            let current = mast_yaml_edit::get_scalar(&before, &path).ok_or_else(|| {
                ErrorInfo::InvalidInput {
                    message: format!("{service} has no image: to retag"),
                }
            })?;
            // Quote to match how compose files spell images, and so a tag that
            // looks numeric ("8.0") cannot be read back as a YAML float.
            let quoted = format!("'{image}'");
            if current.trim() == quoted || current.trim() == image {
                return Ok(FileEditPreview {
                    file: file.to_string_lossy().into_owned(),
                    before: before.clone(),
                    after: before,
                    summary: vec![format!("{service} already runs {image}")],
                    no_op: true,
                });
            }
            let edits = [mast_yaml_edit::Edit::SetScalar { path, value: quoted }];
            let after = mast_yaml_edit::apply_all(&before, &edits)
                .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
            let mut summary = Vec::new();
            match &guard {
                Some((mast_laravel::db::VersionVerdict::WillNotStart { reason }, vol)) => {
                    summary.push(format!("WILL NOT START: {reason}"));
                    summary.push(format!(
                        "applying this is refused — export the data on a {vol}-compatible \
                         image first, or use the recreate-volume repair if the data is \
                         disposable"
                    ));
                }
                Some((mast_laravel::db::VersionVerdict::InPlaceUpgrade { note }, _)) => {
                    summary.push(format!("WARNING: {note}"));
                }
                None => {}
            }
            summary.push(format!(
                "{service}: image {} -> {image}",
                current.trim().trim_matches('\'')
            ));
            summary.push("the running container keeps the old image until you rebuild".into());
            Ok(FileEditPreview {
                file: file.to_string_lossy().into_owned(),
                before,
                after,
                summary,
                no_op: false,
            })
        })
        .await
        .map_err(internal_err)?
    }

    /// Apply a retag through the same write transaction as every other compose
    /// edit.
    pub(crate) async fn apply_service_image(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        service: &str,
        image: &str,
    ) -> Result<(), ErrorInfo> {
        let (invocation, file) = self.catalog_context(project)?;
        match self.retag_version_verdict(project, service, image).await {
            Some((mast_laravel::db::VersionVerdict::WillNotStart { reason }, vol)) => {
                // A guaranteed crash-loop is refused; the compose file stays
                // an escape hatch for anyone who really means it.
                return Err(ErrorInfo::Conflict {
                    message: format!(
                        "{reason}. Export the data on a {vol}-compatible image first, use \
                         the recreate-volume repair if it is disposable, or edit the \
                         compose file directly to override."
                    ),
                });
            }
            Some((mast_laravel::db::VersionVerdict::InPlaceUpgrade { note }, _)) => {
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output { line: format!("note: {note}"), stderr: true },
                );
            }
            None => {}
        }
        let path = vec![
            mast_yaml_edit::key("services"),
            mast_yaml_edit::key(service),
            mast_yaml_edit::key("image"),
        ];
        let edits = [mast_yaml_edit::Edit::SetScalar { path, value: format!("'{image}'") }];
        self.write_compose(
            &invocation,
            &file,
            &edits,
            vec![format!("{service}: image set to {image}")],
        )
        .await?;
        self.emit_op(
            handle,
            op,
            OperationEventKind::Output {
                line: format!("{service}: image set to {image} — rebuild to apply it"),
                stderr: false,
            },
        );
        self.hint();
        Ok(())
    }

    /// Apply a catalog add/remove: compose edit through the 8-gate write
    /// transaction, then (on add) the documented `.env` updates.
    pub(crate) async fn apply_catalog(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        service: &str,
        remove: bool,
    ) -> Result<(), ErrorInfo> {
        let (invocation, file) = self.catalog_context(project)?;
        let def = mast_compose::catalog::catalog_def(service)
            .ok_or_else(|| ErrorInfo::InvalidInput { message: format!("unknown service {service}") })?;
        self.apply_catalog_to(handle, op, &invocation, &file, def, remove).await
    }

    fn custom_to_compose(spec: &mast_contract::CustomServiceSpec) -> mast_compose::catalog::CustomService {
        mast_compose::catalog::CustomService {
            name: spec.name.clone(),
            image: spec.image.clone(),
            ports: spec.ports.clone(),
            volume: spec.volume.clone(),
            command: spec.command.clone(),
        }
    }

    /// Preview adding a user-described service (apply via
    /// `Action::AddCustomService`).
    pub async fn custom_service_preview(
        &self,
        project: &ProjectId,
        spec: &mast_contract::CustomServiceSpec,
    ) -> Result<FileEditPreview, ErrorInfo> {
        let (_invocation, file) = self.catalog_context(project)?;
        let custom = Self::custom_to_compose(spec);
        tokio::task::spawn_blocking(move || {
            let before = std::fs::read_to_string(&file).map_err(internal_err)?;
            let plan = mast_compose::catalog::plan_custom_add(&before, &custom)
                .map_err(|message| ErrorInfo::InvalidInput { message })?;
            let after = mast_yaml_edit::apply_all(&before, &plan.edits)
                .map_err(|e| ErrorInfo::InvalidInput { message: e.to_string() })?;
            Ok(FileEditPreview {
                file: file.to_string_lossy().into_owned(),
                before,
                after,
                summary: plan.summary,
                no_op: false,
            })
        })
        .await
        .map_err(internal_err)?
    }

    pub(crate) async fn apply_custom_service(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        project: &ProjectId,
        spec: &mast_contract::CustomServiceSpec,
    ) -> Result<(), ErrorInfo> {
        let (invocation, file) = self.catalog_context(project)?;
        let custom = Self::custom_to_compose(spec);
        let source = tokio::task::spawn_blocking({
            let file = file.clone();
            move || std::fs::read_to_string(file)
        })
        .await
        .map_err(internal_err)?
        .map_err(internal_err)?;
        let plan = mast_compose::catalog::plan_custom_add(&source, &custom)
            .map_err(|message| ErrorInfo::InvalidInput { message })?;
        self.write_compose(&invocation, &file, &plan.edits, plan.summary.clone()).await?;
        for line in &plan.summary {
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output { line: line.clone(), stderr: false },
            );
        }
        self.hint();
        Ok(())
    }

    /// Every compose write goes through here: the 8-gate transaction plus the
    /// history record, so a config change Mast made is as visible as a command
    /// it ran — including the ones the transaction refused.
    pub(crate) async fn write_compose(
        &self,
        invocation: &mast_compose::ComposeInvocation,
        file: &std::path::Path,
        edits: &[mast_yaml_edit::Edit],
        summary: Vec<String>,
    ) -> Result<(), ErrorInfo> {
        let backups = self.inner.deps.store.backups_dir();
        let result = mast_compose::apply_compose_edit(invocation, file, edits, Some(&backups))
            .await
            .map_err(|e| match e {
                mast_compose::ComposeEditError::ConflictExternalEdit => {
                    ErrorInfo::Conflict { message: e.to_string() }
                }
                other => ErrorInfo::InvalidInput { message: other.to_string() },
            });
        self.record_config_write(file, summary, &result);
        result.map(|_| ())
    }

    /// The catalog write itself, given a resolved invocation.
    pub(crate) async fn apply_catalog_to(
        &self,
        handle: &Arc<OpHandle>,
        op: OperationId,
        invocation: &mast_compose::ComposeInvocation,
        file: &std::path::Path,
        def: &'static mast_compose::catalog::CatalogDef,
        remove: bool,
    ) -> Result<(), ErrorInfo> {
        let source = tokio::task::spawn_blocking({
            let file = file.to_path_buf();
            move || std::fs::read_to_string(file)
        })
        .await
        .map_err(internal_err)?
        .map_err(internal_err)?;
        let plan = if remove {
            mast_compose::catalog::plan_catalog_remove(&source, def)
        } else {
            mast_compose::catalog::plan_catalog_add(&source, def)
        }
        .map_err(|message| ErrorInfo::InvalidInput { message })?;

        self.write_compose(invocation, file, &plan.edits, plan.summary.clone()).await?;
        for line in &plan.summary {
            self.emit_op(
                handle,
                op,
                OperationEventKind::Output { line: line.clone(), stderr: false },
            );
        }

        if !remove && !def.env_sets.is_empty() {
            let env_path = invocation.project_dir.join(".env");
            if env_path.is_file() {
                let backups = self.inner.deps.store.backups_dir();
                let sets: Vec<(String, String)> =
                    def.env_sets.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
                let summary: Vec<String> = sets
                    .iter()
                    .map(|(k, v)| {
                        format!("{k}={}", if mast_laravel::is_secret_key(k) { crate::REDACTED } else { v })
                    })
                    .collect();
                let result = tokio::task::spawn_blocking({
                    let env_path = env_path.clone();
                    move || {
                        mast_laravel::edit_env_file(&env_path, Some(&backups), |f| {
                            for (k, v) in &sets {
                                f.set(k, v)?;
                            }
                            Ok(())
                        })
                    }
                })
                .await
                .map_err(internal_err)?
                .map_err(env_write_error);
                self.record_config_write(&env_path, summary, &result);
                result?;
            } else {
                self.emit_op(
                    handle,
                    op,
                    OperationEventKind::Output {
                        line: "no .env file — skipped the env updates".into(),
                        stderr: true,
                    },
                );
            }
        }
        self.hint();
        Ok(())
    }
}
