//! Full resource YAML exports, committed atomically only after every read succeeds.
use crate::{ClusterSession, DeleteTarget, DynamicObject, Kind, ObjectRef};
use std::{collections::HashSet, io::Write, path::PathBuf, sync::Arc};
use tempfile::NamedTempFile;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[derive(Debug, thiserror::Error)]
pub enum Failure {
    #[error("Select at least one resource to export.")]
    Empty,
    #[error("{target}: {message}")]
    Resource { target: ObjectRef, message: String },
    #[error("{0}")]
    File(String),
}

/// Safe on Windows, macOS and Linux, including archive extraction. Kubernetes
/// names can be long; the writer disambiguates any truncation or case collision.
pub fn filename(kind: &Kind, reference: &ObjectRef) -> String {
    let stem = format!(
        "{}-{}{}",
        kind.resource.kind.to_ascii_lowercase(),
        reference
            .namespace
            .as_ref()
            .map_or(String::new(), |ns| format!("{ns}-")),
        reference.name,
    );
    let stem: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(180)
        .collect();
    format!("{stem}.yaml")
}

pub fn suggested_filename(kind: &Kind, targets: &[DeleteTarget]) -> Option<String> {
    match targets {
        [] => None,
        [target] => Some(filename(kind, &target.reference)),
        _ => Some(format!(
            "{}-resources.zip",
            kind.resource.kind.to_ascii_lowercase()
        )),
    }
}

enum Output {
    Yaml(NamedTempFile),
    Zip(Box<ZipWriter<NamedTempFile>>),
}
impl Output {
    fn write(&mut self, name: &str, object: &DynamicObject) -> Result<(), Failure> {
        let yaml = serde_saphyr::to_string(object).map_err(file_error)?;
        match self {
            Self::Yaml(file) => file.write_all(yaml.as_bytes()).map_err(file_error),
            Self::Zip(zip) => {
                zip.start_file(
                    name,
                    SimpleFileOptions::default()
                        .compression_method(CompressionMethod::Deflated)
                        .unix_permissions(0o600),
                )
                .map_err(file_error)?;
                zip.write_all(yaml.as_bytes()).map_err(file_error)
            }
        }
    }
    fn finish(self, path: PathBuf) -> Result<(), Failure> {
        let file = match self {
            Self::Yaml(file) => file,
            Self::Zip(zip) => zip.finish().map_err(file_error)?,
        };
        file.as_file().sync_all().map_err(file_error)?;
        // The native save dialog owns overwrite confirmation. Until this final
        // rename, failures leave the chosen destination untouched.
        file.persist(path).map_err(file_error)?;
        Ok(())
    }
}
fn file_error(error: impl std::fmt::Display) -> Failure {
    Failure::File(error.to_string())
}

/// Reads one full object at a time (list rows are deliberately slimmed). YAML
/// serialization and ZIP compression run on blocking workers, not the UI or
/// network executor. A failed read never leaves a partial destination archive.
pub async fn save(
    session: Arc<ClusterSession>,
    kind: Kind,
    targets: Vec<DeleteTarget>,
    path: PathBuf,
) -> Result<(), Failure> {
    if targets.is_empty() {
        return Err(Failure::Empty);
    }
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    let archive = targets.len() > 1;
    let mut output = tokio::task::spawn_blocking(move || {
        let temporary = NamedTempFile::new_in(directory).map_err(file_error)?;
        Ok::<_, Failure>(if archive {
            Output::Zip(Box::new(ZipWriter::new(temporary)))
        } else {
            Output::Yaml(temporary)
        })
    })
    .await
    .map_err(file_error)??;
    let mut names = HashSet::new();
    for target in targets {
        let mut object = session
            .clone()
            .get_object(
                kind.resource.clone(),
                target.reference.namespace.clone(),
                target.reference.name.clone(),
            )
            .await
            .map_err(|error| Failure::Resource {
                target: target.reference.clone(),
                message: error.user_message(),
            })?;
        if target.uid.is_empty() || object.metadata.uid.as_deref() != Some(&target.uid) {
            return Err(Failure::Resource {
                target: target.reference,
                message: "This resource was replaced. Refresh the list and select it again.".into(),
            });
        }
        // Matches kubectl's normal YAML output: keep the resource and its data,
        // including Secret values, while omitting verbose field-manager history.
        object.metadata.managed_fields = None;
        let original = filename(&kind, &target.reference);
        let mut name = original.clone();
        let mut suffix = 2;
        while !names.insert(name.to_ascii_lowercase()) {
            name = format!("{}-{suffix}.yaml", original.trim_end_matches(".yaml"));
            suffix += 1;
        }
        output = tokio::task::spawn_blocking(move || {
            output.write(&name, &object)?;
            Ok::<_, Failure>(output)
        })
        .await
        .map_err(file_error)??;
    }
    tokio::task::spawn_blocking(move || output.finish(path))
        .await
        .map_err(file_error)?
}

#[cfg(test)]
mod tests;
