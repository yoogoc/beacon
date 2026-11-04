//! Standalone, single-context kubeconfigs. Exporting never authenticates or runs plugins.
use super::{ConnectionConfig, Kubeconfig, MAX_FILE_BYTES};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    path::Path,
};

#[derive(Debug, thiserror::Error)]
pub enum Failure {
    #[error("The selected context is missing from the kubeconfig source.")]
    Context,
    #[error("The context's cluster is missing from the kubeconfig source.")]
    Cluster,
    #[error("The context's user is missing from the kubeconfig source.")]
    User,
    #[error("Could not read {field}: {reason}")]
    Reference { field: &'static str, reason: String },
    #[error("The {0} file exceeds the 4 MiB export limit.")]
    TooLarge(&'static str),
    #[error("The token file is not valid UTF-8.")]
    TokenEncoding,
    #[error("Could not serialize the kubeconfig.")]
    Serialize,
    #[error("{0}")]
    File(String),
}

pub fn filename(name: &str) -> String {
    let stem: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .take(120)
        .collect();
    let stem = stem.trim_matches(['-', '_', '.']);
    format!(
        "kubeconfig-{}.yaml",
        if stem.is_empty() { "cluster" } else { stem }
    )
}

fn read_reference(path: &str, field: &'static str) -> Result<Vec<u8>, Failure> {
    let read = || -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    let bytes = read().map_err(|e| Failure::Reference {
        field,
        reason: e.to_string(),
    })?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(Failure::TooLarge(field));
    }
    Ok(bytes)
}

/// Source loading has already resolved paths relative to each original file.
/// Minimize before reading references, so unrelated users and clusters are never
/// exported or accessed. Inline data takes precedence over stale file references.
pub fn standalone(target: &ConnectionConfig) -> Result<Kubeconfig, Failure> {
    let config = &target.config;
    let context = config
        .contexts
        .iter()
        .find(|named| named.name == target.context && named.context.is_some())
        .cloned()
        .ok_or(Failure::Context)?;
    let body = context.context.as_ref().ok_or(Failure::Context)?;
    let mut cluster = config
        .clusters
        .iter()
        .find(|named| named.name == body.cluster && named.cluster.is_some())
        .cloned()
        .ok_or(Failure::Cluster)?;
    let mut users = Vec::new();
    if let Some(name) = body.user.as_ref().filter(|name| !name.is_empty()) {
        let mut user = config
            .auth_infos
            .iter()
            .find(|named| &named.name == name && named.auth_info.is_some())
            .cloned()
            .ok_or(Failure::User)?;
        let auth = user.auth_info.as_mut().ok_or(Failure::User)?;
        if auth.client_certificate_data.is_none()
            && let Some(path) = &auth.client_certificate
        {
            auth.client_certificate_data =
                Some(STANDARD.encode(read_reference(path, "client-certificate")?));
        }
        auth.client_certificate = None;
        if auth.client_key_data.is_none()
            && let Some(path) = &auth.client_key
        {
            auth.client_key_data =
                Some(STANDARD.encode(read_reference(path, "client-key")?).into());
        }
        auth.client_key = None;
        if auth.token.is_none()
            && let Some(path) = &auth.token_file
        {
            let token = String::from_utf8(read_reference(path, "tokenFile")?)
                .map_err(|_| Failure::TokenEncoding)?;
            auth.token = Some(token.trim().to_owned().into());
        }
        auth.token_file = None;
        users.push(user);
    }
    let body = cluster.cluster.as_mut().ok_or(Failure::Cluster)?;
    if body.certificate_authority_data.is_none()
        && let Some(path) = &body.certificate_authority
    {
        body.certificate_authority_data =
            Some(STANDARD.encode(read_reference(path, "certificate-authority")?));
    }
    body.certificate_authority = None;
    Ok(Kubeconfig {
        api_version: Some("v1".into()),
        kind: Some("Config".into()),
        current_context: Some(target.context.clone()),
        contexts: vec![context],
        clusters: vec![cluster],
        auth_infos: users,
        ..Kubeconfig::default()
    })
}

/// Stage owner-only output beside its destination. Failed reads/serialization
/// leave existing files intact; the native save dialog owns overwrite consent.
pub fn save(target: &ConnectionConfig, path: &Path) -> Result<(), Failure> {
    let config = standalone(target)?;
    let yaml = serde_saphyr::to_string(&config).map_err(|_| Failure::Serialize)?;
    let directory = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(directory).map_err(file_error)?;
    file.write_all(yaml.as_bytes()).map_err(file_error)?;
    file.as_file().sync_all().map_err(file_error)?;
    file.persist(path).map_err(|e| file_error(e.error))?;
    Ok(())
}

fn file_error(error: std::io::Error) -> Failure {
    Failure::File(error.to_string())
}

#[cfg(test)]
mod tests;
