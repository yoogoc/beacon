//! Helm writes use a frozen chart archive and values after explicit UI review.
//! The CLI receives the session's private kubeconfig and proxy; never current-context.
use super::{History, Identity, Revision, history};
use crate::{ClusterSession, Error, Result};
use futures::channel::mpsc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Write as _, path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::io::AsyncReadExt as _;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChartVersion {
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub app_version: String,
    #[serde(default)]
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct Chart {
    pub reference: String,
    pub version: String,
    pub repository: String,
}
#[derive(Debug, Clone)]
pub enum Change {
    Install(Chart),
    Upgrade(Chart),
    Rollback(i64),
    Uninstall,
}
impl Change {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Install(_) => "Install",
            Self::Upgrade(_) => "Upgrade",
            Self::Rollback(_) => "Rollback",
            Self::Uninstall => "Uninstall",
        }
    }
}
#[derive(Debug, Clone)]
pub struct Request {
    pub identity: Identity,
    pub change: Change,
    pub values: Value,
}

/// Only preparation constructs a plan; reviewed archive and values remain alive until completion.
pub struct Plan {
    pub request: Request,
    pub before_values: Value,
    pub after_values: Value,
    pub before_manifest: String,
    pub after_manifest: String,
    pub baseline: i64,
    pub helm_version: String,
    archive: Option<PathBuf>,
    values: Option<PathBuf>,
    _directory: tempfile::TempDir,
}
fn fail(message: impl Into<String>) -> Error {
    Error::Manifest(message.into())
}
fn identity(identity: &Identity) -> Result<()> {
    let valid = |value: &str, max: usize| {
        !value.is_empty()
            && value.len() <= max
            && !value.starts_with('-')
            && !value.ends_with('-')
            && value
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'.'))
    };
    if !valid(&identity.name, 53) || !valid(&identity.namespace, 63) {
        return Err(fail("Enter a valid Helm release name and namespace."));
    }
    Ok(())
}
fn chart(chart: &Chart) -> Result<()> {
    if chart.reference.trim().is_empty()
        || chart.reference.starts_with('-')
        || chart.reference.contains(['\n', '\r', '\0'])
    {
        return Err(fail(
            "Choose a chart reference, OCI URL or local chart archive.",
        ));
    }
    if chart.version.is_empty()
        || !chart
            .version
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-' | b'+' | b'_'))
    {
        return Err(fail(
            "Choose an exact chart version before preparing the release.",
        ));
    }
    if !chart.repository.is_empty() {
        let url = url::Url::parse(&chart.repository)
            .map_err(|_| fail("Enter an http(s) chart repository URL."))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(fail(
                "Enter an http(s) chart repository URL without embedded credentials.",
            ));
        }
    }
    Ok(())
}
fn command(session: &ClusterSession, namespace: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(session.helm_program());
    command
        .env("HELM_DRIVER", "secret")
        .env("HELM_DEBUG", "false");
    // Helm's global environment overrides must not redirect a reviewed session.
    for name in [
        "HELM_KUBECONTEXT",
        "HELM_NAMESPACE",
        "HELM_KUBEAPISERVER",
        "HELM_KUBETOKEN",
        "HELM_KUBECAFILE",
        "HELM_KUBEASUSER",
        "HELM_KUBEASGROUPS",
        "HELM_KUBETLS_SERVER_NAME",
        "HELM_KUBEINSECURE_SKIP_TLS_VERIFY",
    ] {
        command.env_remove(name);
    }
    command.args([
        "--kube-context",
        session.id().context_name(),
        "--namespace",
        namespace,
    ]);
    if let Some(config) = &session.file_transport().config {
        command.arg("--kubeconfig").arg(config.path());
    }
    for (key, value) in session.file_transport().environment() {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}
async fn read<R: tokio::io::AsyncRead + Unpin>(
    stream: Option<R>,
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    let Some(mut stream) = stream else {
        return Ok(vec![]);
    };
    let mut out = vec![];
    let mut buffer = [0; 8192];
    loop {
        let count = stream.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        if out.len() + count > limit {
            return Err(std::io::Error::other(
                "Helm output exceeded the supported size.",
            ));
        }
        out.extend_from_slice(&buffer[..count]);
    }
    Ok(out)
}
async fn run(mut command: tokio::process::Command, timeout: Duration) -> Result<String> {
    let mut credentials = Vec::new();
    for (name, value) in command.as_std().get_envs() {
        if name
            .to_string_lossy()
            .to_ascii_uppercase()
            .ends_with("_PROXY")
            && let Some(value) = value
            && let Ok(url) = url::Url::parse(&value.to_string_lossy())
            && let Some(password) = url.password()
        {
            credentials.push(password.to_owned());
            credentials.push(value.to_string_lossy().into_owned());
        }
    }
    let mut child=command.spawn().map_err(|error|fail(format!("Helm could not start: {error}. Install Helm 3.13+ or Helm 4 and make it available on Beacon's PATH.")))?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let result = tokio::time::timeout(timeout, async {
        let (stdout, stderr) =
            tokio::try_join!(read(stdout, 32 * 1024 * 1024), read(stderr, 256 * 1024))?;
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((stdout, stderr, status))
    })
    .await;
    match result {
        Ok(Ok((stdout, _stderr, status))) if status.success() => {
            Ok(String::from_utf8_lossy(&stdout).into_owned())
        }
        Ok(Ok((_, stderr, status))) => {
            let mut message = String::from_utf8_lossy(&stderr).trim().to_string();
            for value in credentials {
                if !value.is_empty() {
                    message = message.replace(&value, "[concealed]");
                }
            }
            Err(fail(format!("Helm failed ({status}): {message}")))
        }
        Ok(Err(error)) => {
            let _ = child.kill().await;
            Err(fail(format!("Helm process failed: {error}")))
        }
        Err(_) => {
            let _ = child.kill().await;
            Err(fail(
                "Helm timed out. The release may have partially changed; refresh its history before retrying.",
            ))
        }
    }
}
async fn availability(session: &ClusterSession, namespace: &str) -> Result<String> {
    let mut command = command(session, namespace);
    command.args(["version", "--short"]);
    let version = run(command, Duration::from_secs(15)).await?;
    let numbers: Vec<_> = version
        .trim()
        .trim_start_matches('v')
        .split(['.', '+', '-'])
        .take(2)
        .filter_map(|s| s.parse::<u32>().ok())
        .collect();
    if !matches!(numbers.as_slice(),[3,minor] if *minor>=13)
        && !matches!(numbers.as_slice(), [4, _])
    {
        return Err(fail(
            "Write operations require Helm 3.13+ or Helm 4. Read-only release details remain available.",
        ));
    }
    Ok(version.trim().into())
}
pub async fn versions(
    session: Arc<ClusterSession>,
    namespace: String,
    reference: String,
) -> Result<Vec<ChartVersion>> {
    if reference.is_empty() || reference.starts_with('-') {
        return Err(fail("Enter a repository/chart reference first."));
    }
    availability(&session, &namespace).await?;
    let mut command = command(&session, &namespace);
    command.args([
        "search",
        "repo",
        &reference,
        "--versions",
        "--output",
        "json",
    ]);
    let text = run(command, Duration::from_secs(60)).await?;
    let mut versions: Vec<ChartVersion> = serde_json::from_str(&text)
        .map_err(|_| fail("Helm returned an invalid chart version list."))?;
    versions.retain(|chart| chart.name == reference);
    Ok(versions)
}
fn manifest(revision: &Revision) -> String {
    std::iter::once(&revision.manifest)
        .chain(revision.hooks.iter())
        .filter(|text| !text.trim().is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n---\n")
}
fn latest(history: &History) -> Result<Option<&Revision>> {
    match history.revisions.first() {
        None => Ok(None),
        Some(record) => record.revision.as_ref().map(Some).ok_or_else(||fail("The newest stored revision is unreadable. Repair it before a Helm write operation.")),
    }
}
async fn storage_permissions(
    session: &Arc<ClusterSession>,
    namespace: &str,
    change: &Change,
) -> Result<()> {
    let rules = session.clone().rules(Some(namespace.into())).await;
    let verbs: &[&str] = match change {
        Change::Uninstall => &["get", "list", "update"],
        _ => &["get", "list", "create", "update"],
    };
    for verb in verbs {
        if !rules.allows(verb, "", "secrets") {
            return Err(fail(format!(
                "Permission denied: {verb} secrets in {namespace} is required for Helm release storage."
            )));
        }
    }
    Ok(())
}
async fn resources_permissions(session: &Arc<ClusterSession>, plan: &Plan) -> Result<()> {
    let mut old: Vec<Value> = serde_saphyr::from_multiple(&plan.before_manifest)
        .map_err(|_| fail("The current manifest cannot be parsed for permission checks."))?;
    let desired: Vec<Value> = serde_saphyr::from_multiple(&plan.after_manifest)
        .map_err(|_| fail("The rendered manifest cannot be parsed for permission checks."))?;
    let id = |value: &Value| {
        (
            value["apiVersion"].as_str().unwrap_or_default().to_owned(),
            value["kind"].as_str().unwrap_or_default().to_owned(),
            value
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or(&plan.request.identity.namespace)
                .to_owned(),
            value
                .pointer("/metadata/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        )
    };
    let mut operations = vec![];
    for resource in desired {
        if resource.is_null() {
            continue;
        }
        let matching = old.iter().position(|value| id(value) == id(&resource));
        let unchanged = matching.is_some_and(|index| old[index] == resource);
        if let Some(index) = matching {
            old.remove(index);
        }
        if !unchanged {
            operations.push((
                resource,
                if matching.is_some() {
                    "patch"
                } else {
                    "create"
                },
            ));
        }
    }
    operations.extend(
        old.into_iter()
            .filter(|resource| !resource.is_null())
            .map(|resource| (resource, "delete")),
    );
    for (resource, verb) in operations {
        let api = resource["apiVersion"].as_str().unwrap_or_default();
        let (group, version) = api.split_once('/').unwrap_or(("", api));
        let kind = resource["kind"].as_str().unwrap_or_default();
        let Some(kind) = session.discovery().kinds().iter().find(|candidate| {
            candidate.resource.group == group
                && candidate.resource.version == version
                && candidate.resource.kind == kind
        }) else {
            continue;
        };
        let namespace = kind.namespaced.then(|| {
            resource
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                .unwrap_or(&plan.request.identity.namespace)
                .to_owned()
        });
        let rules = session.clone().rules(namespace.clone()).await;
        if !rules.allows(verb, group, &kind.resource.plural) {
            return Err(fail(format!(
                "Permission denied: {verb} {} in {}.",
                kind.resource.plural,
                namespace.as_deref().unwrap_or("cluster scope")
            )));
        }
    }
    Ok(())
}
pub async fn prepare(session: Arc<ClusterSession>, request: Request) -> Result<Plan> {
    let values = request.values.clone();
    prepare_inner(session, request)
        .await
        .map_err(|error| fail(scrub(&values, error.user_message())))
}
fn scrub(value: &Value, mut text: String) -> String {
    fn walk(value: &Value, sensitive: bool, replacements: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, value) in map {
                    let key = key.to_lowercase();
                    walk(
                        value,
                        sensitive
                            || ["password", "token", "secret", "private", "key"]
                                .iter()
                                .any(|part| key.contains(part)),
                        replacements,
                    );
                }
            }
            Value::Array(items) => items
                .iter()
                .for_each(|value| walk(value, sensitive, replacements)),
            Value::String(value) if !value.is_empty() && (sensitive || value.len() > 3) => {
                replacements.push(value.clone())
            }
            _ => {}
        }
    }
    let mut replacements = vec![];
    walk(value, false, &mut replacements);
    replacements.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in replacements {
        text = text.replace(&value, "[concealed]");
    }
    text
}
fn scrub_manifest(manifest: &str, mut message: String) -> String {
    fn walk(value: &Value, message: &mut String) {
        if value["kind"] == "Secret" {
            for field in ["data", "stringData"] {
                if let Some(data) = value.get(field) {
                    *message = scrub(&serde_json::json!({"secret":data}), message.clone());
                }
            }
        } else {
            match value {
                Value::Object(map) => map.values().for_each(|value| walk(value, message)),
                Value::Array(items) => items.iter().for_each(|value| walk(value, message)),
                _ => {}
            }
        }
    }
    if let Ok(documents) = serde_saphyr::from_multiple::<Value>(manifest) {
        for document in documents {
            walk(&document, &mut message);
        }
    }
    message
}
async fn prepare_inner(session: Arc<ClusterSession>, request: Request) -> Result<Plan> {
    identity(&request.identity)?;
    if !request.values.is_object() {
        return Err(fail("Helm values must be a YAML mapping."));
    }
    storage_permissions(&session, &request.identity.namespace, &request.change).await?;
    let version = availability(&session, &request.identity.namespace).await?;
    let stored = history(session.client(), &request.identity).await?;
    let current = latest(&stored)?;
    match &request.change {
        Change::Install(_)
            if current.is_some_and(|revision| revision.summary.status != "uninstalled") =>
        {
            return Err(fail(
                "This release already exists. Choose Upgrade or another name.",
            ));
        }
        Change::Upgrade(_) | Change::Rollback(_) | Change::Uninstall if current.is_none() => {
            return Err(fail(
                "This release no longer exists. Refresh the release list.",
            ));
        }
        _ => {}
    }
    let directory = tempfile::tempdir().map_err(|error| fail(error.to_string()))?;
    let mut plan = Plan {
        baseline: current.map_or(0, |release| release.summary.revision),
        before_values: current
            .filter(|_| !matches!(request.change, Change::Install(_)))
            .map_or_else(|| serde_json::json!({}), |release| release.values.clone()),
        before_manifest: current
            .filter(|_| !matches!(request.change, Change::Install(_)))
            .map_or_else(String::new, manifest),
        after_values: request.values.clone(),
        after_manifest: String::new(),
        request,
        helm_version: version,
        archive: None,
        values: None,
        _directory: directory,
    };
    match &plan.request.change {
        Change::Install(chart_spec) | Change::Upgrade(chart_spec) => {
            chart(chart_spec)?;
            let mut pull = command(&session, &plan.request.identity.namespace);
            let path = std::path::Path::new(&chart_spec.reference);
            if path.is_file() {
                let archive = plan._directory.path().join("chart.tgz");
                std::fs::copy(path, &archive).map_err(|error| fail(error.to_string()))?;
                plan.archive = Some(archive);
            } else {
                if path.is_dir() {
                    pull.args(["package", &chart_spec.reference]);
                } else {
                    pull.args([
                        "pull",
                        &chart_spec.reference,
                        "--version",
                        &chart_spec.version,
                    ]);
                    if !chart_spec.repository.is_empty() {
                        pull.args(["--repo", &chart_spec.repository]);
                    }
                }
                pull.arg("--destination").arg(plan._directory.path());
                run(pull, Duration::from_secs(120)).await?;
                plan.archive = std::fs::read_dir(plan._directory.path())
                    .map_err(|error| fail(error.to_string()))?
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .find(|path| path.extension().is_some_and(|extension| extension == "tgz"));
            }
            let archive = plan
                .archive
                .as_ref()
                .ok_or_else(|| fail("Helm did not produce a chart archive."))?;
            let values = plan._directory.path().join("values.json");
            let mut file =
                std::fs::File::create(&values).map_err(|error| fail(error.to_string()))?;
            serde_json::to_writer(&mut file, &plan.request.values)
                .map_err(|error| fail(error.to_string()))?;
            file.flush().map_err(|error| fail(error.to_string()))?;
            plan.values = Some(values.clone());
            let mut render = command(&session, &plan.request.identity.namespace);
            render
                .arg(plan.request.change.label().to_lowercase())
                .arg(&plan.request.identity.name)
                .arg(archive)
                .args(["--dry-run=server", "--output", "json", "--timeout", "5m"])
                .arg("--values")
                .arg(&values);
            if matches!(plan.request.change, Change::Install(_)) && plan.baseline > 0 {
                render.arg("--replace");
            }
            let text = run(render, Duration::from_secs(120)).await?;
            let release: Value = serde_json::from_str(&text)
                .map_err(|_| fail("Helm returned an invalid release preview."))?;
            if release["name"] != plan.request.identity.name
                || release["namespace"] != plan.request.identity.namespace
            {
                return Err(fail(
                    "Helm's preview identity does not match the reviewed target.",
                ));
            }
            if release
                .pointer("/chart/metadata/version")
                .and_then(Value::as_str)
                != Some(chart_spec.version.as_str())
            {
                return Err(fail(
                    "The downloaded chart version differs from the chosen version. Choose its exact version and prepare again.",
                ));
            }
            plan.after_manifest = release
                .get("manifest")
                .and_then(Value::as_str)
                .ok_or_else(|| fail("Helm preview omitted the rendered manifest."))?
                .into();
            for hook in release
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|hook| hook.get("manifest").and_then(Value::as_str))
            {
                plan.after_manifest.push_str("\n---\n");
                plan.after_manifest.push_str(hook);
            }
            if matches!(plan.request.change, Change::Install(_)) {
                let mut crds = command(&session, &plan.request.identity.namespace);
                crds.args(["show", "crds"]).arg(archive);
                let crds = run(crds, Duration::from_secs(30)).await?;
                if !crds.trim().is_empty() {
                    plan.after_manifest.push_str("\n---\n");
                    plan.after_manifest.push_str(&crds);
                }
            }
        }
        Change::Rollback(number) => {
            if *number < 1 || *number == plan.baseline {
                return Err(fail("Choose a different stored revision to roll back to."));
            }
            let revision = stored
                .revisions
                .iter()
                .find(|record| record.number == *number)
                .and_then(|record| record.revision.as_ref())
                .ok_or_else(|| {
                    fail("The requested rollback revision is unavailable or damaged.")
                })?;
            plan.after_values = revision.values.clone();
            plan.after_manifest = manifest(revision);
        }
        Change::Uninstall => {
            // Helm retains objects explicitly annotated with resource-policy=keep.
            let resources: Vec<Value> = serde_saphyr::from_multiple(&plan.before_manifest)
                .map_err(|_| fail("The current manifest cannot be reviewed for uninstall."))?;
            plan.after_manifest = resources
                .into_iter()
                .filter(|value| {
                    value
                        .pointer("/metadata/annotations/helm.sh~1resource-policy")
                        .and_then(Value::as_str)
                        == Some("keep")
                })
                .map(|value| {
                    serde_saphyr::to_string(&value)
                        .map_err(|_| fail("A retained resource could not be rendered."))
                })
                .collect::<Result<Vec<_>>>()?
                .join("---\n");
            plan.after_values = serde_json::json!({});
        }
    }
    resources_permissions(&session, &plan).await?;
    Ok(plan)
}
fn progress(sender: &mpsc::UnboundedSender<String>, message: &str) {
    let _ = sender.unbounded_send(message.into());
}
pub async fn execute(
    session: Arc<ClusterSession>,
    plan: Arc<Plan>,
    sender: mpsc::UnboundedSender<String>,
) -> Result<String> {
    progress(&sender, "Checking release state and permissions…");
    storage_permissions(
        &session,
        &plan.request.identity.namespace,
        &plan.request.change,
    )
    .await?;
    resources_permissions(&session, &plan).await?;
    let now = history(session.client(), &plan.request.identity).await?;
    if now.revisions.first().map_or(0, |record| record.number) != plan.baseline {
        return Err(fail(
            "The release changed after review. Prepare and review a new plan before continuing.",
        ));
    }
    let mut command = command(&session, &plan.request.identity.namespace);
    command
        .arg(plan.request.change.label().to_lowercase())
        .arg(&plan.request.identity.name);
    match &plan.request.change {
        Change::Install(_) | Change::Upgrade(_) => {
            command
                .arg(plan.archive.as_ref().unwrap())
                .arg("--values")
                .arg(plan.values.as_ref().unwrap())
                .args(["--output", "json"]);
        }
        Change::Rollback(number) => {
            command.arg(number.to_string());
        }
        Change::Uninstall => {
            command.args(["--keep-history", "--cascade", "foreground"]);
        }
    }
    if matches!(plan.request.change, Change::Install(_)) && plan.baseline > 0 {
        command.arg("--replace");
    }
    command.args(["--wait", "--timeout", "5m"]);
    progress(
        &sender,
        "Helm is applying the reviewed change and waiting for resources…",
    );
    match run(command, Duration::from_secs(900)).await {
        Ok(_) => {
            progress(&sender, "Finished. Refreshing release history…");
            Ok(format!(
                "{} completed for {}/{}.",
                plan.request.change.label(),
                plan.request.identity.namespace,
                plan.request.identity.name
            ))
        }
        Err(error) => {
            let mut message = error.user_message();
            message = scrub(&plan.after_values, message);
            message = scrub(&plan.before_values, message);
            message = scrub_manifest(&plan.after_manifest, message);
            message = scrub_manifest(&plan.before_manifest, message);
            Err(fail(message))
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
