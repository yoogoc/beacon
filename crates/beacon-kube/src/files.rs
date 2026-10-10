//! Container files accessed through exec. No host mounts or shell-interpolated paths.
use crate::{
    ClusterSession, DynamicObject,
    file_exec::{self, Input, Writer},
};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{Arc, atomic::AtomicU64},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const PREVIEW_LIMIT: usize = 256 * 1024;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub namespace: String,
    pub pod: String,
    pub uid: String,
    pub container: String,
    pub container_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Container {
    pub name: String,
    pub group: &'static str,
    pub id: String,
}
pub fn containers(object: &DynamicObject) -> Vec<Container> {
    let mut result = Vec::new();
    for (field, group) in [
        ("containerStatuses", "Containers"),
        ("initContainerStatuses", "Init containers"),
        ("ephemeralContainerStatuses", "Debug containers"),
    ] {
        if let Some(statuses) = object
            .data
            .pointer(&format!("/status/{field}"))
            .and_then(serde_json::Value::as_array)
        {
            for status in statuses {
                if status.pointer("/state/running").is_some()
                    && let (Some(name), Some(id)) =
                        (status["name"].as_str(), status["containerID"].as_str())
                {
                    result.push(Container {
                        name: name.into(),
                        group,
                        id: id.into(),
                    });
                }
            }
        }
    }
    result
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub modified: i64,
    pub permissions: String,
    pub owner: String,
    pub link: Option<String>,
    pub writable: bool,
}
impl Entry {
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
    pub fn directory(&self) -> bool {
        self.kind == "directory"
    }
    pub fn regular(&self) -> bool {
        self.kind.contains("regular")
    }
}
#[derive(Clone, Debug)]
pub struct Listing {
    pub path: String,
    pub entries: Vec<Entry>,
    pub writable: bool,
    pub tar: bool,
}
#[derive(Clone, Debug)]
pub struct Preview {
    pub entry: Entry,
    pub text: Option<String>,
    pub truncated: bool,
    pub checksum: Option<String>,
}
impl Preview {
    pub fn editable(&self) -> bool {
        self.entry.regular()
            && self.entry.link.is_none()
            && self.entry.writable
            && !self.truncated
            && self.text.is_some()
            && self.checksum.is_some()
    }
}
#[derive(Clone)]
pub struct Browser {
    pub session: Arc<ClusterSession>,
    pub target: Target,
}

/// Canonical lexical path; no expansion of ~, variables or glob patterns.
pub fn path(value: &str) -> Result<String, String> {
    if !value.starts_with('/') || value.contains('\0') {
        return Err("Enter an absolute container path without NUL characters.".into());
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    Ok(format!("/{}", parts.join("/")))
}
/// Keep a container filename inside the selected host folder, including on Windows.
pub fn local_filename(name: &str) -> String {
    let safe = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    let safe = safe.trim_end_matches([' ', '.']);
    let stem = safe.split('.').next().unwrap_or("").to_ascii_uppercase();
    if safe.is_empty() {
        "file".into()
    } else if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
    {
        format!("file_{safe}")
    } else {
        safe.into()
    }
}
pub fn child(directory: &str, name: &str) -> Result<String, String> {
    if name.is_empty() || matches!(name, "." | "..") || name.contains(['/', '\0']) {
        return Err("Use a single file or directory name.".into());
    }
    path(&format!("{}/{name}", directory.trim_end_matches('/')))
}
fn script(script: &str, args: &[String]) -> Vec<String> {
    [
        vec![
            "sh".into(),
            "-c".into(),
            format!("export LC_ALL=C\n{script}"),
            "beacon-files".into(),
        ],
        args.to_vec(),
    ]
    .concat()
}

// NUL-separated fields preserve spaces, tabs, newlines and shell metacharacters.
const STAT: &str = r#"entry() {
 p=$1
 attrs=$(stat -c '%F|%s|%Y|%a|%u:%g' -- "$p") || return
 IFS='|' read -r kind size modified mode owner <<END
$attrs
END
 writable=0; if [ -w "$p" ]; then writable=1; fi
 printf '%s\000%s\000%s\000%s\000%s\000%s\000' "$p" "$kind" "$size" "$modified" "$mode" "$owner"
 if [ -L "$p" ]; then readlink -n -- "$p" || return; fi
 printf '\000%s\000' "$writable"
}
"#;
const CHECK_TOOLS: &str = r#"for tool in stat head cat readlink cksum wc; do command -v "$tool" >/dev/null || { echo "File browsing requires sh, stat (GNU/BusyBox), head, cat, readlink, cksum and wc. Missing: $tool" >&2; exit 1; }; done
stat -c '%s' / >/dev/null || { echo 'The container stat command must support GNU/BusyBox -c format.' >&2; exit 1; }
"#;

impl Browser {
    async fn verify(&self, write_path: Option<&str>) -> Result<Pod, String> {
        let pod = Api::<Pod>::namespaced(self.session.client().clone(), &self.target.namespace)
            .get(&self.target.pod)
            .await
            .map_err(|e| crate::error::diagnose(&e))?;
        if self.target.uid.is_empty()
            || pod.metadata.uid.as_deref() != Some(&self.target.uid)
            || pod.metadata.deletion_timestamp.is_some()
        {
            return Err(
                "The original Pod no longer exists. Reopen Files on the current Pod.".into(),
            );
        }
        let statuses = pod.status.as_ref().into_iter().flat_map(|s| {
            s.container_statuses
                .iter()
                .flatten()
                .chain(s.init_container_statuses.iter().flatten())
                .chain(s.ephemeral_container_statuses.iter().flatten())
        });
        if !statuses.into_iter().any(|s| {
            s.name == self.target.container
                && s.container_id.as_deref() == Some(&self.target.container_id)
                && s.state.as_ref().is_some_and(|s| s.running.is_some())
        }) {
            return Err(
                "The container stopped or restarted. Reopen Files to use its current filesystem."
                    .into(),
            );
        }
        if let Some(path) = write_path
            && read_only(&pod, &self.target.container, path)
        {
            return Err("This path is on a read-only volume mount.".into());
        }
        Ok(pod)
    }
    async fn run(
        &self,
        code: &str,
        args: &[String],
        input: Option<Input>,
        output: &mut Writer,
        limit: u64,
        progress: Arc<AtomicU64>,
    ) -> Result<u64, String> {
        file_exec::run(
            &self.session,
            &self.target,
            &script(code, args),
            input,
            output,
            limit,
            &progress,
        )
        .await
    }
    async fn collect(
        &self,
        code: &str,
        args: &[String],
        input: Option<Input>,
    ) -> Result<Vec<u8>, String> {
        // Duplex backpressure keeps the transport bounded; the consumer also has a hard cap.
        let (writer, mut reader) = tokio::io::duplex(65536);
        let mut writer: Writer = Box::new(writer);
        let read = async {
            let mut bytes = Vec::new();
            reader
                .read_to_end(&mut bytes)
                .await
                .map_err(|e| e.to_string())?;
            Ok::<_, String>(bytes)
        };
        // Drop writer as soon as run finishes so the reader sees EOF.
        let run = async {
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(60),
                self.run(
                    code,
                    args,
                    input,
                    &mut writer,
                    file_exec::LIMIT,
                    Arc::new(AtomicU64::new(0)),
                ),
            )
            .await
            .map_err(|_| "Browsing or saving timed out after 60s; operation cancelled.".to_string())
            .and_then(|r| r);
            drop(writer);
            result
        };
        let (_, bytes) = tokio::try_join!(run, read)?;
        Ok(bytes)
    }
    async fn resolve(&self, value: &str) -> Result<String, String> {
        let value = path(value)?;
        if value == "/" {
            return Ok(value);
        }
        self.verify(None).await?;
        let bytes = self.collect(r#"p=$1; parent=${p%/*}; [ -n "$parent" ] || parent=/; leaf=${p##*/}; cd -- "$parent" || exit; printf '%s/%s' "$(pwd -P)" "$leaf""#, &[value], None).await?;
        path(std::str::from_utf8(&bytes).map_err(|_| "Path is not UTF-8")?)
    }
    pub async fn list(&self, directory: &str) -> Result<Listing, String> {
        let directory = path(directory)?;
        let pod = self.verify(None).await?;
        let code = format!(
            "{CHECK_TOOLS}{STAT}\ncd -- \"$1\" || exit; physical=$(pwd -P); printf '%s\\000' \"$physical\"; [ -w . ] && printf '1\\000' || printf '0\\000'; command -v tar >/dev/null && printf '1\\000' || printf '0\\000'; for p in \"$physical\"/* \"$physical\"/.[!.]* \"$physical\"/..?*; do [ -e \"$p\" ] || [ -L \"$p\" ] || continue; entry \"$p\" || exit; done"
        );
        let bytes = self.collect(&code, &[directory], None).await?;
        let fields = fields(&bytes)?;
        if fields.len() < 3 {
            return Err("Invalid directory response".into());
        }
        let physical = path(fields[0])?;
        let mut entries = parse_entries(&fields[3..])?;
        for entry in &mut entries {
            entry.writable &= !read_only(&pod, &self.target.container, &entry.path);
        }
        entries.sort_by(|a, b| {
            b.directory()
                .cmp(&a.directory())
                .then_with(|| a.name().cmp(b.name()))
        });
        Ok(Listing {
            writable: fields[1] == "1" && !read_only(&pod, &self.target.container, &physical),
            tar: fields[2] == "1",
            path: physical,
            entries,
        })
    }
    pub async fn preview(&self, file: &str) -> Result<Preview, String> {
        let file = self.resolve(file).await?;
        let pod = self.verify(None).await?;
        let code = format!(
            "{STAT}entry \"$1\" || exit; if [ -f \"$1\" ] && [ ! -L \"$1\" ]; then size=$(stat -c '%s' -- \"$1\") || exit; if [ \"$size\" -le {PREVIEW_LIMIT} ]; then head -c {} -- \"$1\" | cksum || exit; else printf '\\n'; fi; head -c {} -- \"$1\"; fi",
            PREVIEW_LIMIT + 1,
            PREVIEW_LIMIT + 1
        );
        let bytes = self.collect(&code, &[file], None).await?;
        let mut fields = bytes.splitn(9, |b| *b == 0);
        let metadata = (0..8)
            .map(|_| {
                fields
                    .next()
                    .ok_or("Incomplete file metadata")
                    .and_then(|b| std::str::from_utf8(b).map_err(|_| "Filename is not UTF-8"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut entry = parse_entries(&metadata)?.remove(0);
        entry.writable &= !read_only(&pod, &self.target.container, &entry.path);
        let content = fields.next().unwrap_or_default();
        let (checksum, content) = if entry.regular() && entry.link.is_none() {
            let newline = content
                .iter()
                .position(|b| *b == b'\n')
                .ok_or("Missing file checksum")?;
            (
                std::str::from_utf8(&content[..newline])
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                &content[newline + 1..],
            )
        } else {
            (None, &[][..])
        };
        let truncated = content.len() > PREVIEW_LIMIT || entry.size > PREVIEW_LIMIT as u64;
        let content = &content[..content.len().min(PREVIEW_LIMIT)];
        let text = entry
            .regular()
            .then(|| {
                std::str::from_utf8(content)
                    .or_else(|error| {
                        if truncated && error.error_len().is_none() {
                            std::str::from_utf8(&content[..error.valid_up_to()])
                        } else {
                            Err(error)
                        }
                    })
                    .ok()
                    .filter(|s| !s.contains('\0'))
                    .map(str::to_owned)
            })
            .flatten();
        Ok(Preview {
            entry,
            text,
            truncated,
            checksum,
        })
    }
    pub async fn save(&self, preview: &Preview, text: String) -> Result<(), String> {
        if !preview.editable() {
            return Err(
                "This preview cannot be edited; download the complete file instead.".into(),
            );
        }
        let resolved = self.resolve(&preview.entry.path).await?;
        if resolved != preview.entry.path {
            return Err("The file location changed. Refresh before saving.".into());
        }
        self.verify(Some(&resolved)).await?;
        let args = vec![
            preview.entry.path.clone(),
            text.len().to_string(),
            preview.checksum.clone().unwrap(),
            "1".into(),
        ];
        self.collect(
            WRITE,
            &args,
            Some(Input {
                size: text.len() as u64,
                reader: Box::new(std::io::Cursor::new(text.into_bytes())),
            }),
        )
        .await?;
        Ok(())
    }
    pub async fn mkdir(&self, directory: &str, name: &str) -> Result<(), String> {
        let destination = self.resolve(&child(directory, name)?).await?;
        self.verify(Some(&destination)).await?;
        self.collect("mkdir -- \"$1\"", &[destination], None)
            .await?;
        Ok(())
    }
    pub async fn rename(&self, source: &str, name: &str) -> Result<(), String> {
        let source = self.resolve(source).await?;
        protect(&source)?;
        let parent = source.rsplit_once('/').unwrap().0;
        let destination = child(if parent.is_empty() { "/" } else { parent }, name)?;
        self.verify(Some(&source)).await?;
        self.verify(Some(&destination)).await?;
        self.collect("[ ! -e \"$2\" ] && [ ! -L \"$2\" ] || { echo 'Destination already exists' >&2; exit 1; }; mv -T -n -- \"$1\" \"$2\"; [ ! -e \"$1\" ] && [ ! -L \"$1\" ] || { echo 'Rename was refused; destination exists' >&2; exit 1; }", &[source, destination], None).await?;
        Ok(())
    }
    pub async fn delete(&self, files: &[String]) -> Result<(), String> {
        if files.is_empty() {
            return Err("Select files to delete".into());
        }
        let mut paths = Vec::new();
        for file in files {
            let file = self.resolve(file).await?;
            protect(&file)?;
            self.verify(Some(&file)).await?;
            paths.push(file);
        }
        self.collect("rm -rf -- \"$@\"", &paths, None).await?;
        Ok(())
    }
    pub async fn download(
        &self,
        files: &[String],
        archive: bool,
        destination: PathBuf,
        progress: Arc<AtomicU64>,
    ) -> Result<u64, String> {
        self.verify(None).await?;
        if files.is_empty() {
            return Err("Select files to download".into());
        }
        let paths = files
            .iter()
            .map(|s| path(s))
            .collect::<Result<Vec<_>, _>>()?;
        let code = if archive {
            "command -v tar >/dev/null || { echo 'Directory and multi-file downloads require tar in the container.' >&2; exit 1; }; cd / || exit; tar -cf - -- \"$@\""
        } else {
            "[ -f \"$1\" ] && [ ! -L \"$1\" ] || { echo 'Only regular files can be downloaded directly; use an archive for directories and links.' >&2; exit 1; }; cat -- \"$1\""
        };
        let args = if archive {
            paths
                .iter()
                .map(|p| {
                    if p == "/" {
                        ".".into()
                    } else {
                        p.trim_start_matches('/').to_string()
                    }
                })
                .collect::<Vec<_>>()
        } else {
            paths
        };
        let temporary = tempfile::NamedTempFile::new_in(
            destination.parent().ok_or("Choose a destination folder")?,
        )
        .map_err(|e| e.to_string())?;
        let mut output: Writer = Box::new(tokio::fs::File::from_std(
            temporary.reopen().map_err(|e| e.to_string())?,
        ));
        let bytes = self
            .run(code, &args, None, &mut output, u64::MAX, progress)
            .await?;
        output.shutdown().await.map_err(|e| e.to_string())?;
        drop(output);
        // Never overwrite an unrelated local file; persist only a complete transfer.
        temporary
            .persist_noclobber(destination)
            .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
    pub async fn upload(
        &self,
        local: PathBuf,
        destination: &str,
        overwrite: bool,
        progress: Arc<AtomicU64>,
    ) -> Result<(), String> {
        let destination = self.resolve(destination).await?;
        protect(&destination)?;
        self.verify(Some(&destination)).await?;
        let input = tokio::fs::File::open(local)
            .await
            .map_err(|e| e.to_string())?;
        let metadata = input.metadata().await.map_err(|e| e.to_string())?;
        if !metadata.is_file() {
            return Err(
                "Choose a regular local file. Directories can be downloaded as tar archives."
                    .into(),
            );
        }
        let args = vec![
            destination,
            metadata.len().to_string(),
            String::new(),
            if overwrite { "1" } else { "0" }.into(),
        ];
        let mut output: Writer = Box::new(tokio::io::sink());
        self.run(
            WRITE,
            &args,
            Some(Input {
                size: metadata.len(),
                reader: Box::new(input),
            }),
            &mut output,
            file_exec::LIMIT,
            progress,
        )
        .await?;
        Ok(())
    }
}

const WRITE: &str = r#"set -eu
p=$1; expected_size=$2; expected_checksum=$3; overwrite=$4
[ ! -L "$p" ] || { echo 'Open the symlink target explicitly before editing.' >&2; exit 1; }
[ ! -e "$p" ] || [ -f "$p" ] || { echo 'Destination is not a regular file.' >&2; exit 1; }
[ "$overwrite" = 1 ] || { [ ! -e "$p" ] || { echo 'Destination already exists; confirm overwrite first.' >&2; exit 1; }; }
dir=${p%/*}; [ -n "$dir" ] || dir=/
tmp=$(mktemp "$dir/.beacon-upload-XXXXXX")
trap 'rm -f -- "$tmp"' EXIT HUP INT TERM
if [ -e "$p" ]; then cp -p -- "$p" "$tmp"; fi
head -c "$expected_size" > "$tmp"
[ "$(wc -c < "$tmp" | tr -d ' ')" = "$expected_size" ] || { echo 'Incomplete transfer; original file preserved.' >&2; exit 1; }
[ -z "$expected_checksum" ] || { [ "$(head -c 262145 -- "$p" | cksum)" = "$expected_checksum" ] || { echo 'File changed since preview. Refresh before saving.' >&2; exit 1; }; }
[ ! -L "$p" ] || exit 1
[ "$overwrite" = 1 ] || { [ ! -e "$p" ] || { echo 'Destination appeared during upload; original preserved.' >&2; exit 1; }; }
mv -T -- "$tmp" "$p"
"#;
fn protect(path: &str) -> Result<(), String> {
    if path == "/" {
        Err("The container root cannot be modified.".into())
    } else {
        Ok(())
    }
}
fn fields(bytes: &[u8]) -> Result<Vec<&str>, String> {
    let mut fields = bytes
        .split(|b| *b == 0)
        .map(|b| {
            std::str::from_utf8(b)
                .map_err(|_| "This directory contains a filename that is not UTF-8.".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if fields.last() == Some(&"") {
        fields.pop();
    }
    Ok(fields)
}
fn parse_entries(fields: &[&str]) -> Result<Vec<Entry>, String> {
    if !fields.len().is_multiple_of(8) {
        return Err("Incomplete file listing; refresh and try again.".into());
    }
    fields
        .chunks(8)
        .map(|f| {
            Ok(Entry {
                path: path(f[0])?,
                kind: f[1].into(),
                size: f[2].parse().map_err(|_| "Invalid file size")?,
                modified: f[3].parse().map_err(|_| "Invalid file timestamp")?,
                permissions: f[4].into(),
                owner: f[5].into(),
                link: (!f[6].is_empty()).then(|| f[6].into()),
                writable: f[7] == "1",
            })
        })
        .collect()
}
fn read_only(pod: &Pod, container: &str, path: &str) -> bool {
    let Some(spec) = &pod.spec else {
        return true;
    };
    let projected: BTreeSet<_> = spec
        .volumes
        .iter()
        .flatten()
        .filter(|v| {
            v.config_map.is_some()
                || v.secret.is_some()
                || v.projected.is_some()
                || v.downward_api.is_some()
        })
        .map(|v| v.name.as_str())
        .collect();
    let container = spec
        .containers
        .iter()
        .chain(spec.init_containers.iter().flatten())
        .find(|c| c.name == container)
        .map(|c| (&c.volume_mounts, &c.security_context))
        .or_else(|| {
            spec.ephemeral_containers
                .iter()
                .flatten()
                .find(|c| c.name == container)
                .map(|c| (&c.volume_mounts, &c.security_context))
        });
    let Some((mounts, security)) = container else {
        return true;
    };
    let mount = mounts
        .iter()
        .flatten()
        .filter(|m| {
            path == m.mount_path
                || path.starts_with(&format!("{}/", m.mount_path.trim_end_matches('/')))
        })
        .max_by_key(|m| m.mount_path.len());
    mount.map_or_else(
        || {
            security
                .as_ref()
                .is_some_and(|s| s.read_only_root_filesystem == Some(true))
        },
        |m| m.read_only == Some(true) || projected.contains(m.name.as_str()),
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        ClusterId,
        test_support::{FileFixture, Fixture},
    };
    async fn setup() -> (FileFixture, Fixture, Browser) {
        let files = FileFixture::new();
        let fixture = Fixture::start(vec![files.pod()]).await;
        fixture.refuse_exec();
        let session = Arc::new(
            ClusterSession::for_testing(
                ClusterId::new("isolated-files"),
                fixture.url.clone(),
                vec![],
            )
            .with_file_test_program(files.program.clone()),
        );
        let browser = Browser {
            session,
            target: Target {
                namespace: "default".into(),
                pod: "file-pod".into(),
                uid: "file-uid".into(),
                container: "app".into(),
                container_id: "fixture://app".into(),
            },
        };
        (files, fixture, browser)
    }
    #[tokio::test]
    async fn list_preview_save_conflict_and_safe_filenames() {
        let (files, _fixture, browser) = setup().await;
        let strange = "quote' ; $(touch nope)\n配置.txt";
        std::fs::write(files.root.join(strange), "before\n").unwrap();
        std::fs::create_dir(files.root.join("folder")).unwrap();
        std::os::unix::fs::symlink(strange, files.root.join("link")).unwrap();
        let listing = browser.list("/").await.unwrap();
        assert_eq!(listing.entries.len(), 3);
        assert!(listing.entries[0].directory());
        let entry = listing
            .entries
            .iter()
            .find(|e| e.name() == strange)
            .unwrap();
        let preview = browser.preview(&entry.path).await.unwrap();
        assert!(preview.editable());
        assert_eq!(preview.text.as_deref(), Some("before\n"));
        browser.save(&preview, "after\n".into()).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(files.root.join(strange)).unwrap(),
            "after\n"
        );
        assert!(
            browser
                .save(&preview, "overwrite\n".into())
                .await
                .unwrap_err()
                .contains("changed")
        );
        let link = browser
            .preview(files.root.join("link").to_str().unwrap())
            .await
            .unwrap();
        assert!(!link.editable());
        assert_eq!(link.entry.link.as_deref(), Some(strange));
        assert!(!files.root.join("nope").exists());
    }
    #[tokio::test]
    async fn transfers_preserve_binary_and_downloads_never_clobber() {
        let (files, _fixture, browser) = setup().await;
        let bytes = (0..=255).cycle().take(3 * 1024 * 1024).collect::<Vec<u8>>();
        let local = tempfile::tempdir().unwrap();
        let input = local.path().join("input.bin");
        std::fs::write(&input, &bytes).unwrap();
        let destination = files.root.join("file.bin");
        browser
            .upload(
                input.clone(),
                destination.to_str().unwrap(),
                false,
                Arc::new(AtomicU64::new(0)),
            )
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);
        let preview = browser
            .preview(destination.to_str().unwrap())
            .await
            .unwrap();
        assert!(preview.text.is_none());
        assert!(preview.truncated);
        assert!(!preview.editable());
        assert!(
            browser
                .upload(
                    input,
                    destination.to_str().unwrap(),
                    false,
                    Arc::new(AtomicU64::new(0))
                )
                .await
                .is_err()
        );
        let output = local.path().join("download.bin");
        let count = browser
            .download(
                &[destination.to_str().unwrap().into()],
                false,
                output.clone(),
                Arc::new(AtomicU64::new(0)),
            )
            .await
            .unwrap();
        assert_eq!(count, bytes.len() as u64);
        assert_eq!(std::fs::read(&output).unwrap(), bytes);
        std::fs::write(&output, b"keep").unwrap();
        assert!(
            browser
                .download(
                    &[destination.to_str().unwrap().into()],
                    false,
                    output.clone(),
                    Arc::new(AtomicU64::new(0))
                )
                .await
                .is_err()
        );
        assert_eq!(std::fs::read(output).unwrap(), b"keep");
        assert!(
            local.path().read_dir().unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp"))
        );
    }
    #[tokio::test]
    async fn mutations_pin_identity_and_protect_projected_mounts_and_symlink_parents() {
        let (files, fixture, browser) = setup().await;
        let mount = files.root.join("config");
        std::fs::create_dir(&mount).unwrap();
        std::fs::write(mount.join("secret"), b"original").unwrap();
        std::os::unix::fs::symlink(&mount, files.root.join("alias")).unwrap();
        let mut pod = files.pod();
        pod["spec"]["volumes"] = serde_json::json!([{"name":"config","configMap":{"name":"test"}}]);
        pod["spec"]["containers"][0]["volumeMounts"] =
            serde_json::json!([{"name":"config","mountPath":mount.to_str().unwrap()}]);
        fixture.put(pod.clone());
        let p = browser
            .preview(files.root.join("alias/secret").to_str().unwrap())
            .await
            .unwrap();
        assert!(!p.editable());
        assert!(
            browser
                .mkdir(mount.to_str().unwrap(), "new")
                .await
                .unwrap_err()
                .contains("read-only")
        );
        assert!(
            browser
                .delete(&[files.root.join("alias/secret").to_str().unwrap().into()])
                .await
                .unwrap_err()
                .contains("read-only")
        );
        pod["metadata"]["uid"] = serde_json::json!("replacement");
        fixture.put(pod.clone());
        let calls = files.calls();
        assert!(
            browser
                .list("/")
                .await
                .unwrap_err()
                .contains("original Pod")
        );
        assert_eq!(calls, files.calls());
        pod["metadata"]["uid"] = serde_json::json!("file-uid");
        pod["status"]["containerStatuses"][0]["containerID"] = serde_json::json!("restarted");
        fixture.put(pod);
        assert!(browser.list("/").await.unwrap_err().contains("restarted"));
    }
    #[tokio::test]
    async fn mkdir_rename_multidelete_and_tar_download() {
        let (files, _fixture, browser) = setup().await;
        let root = files.root.to_str().unwrap();
        browser.mkdir(root, "first").await.unwrap();
        std::fs::write(files.root.join("first/a.txt"), "hello").unwrap();
        browser
            .rename(files.root.join("first").to_str().unwrap(), "renamed")
            .await
            .unwrap();
        std::fs::write(files.root.join("second.txt"), "second").unwrap();
        let targets = vec![
            files.root.join("renamed").to_str().unwrap().into(),
            files.root.join("second.txt").to_str().unwrap().into(),
        ];
        let local = tempfile::tempdir().unwrap();
        let archive = local.path().join("files.tar");
        browser
            .download(&targets, true, archive.clone(), Arc::new(AtomicU64::new(0)))
            .await
            .unwrap();
        let listing = tokio::process::Command::new("tar")
            .args(["-tf"])
            .arg(archive)
            .output()
            .await
            .unwrap();
        assert!(listing.status.success());
        assert!(String::from_utf8_lossy(&listing.stdout).contains("renamed/a.txt"));
        browser.delete(&targets).await.unwrap();
        assert!(files.root.read_dir().unwrap().next().is_none());
        assert!(browser.delete(&["/".into()]).await.is_err());
    }
    #[test]
    fn canonical_paths_do_not_expand_shell_input() {
        assert_eq!(path("/etc/../tmp//a$HOME").unwrap(), "/tmp/a$HOME");
        assert!(path("relative").is_err());
        assert!(child("/", "../bad").is_err());
        assert!(path("/a\0b").is_err());
        assert_eq!(
            child("/tmp", "-strange\n'$(hello)").unwrap(),
            "/tmp/-strange\n'$(hello)"
        );
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;
    #[test]
    fn download_names_stay_inside_host_folders_on_all_platforms() {
        assert_eq!(
            local_filename("C:\\Windows\\file:bad.txt"),
            "C__Windows_file_bad.txt"
        );
        assert_eq!(local_filename("CON.txt"), "file_CON.txt");
        assert_eq!(local_filename("../../secret"), ".._.._secret");
        assert_eq!(local_filename("配置.txt"), "配置.txt");
        assert_eq!(local_filename(".."), "file");
    }
    #[test]
    fn longest_mount_wins_over_readonly_root_and_parent_mounts() {
        let pod: Pod=serde_json::from_value(serde_json::json!({"spec":{"containers":[{"name":"app","image":"test","securityContext":{"readOnlyRootFilesystem":true},"volumeMounts":[{"name":"ro","mountPath":"/data","readOnly":true},{"name":"rw","mountPath":"/data/cache"}]}]}})).unwrap();
        assert!(read_only(&pod, "app", "/etc/config"));
        assert!(read_only(&pod, "app", "/data/config"));
        assert!(!read_only(&pod, "app", "/data/cache/file"));
    }
}
