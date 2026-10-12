//! Local kubeconfig sources. Loading discovers contexts without authenticating.
//! System configuration keeps kubectl's merge rules and its legacy identities;
//! explicitly added files resolve independently, including files in a folder.
use crate::{
    ClusterId,
    config::{ContextEntry, Contexts},
};
pub use kube::config::Kubeconfig;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
};

const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SourceKind {
    File {
        path: PathBuf,
    },
    Directory {
        path: PathBuf,
        #[serde(default)]
        recursive: bool,
        #[serde(default = "default_patterns")]
        patterns: String,
    },
    Imported,
}
pub fn default_patterns() -> String {
    "config, *.yaml, *.yml, *.config".into()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Keys use the relative file path (empty for single files) and context name.
    #[serde(default)]
    pub excluded: BTreeSet<String>,
}
fn yes() -> bool {
    true
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub system_enabled: bool,
    pub sources: Vec<Source>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            system_enabled: true,
            sources: vec![],
        }
    }
}
impl Preferences {
    pub fn validate(&self) -> Result<(), String> {
        if self.sources.len() > 128 {
            return Err("At most 128 kubeconfig sources are supported.".into());
        }
        let mut ids = HashSet::new();
        for source in &self.sources {
            if source.id.is_empty()
                || source.id.len() > 96
                || !source
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || source.id == "system"
                || !ids.insert(&source.id)
            {
                return Err("Kubeconfig source IDs must be unique local identifiers.".into());
            }
            if source.name.trim().is_empty() || source.name.len() > 120 {
                return Err("Source names must contain 1–120 characters.".into());
            }
            match &source.kind {
                SourceKind::File { path } | SourceKind::Directory { path, .. }
                    if path.as_os_str().is_empty() =>
                {
                    return Err("Choose a kubeconfig location.".into());
                }
                SourceKind::Directory { patterns, .. } if patterns.trim().is_empty() => {
                    return Err("Choose at least one filename pattern.".into());
                }
                _ => {}
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct ConnectionConfig {
    pub config: Arc<Kubeconfig>,
    pub context: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextInfo {
    pub entry: ContextEntry,
    pub file: PathBuf,
    pub key: String,
    pub server: String,
    pub authentication: String,
    pub included: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub file: PathBuf,
    pub message: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStatus {
    pub id: String,
    pub name: String,
    pub location: String,
    pub enabled: bool,
    pub files: usize,
    pub contexts: Vec<ContextInfo>,
    pub issues: Vec<Issue>,
}
#[derive(Clone, Default)]
pub struct Catalog {
    pub statuses: Vec<SourceStatus>,
    pub contexts: Contexts,
    pub targets: HashMap<ClusterId, ConnectionConfig>,
    pub fingerprint: u64,
    pub input_fingerprint: u64,
}
impl Catalog {
    pub fn source_for(&self, id: &ClusterId) -> Option<&SourceStatus> {
        self.statuses
            .iter()
            .find(|s| s.contexts.iter().any(|c| &c.entry.id == id))
    }
    pub fn labels(&self) -> HashMap<ClusterId, String> {
        self.statuses
            .iter()
            .filter(|source| source.id != "system")
            .flat_map(|source| {
                source.contexts.iter().map(move |context| {
                    let file = context.file.strip_prefix(Path::new(&source.location)).ok();
                    let label = file
                        .filter(|file| !file.as_os_str().is_empty())
                        .map_or_else(
                            || source.name.clone(),
                            |file| format!("{} · {}", source.name, file.display()),
                        );
                    (context.entry.id.clone(), label)
                })
            })
            .collect()
    }
}

pub fn system_paths() -> Vec<PathBuf> {
    if let Some(paths) = std::env::var_os("KUBECONFIG").filter(|s| !s.is_empty()) {
        std::env::split_paths(&paths)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    } else {
        std::env::home_dir()
            .map(|home| vec![home.join(".kube/config")])
            .unwrap_or_default()
    }
}
pub fn expand_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let expanded = if text == "~" || text.starts_with("~/") || text.starts_with("~\\") {
        std::env::home_dir()
            .map(|home| home.join(text.get(2..).unwrap_or_default()))
            .unwrap_or_else(|| path.to_owned())
    } else {
        path.to_owned()
    };
    if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir().unwrap_or_default().join(expanded)
    }
}
pub fn imported_path(directory: &Path, id: &str) -> PathBuf {
    directory.join("kubeconfigs").join(format!("{id}.yaml"))
}
fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf29ce484222325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    })
}
fn key(file: &str, context: &str) -> String {
    format!("{file}\n{context}")
}
fn scoped_id(source: &str, file: &str, context: &str) -> ClusterId {
    ClusterId::from_source(source, stable_hash(file), context)
}
fn glob(pattern: &str, name: &str) -> bool {
    let pattern: Vec<_> = pattern.chars().collect();
    let name: Vec<_> = name.chars().collect();
    let (mut p, mut n, mut star, mut resume) = (0, 0, None, 0);
    while n < name.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == name[n]) {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some(p);
            p += 1;
            resume = n;
        } else if let Some(s) = star {
            p = s + 1;
            resume += 1;
            n = resume;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}
fn scan(root: &Path, recursive: bool, patterns: &str) -> (Vec<PathBuf>, Vec<Issue>) {
    let mut files = vec![];
    let mut issues = vec![];
    let mut pending = vec![(root.to_owned(), 0)];
    while let Some((folder, depth)) = pending.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) => {
                issues.push(Issue {
                    file: folder,
                    message: error.to_string(),
                });
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    issues.push(Issue {
                        file: folder.clone(),
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            let path = entry.path();
            let ty = match entry.file_type() {
                Ok(ty) => ty,
                Err(error) => {
                    issues.push(Issue {
                        file: path,
                        message: error.to_string(),
                    });
                    continue;
                }
            };
            if ty.is_dir() && recursive && depth < 16 {
                pending.push((path, depth + 1));
            } else if ty.is_file()
                && patterns
                    .split(',')
                    .any(|pattern| glob(pattern.trim(), &entry.file_name().to_string_lossy()))
            {
                files.push(path);
            }
            if files.len() >= MAX_FILES || pending.len() >= MAX_FILES {
                issues.push(Issue {
                    file: root.to_owned(),
                    message:
                        "Scan limit reached (1000 files or folders). Use a more specific source."
                            .into(),
                });
                files.sort();
                return (files, issues);
            }
        }
    }
    files.sort();
    (files, issues)
}

fn read_config(path: &Path, fingerprint: &mut impl Hasher) -> Result<Kubeconfig, String> {
    path.hash(fingerprint);
    let size = fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_FILE_BYTES {
        return Err("Kubeconfig exceeds the 4 MiB limit.".into());
    }
    // Hash contents, rather than timestamps alone, to detect replacement and
    // coarse-timestamp saves. Reading and parsing only run on background threads.
    fs::read(path).map_err(|e| e.to_string())?.hash(fingerprint);
    Kubeconfig::read_from(path).map_err(config_error)
}
fn config_error(error: kube::config::KubeconfigError) -> String {
    if let kube::config::KubeconfigError::Parse(error) = error {
        // YAML diagnostics can contain credential values. Keep coordinates,
        // but do not render or log snippets from the configuration.
        error.location().map_or_else(
            || "Could not parse kubeconfig YAML.".into(),
            |location| {
                format!(
                    "Could not parse kubeconfig YAML at line {}, column {}.",
                    location.line(),
                    location.column()
                )
            },
        )
    } else {
        error.to_string()
    }
}
fn context_info(
    config: &Kubeconfig,
    entry: ContextEntry,
    file: PathBuf,
    context_key: String,
    included: bool,
) -> Result<ContextInfo, String> {
    let cluster = config
        .clusters
        .iter()
        .find(|c| c.name == entry.cluster)
        .and_then(|c| c.cluster.as_ref())
        .ok_or_else(|| {
            format!(
                "{}: referenced cluster is missing.",
                entry.id.context_name()
            )
        })?;
    let server = cluster.server.clone().ok_or("API server is missing.")?;
    let parsed =
        url::Url::parse(&server).map_err(|_| "API server must be an HTTP or HTTPS URL.")?;
    if !matches!(parsed.scheme(), "https" | "http") || parsed.host_str().is_none() {
        return Err("API server must be an HTTP or HTTPS URL.".into());
    }
    let authentication = match entry.user.as_ref() {
        Some(name) if !name.is_empty() => {
            let user = config
                .auth_infos
                .iter()
                .find(|u| &u.name == name)
                .and_then(|u| u.auth_info.as_ref())
                .ok_or_else(|| {
                    format!("{}: referenced user is missing.", entry.id.context_name())
                })?;
            if let Some(exec) = &user.exec {
                format!(
                    "Exec: {}",
                    exec.command.as_deref().unwrap_or("missing command")
                )
            } else if user.client_certificate.is_some() || user.client_certificate_data.is_some() {
                "Client certificate".into()
            } else if user.token.is_some() || user.token_file.is_some() {
                "Token".into()
            } else if user.auth_provider.is_some() {
                "Auth provider".into()
            } else if user.username.is_some() {
                "Basic authentication".into()
            } else {
                "No credentials".into()
            }
        }
        _ => "No credentials".into(),
    };
    Ok(ContextInfo {
        entry,
        file,
        key: context_key,
        server,
        authentication,
        included,
    })
}

pub fn load(preferences: &Preferences, directory: &Path, system: &[PathBuf]) -> Catalog {
    let mut catalog = Catalog::default();
    let mut entries = vec![];
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut seen = HashSet::new();
    let mut status = SourceStatus {
        id: "system".into(),
        name: "System kubeconfig".into(),
        location: system
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        enabled: preferences.system_enabled,
        files: 0,
        contexts: vec![],
        issues: vec![],
    };
    if preferences.system_enabled {
        let mut config = Kubeconfig::default();
        let mut failed = false;
        for path in system {
            let path = expand_path(path);
            match read_config(&path, &mut hasher)
                .and_then(|next| config.clone().merge(next).map_err(|e| e.to_string()))
            {
                Ok(next) => {
                    config = next;
                    status.files += 1;
                }
                Err(message) => {
                    status.issues.push(Issue {
                        file: path,
                        message,
                    });
                    failed = true;
                }
            }
        }
        // Preserve the existing all-or-nothing KUBECONFIG merge semantics.
        if !failed {
            for path in system {
                let path = expand_path(path);
                seen.insert(fs::canonicalize(&path).unwrap_or(path));
            }
            let config = Arc::new(config);
            for entry in Contexts::from_kubeconfig(&config).entries() {
                match context_info(
                    &config,
                    entry.clone(),
                    PathBuf::new(),
                    key("", entry.id.context_name()),
                    true,
                ) {
                    Ok(info) => {
                        catalog.targets.insert(
                            entry.id.clone(),
                            ConnectionConfig {
                                config: config.clone(),
                                context: entry.id.context_name().to_string(),
                            },
                        );
                        entries.push(entry.clone());
                        status.contexts.push(info);
                    }
                    Err(message) => status.issues.push(Issue {
                        file: PathBuf::new(),
                        message,
                    }),
                }
            }
        }
    }
    catalog.statuses.push(status);
    for source in &preferences.sources {
        let root = match &source.kind {
            SourceKind::File { path } | SourceKind::Directory { path, .. } => expand_path(path),
            SourceKind::Imported => imported_path(directory, &source.id),
        };
        let mut status = SourceStatus {
            id: source.id.clone(),
            name: source.name.clone(),
            location: root.display().to_string(),
            enabled: source.enabled,
            files: 0,
            contexts: vec![],
            issues: vec![],
        };
        if source.enabled {
            let (files, issues) = match &source.kind {
                SourceKind::Directory {
                    recursive,
                    patterns,
                    ..
                } => scan(&root, *recursive, patterns),
                _ => (vec![root.clone()], vec![]),
            };
            status.issues = issues;
            for path in files {
                let canonical = fs::canonicalize(&path).unwrap_or(path.clone());
                if !seen.insert(canonical) {
                    status.issues.push(Issue {
                        file: path,
                        message: "This file is already supplied by an earlier source.".into(),
                    });
                    continue;
                }
                match read_config(&path, &mut hasher) {
                    Ok(config) => {
                        status.files += 1;
                        let relative = if matches!(source.kind, SourceKind::Directory { .. }) {
                            path.strip_prefix(&root)
                                .unwrap_or(&path)
                                .to_string_lossy()
                                .replace('\\', "/")
                        } else {
                            String::new()
                        };
                        let config = Arc::new(config);
                        for entry in Contexts::from_kubeconfig(&config).entries() {
                            let context = entry.id.context_name().to_string();
                            let context_key = key(&relative, &context);
                            let included = !source.excluded.contains(&context_key);
                            let mut entry = entry.clone();
                            entry.id = scoped_id(&source.id, &relative, &context);
                            match context_info(
                                &config,
                                entry.clone(),
                                path.clone(),
                                context_key,
                                included,
                            ) {
                                Ok(info) => {
                                    if included {
                                        catalog.targets.insert(
                                            entry.id.clone(),
                                            ConnectionConfig {
                                                config: config.clone(),
                                                context,
                                            },
                                        );
                                        entries.push(entry);
                                    }
                                    status.contexts.push(info);
                                }
                                Err(message) => status.issues.push(Issue {
                                    file: path.clone(),
                                    message,
                                }),
                            }
                        }
                        if config.contexts.is_empty() {
                            status.issues.push(Issue {
                                file: path,
                                message: "No contexts found in this kubeconfig.".into(),
                            });
                        }
                    }
                    Err(message) => status.issues.push(Issue {
                        file: path,
                        message,
                    }),
                }
            }
        }
        catalog.statuses.push(status);
    }
    for status in &catalog.statuses {
        status.location.hash(&mut hasher);
        for issue in &status.issues {
            issue.file.hash(&mut hasher);
            issue.message.hash(&mut hasher);
        }
    }
    catalog.contexts = Contexts::from_entries(entries);
    catalog.input_fingerprint = hasher.finish();
    serde_json::to_vec(preferences)
        .unwrap_or_default()
        .hash(&mut hasher);
    catalog.fingerprint = hasher.finish();
    catalog
}

/// Normalizes references before storing a pasted configuration. A relative
/// reference needs an explicit base directory; parsing never runs exec helpers.
pub fn pasted(text: &str, base: Option<&Path>) -> Result<Kubeconfig, String> {
    if text.len() as u64 > MAX_FILE_BYTES {
        return Err("Kubeconfig exceeds the 4 MiB limit.".into());
    }
    let mut config = Kubeconfig::from_yaml(text).map_err(config_error)?;
    fn resolve(value: &mut Option<String>, base: Option<&Path>) -> Result<(), String> {
        if let Some(path) = value.as_mut()
            && !Path::new(path).is_absolute()
        {
            let base = base.ok_or("This kubeconfig contains relative file references. Choose a base folder or use embedded certificate data.")?;
            *path = expand_path(base)
                .join(&*path)
                .to_string_lossy()
                .into_owned();
        }
        Ok(())
    }
    for cluster in &mut config.clusters {
        if let Some(cluster) = &mut cluster.cluster
            && cluster.certificate_authority_data.is_none()
        {
            resolve(&mut cluster.certificate_authority, base)?;
        }
    }
    for user in &mut config.auth_infos {
        if let Some(user) = &mut user.auth_info {
            if user.client_certificate_data.is_none() {
                resolve(&mut user.client_certificate, base)?;
            }
            if user.client_key_data.is_none() {
                resolve(&mut user.client_key, base)?;
            }
            if user.token.is_none() {
                resolve(&mut user.token_file, base)?;
            }
            if let Some(exec) = &mut user.exec
                && exec
                    .command
                    .as_deref()
                    .is_some_and(|c| c.contains('/') || c.contains('\\'))
            {
                resolve(&mut exec.command, base)?;
            }
        }
    }
    if config.contexts.is_empty() {
        return Err("No contexts found in this kubeconfig.".into());
    }
    Ok(config)
}

/// Prepared off-thread; installation and rollback only rename owner-only files.
pub struct ImportWrite {
    target: PathBuf,
    staged: Option<tempfile::NamedTempFile>,
    backup: Option<tempfile::NamedTempFile>,
    installed: bool,
}
impl ImportWrite {
    pub fn install(&mut self) -> Result<(), String> {
        self.staged
            .take()
            .ok_or("Import was already installed.")?
            .persist(&self.target)
            .map_err(|e| e.error.to_string())?;
        self.installed = true;
        Ok(())
    }
    pub fn rollback(&mut self) -> Result<(), String> {
        if !self.installed {
            return Ok(());
        }
        if let Some(backup) = self.backup.take() {
            if let Err(error) = backup.persist(&self.target) {
                let reason = error.error.to_string();
                let (_, recovery) = error.file.keep().map_err(|e| e.error.to_string())?;
                return Err(format!(
                    "Could not restore the previous kubeconfig: {reason}. Its owner-only backup is at {}.",
                    recovery.display()
                ));
            }
        } else {
            fs::remove_file(&self.target).map_err(|e| e.to_string())?;
        }
        self.installed = false;
        Ok(())
    }
}
/// Owner-only staging for configurations containing credentials.
pub fn stage_import(
    directory: &Path,
    id: &str,
    config: &Kubeconfig,
) -> Result<ImportWrite, String> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("Invalid source identifier.".into());
    }
    let path = imported_path(directory, id);
    let folder = path.parent().unwrap();
    fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(folder).map_err(|e| e.to_string())?;
    use std::io::Write as _;
    let data = serde_json::to_vec(config).map_err(|e| e.to_string())?;
    temporary
        .write_all(&data)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    let backup = if path.exists() {
        let mut backup = tempfile::NamedTempFile::new_in(folder).map_err(|e| e.to_string())?;
        let data = fs::read(&path).map_err(|e| e.to_string())?;
        backup
            .write_all(&data)
            .and_then(|_| backup.as_file().sync_all())
            .map_err(|e| e.to_string())?;
        Some(backup)
    } else {
        None
    };
    Ok(ImportWrite {
        target: path,
        staged: Some(temporary),
        backup,
        installed: false,
    })
}
pub fn save_import(directory: &Path, id: &str, config: &Kubeconfig) -> Result<(), String> {
    stage_import(directory, id, config)?.install()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn yaml(server: &str) -> String {
        format!(
            "apiVersion: v1\nkind: Config\nclusters:\n- name: cluster\n  cluster:\n    server: {server}\ncontexts:\n- name: dev\n  context:\n    cluster: cluster\n    user: user\n    namespace: team\nusers:\n- name: user\n  user:\n    exec:\n      apiVersion: client.authentication.k8s.io/v1\n      command: never-run-this-helper\ncurrent-context: dev\n"
        )
    }
    fn file(id: &str, path: PathBuf) -> Source {
        Source {
            id: id.into(),
            name: id.into(),
            kind: SourceKind::File { path },
            enabled: true,
            excluded: BTreeSet::new(),
        }
    }
    #[test]
    fn same_names_are_isolated_and_keep_the_exact_source_for_connecting() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a.yaml");
        let b = directory.path().join("b.yaml");
        fs::write(&a, yaml("https://a.example.com")).unwrap();
        fs::write(&b, yaml("https://b.example.com")).unwrap();
        let preferences = Preferences {
            system_enabled: false,
            sources: vec![file("a", a.clone()), file("b", b.clone())],
        };
        let catalog = load(&preferences, directory.path(), &[]);
        let entries = catalog.contexts.entries();
        assert_eq!(entries.len(), 2);
        assert_ne!(entries[0].id, entries[1].id);
        assert_ne!(entries[0].id, ClusterId::new(entries[0].id.as_str()));
        for (entry, server) in entries
            .iter()
            .zip(["https://a.example.com", "https://b.example.com"])
        {
            assert_eq!(entry.id.context_name(), "dev");
            assert_eq!(entry.id.display_name(), "dev");
            let target = &catalog.targets[&entry.id];
            assert_eq!(target.context, "dev");
            assert_eq!(
                target.config.clusters[0]
                    .cluster
                    .as_ref()
                    .unwrap()
                    .server
                    .as_deref(),
                Some(server)
            );
        }
        fs::write(&a, yaml("https://new.example.com")).unwrap();
        let new = load(&preferences, directory.path(), &[]);
        assert_ne!(catalog.input_fingerprint, new.input_fingerprint);
        assert_eq!(
            catalog.contexts.entries()[0].id,
            new.contexts.entries()[0].id
        );
        // Existing sessions retain their old Arc snapshot until reconnecting.
        assert_eq!(
            catalog.targets[&entries[0].id].config.clusters[0]
                .cluster
                .as_ref()
                .unwrap()
                .server
                .as_deref(),
            Some("https://a.example.com")
        );
    }
    #[test]
    fn folder_files_are_independent_partial_errors_and_exclusions_do_not_leak() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("configs");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("dev.yaml"), yaml("https://one.example.com")).unwrap();
        fs::write(
            root.join("nested/dev.yaml"),
            yaml("https://two.example.com"),
        )
        .unwrap();
        fs::write(root.join("broken.yaml"), "clusters: [").unwrap();
        fs::write(root.join("ignore.txt"), yaml("https://ignored.example.com")).unwrap();
        let mut source = file("folder", root.clone());
        source.kind = SourceKind::Directory {
            path: root,
            recursive: false,
            patterns: default_patterns(),
        };
        let mut preferences = Preferences {
            system_enabled: false,
            sources: vec![source],
        };
        let flat = load(&preferences, directory.path(), &[]);
        assert_eq!(flat.contexts.entries().len(), 1);
        assert_eq!(flat.statuses[1].issues.len(), 1);
        if let SourceKind::Directory { recursive, .. } = &mut preferences.sources[0].kind {
            *recursive = true;
        }
        let recursive = load(&preferences, directory.path(), &[]);
        assert_eq!(recursive.contexts.entries().len(), 2);
        assert_ne!(
            recursive.contexts.entries()[0].id,
            recursive.contexts.entries()[1].id
        );
        preferences.sources[0]
            .excluded
            .insert(key("dev.yaml", "dev"));
        let filtered = load(&preferences, directory.path(), &[]);
        assert_eq!(filtered.contexts.entries().len(), 1);
        assert_eq!(filtered.statuses[1].contexts.len(), 2);
        assert!(
            filtered
                .labels()
                .values()
                .any(|label| label.contains("nested"))
        );
        assert_eq!(recursive.input_fingerprint, filtered.input_fingerprint);
    }
    #[test]
    fn system_merge_keeps_legacy_ids_and_added_files_survive_a_system_error() {
        let directory = tempfile::tempdir().unwrap();
        let a = directory.path().join("a");
        let b = directory.path().join("b");
        let bad = directory.path().join("bad");
        fs::write(&a, yaml("https://first.example.com")).unwrap();
        fs::write(&b, yaml("https://second.example.com")).unwrap();
        fs::write(&bad, "clusters: [").unwrap();
        let preferences = Preferences {
            system_enabled: true,
            sources: vec![file("extra", a.clone())],
        };
        let catalog = load(&preferences, directory.path(), &[a.clone(), b]);
        assert_eq!(catalog.contexts.entries().len(), 1);
        assert_eq!(catalog.contexts.entries()[0].id, ClusterId::new("dev"));
        assert_eq!(
            catalog.targets[&ClusterId::new("dev")].config.clusters[0]
                .cluster
                .as_ref()
                .unwrap()
                .server
                .as_deref(),
            Some("https://first.example.com")
        );
        assert!(
            catalog.statuses[1].issues[0]
                .message
                .contains("already supplied")
        );
        let catalog = load(&preferences, directory.path(), &[a, bad]);
        assert_eq!(catalog.contexts.entries().len(), 1);
        assert_ne!(catalog.contexts.entries()[0].id, ClusterId::new("dev"));
        assert_eq!(catalog.statuses[0].issues.len(), 1);
        assert!(catalog.statuses[1].issues.is_empty());
    }
    #[test]
    fn references_are_resolved_without_executing_credentials() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config");
        let text = yaml("https://one.example.com")
            .replace(
                "server: https://one.example.com",
                "server: https://one.example.com\n    certificate-authority: ca.pem",
            )
            .replace(
                "command: never-run-this-helper",
                &format!("command: .{}credential-helper", std::path::MAIN_SEPARATOR),
            );
        fs::write(&path, &text).unwrap();
        let catalog = load(
            &Preferences {
                system_enabled: false,
                sources: vec![file("source", path)],
            },
            directory.path(),
            &[],
        );
        assert_eq!(catalog.targets.len(), 1);
        let config = &catalog.targets.values().next().unwrap().config;
        assert_eq!(
            Path::new(
                config.clusters[0]
                    .cluster
                    .as_ref()
                    .unwrap()
                    .certificate_authority
                    .as_ref()
                    .unwrap()
            ),
            directory.path().join("ca.pem")
        );
        assert!(
            pasted(&text, None)
                .unwrap_err()
                .contains("relative file references")
        );
        let pasted = pasted(&text, Some(directory.path())).unwrap();
        assert!(
            Path::new(
                pasted.auth_infos[0]
                    .auth_info
                    .as_ref()
                    .unwrap()
                    .exec
                    .as_ref()
                    .unwrap()
                    .command
                    .as_ref()
                    .unwrap()
            )
            .is_absolute()
        );
    }
    #[test]
    fn import_staging_rolls_back_and_never_changes_linked_files() {
        let directory = tempfile::tempdir().unwrap();
        let old = pasted(&yaml("https://old.example.com"), None).unwrap();
        let new = pasted(&yaml("https://new.example.com"), None).unwrap();
        save_import(directory.path(), "imported", &old).unwrap();
        let path = imported_path(directory.path(), "imported");
        let original = fs::read(&path).unwrap();
        let mut staged = stage_import(directory.path(), "imported", &new).unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        staged.install().unwrap();
        assert_ne!(fs::read(&path).unwrap(), original);
        staged.rollback().unwrap();
        assert_eq!(fs::read(&path).unwrap(), original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(save_import(directory.path(), "../escape", &new).is_err());
        let catalog = load(
            &Preferences {
                system_enabled: false,
                sources: vec![Source {
                    id: "imported".into(),
                    name: "Imported".into(),
                    kind: SourceKind::Imported,
                    enabled: true,
                    excluded: BTreeSet::new(),
                }],
            },
            directory.path(),
            &[],
        );
        assert_eq!(catalog.contexts.entries().len(), 1);
    }
    #[test]
    fn disabled_sources_and_invalid_reference_contexts_are_not_connectable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config");
        fs::write(&path, yaml("https://one.example.com")).unwrap();
        let mut source = file("disabled", path.clone());
        source.enabled = false;
        assert!(
            load(
                &Preferences {
                    system_enabled: false,
                    sources: vec![source]
                },
                directory.path(),
                &[]
            )
            .targets
            .is_empty()
        );
        fs::write(
            &path,
            yaml("https://one.example.com").replace("cluster: cluster", "cluster: missing"),
        )
        .unwrap();
        let catalog = load(
            &Preferences {
                system_enabled: false,
                sources: vec![file("bad", path)],
            },
            directory.path(),
            &[],
        );
        assert!(catalog.targets.is_empty());
        assert!(
            catalog.statuses[1].issues[0]
                .message
                .contains("referenced cluster")
        );
    }
}
