//! Helm releases.
//!
//! Read out of the cluster, not out of the `helm` binary. The design notes
//! recommended shelling out, and this goes the other way for two reasons that
//! only became clear once the rest of the client existed: Beacon already has a
//! working, authenticated client for this cluster, and shelling out would
//! reintroduce exactly the `PATH` problem that §5 exists to solve — a GUI
//! launched from Finder cannot find `helm` either.
//!
//! A release is a Secret of type `helm.sh/release.v1`, one per revision, named
//! `sh.helm.release.v1.<release>.v<revision>`. Its payload is **base64 twice**
//! — Helm encodes the gzipped JSON, and the API encodes the Secret value on
//! top of that — then gzip, then JSON. Decoding once gets you a string
//! beginning `H4sI`, which is what base64-encoded gzip looks like, and is the
//! shape of this bug when it is present.

use std::collections::HashMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use kube::{
    Api,
    api::{ApiResource, DynamicObject, ListParams},
    core::GroupVersionKind,
};
use serde_json::Value;

use crate::Result;

/// The Secret type Helm writes.
const RELEASE_TYPE: &str = "helm.sh/release.v1";

/// One release, at its latest revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub name: String,
    pub namespace: String,
    pub revision: i64,
    /// `deployed`, `failed`, `superseded`, `pending-upgrade`...
    pub status: String,
    /// `argo-workflows-0.45.0`, as `helm list` prints it.
    pub chart: String,
    /// The version of the thing the chart installs, which is usually what
    /// somebody is actually asking about.
    pub app_version: String,
    /// RFC3339, as Helm wrote it.
    pub updated: Option<String>,
}

/// A release identity stays stable while its active revision changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub namespace: String,
}

impl From<&Release> for Identity {
    fn from(release: &Release) -> Self {
        Self {
            name: release.name.clone(),
            namespace: release.namespace.clone(),
        }
    }
}

/// The stored configuration and rendered resources for one immutable revision.
/// These are read from Helm storage, including releases created elsewhere.
#[derive(Debug, Clone, PartialEq)]
pub struct Revision {
    pub summary: Release,
    pub chart_name: String,
    pub chart_version: String,
    pub first_deployed: Option<String>,
    pub description: String,
    pub values: Value,
    pub manifest: String,
    pub notes: String,
}

/// Preserve unreadable revisions in the history instead of silently hiding them.
#[derive(Debug, Clone, PartialEq)]
pub struct RevisionRecord {
    pub number: i64,
    pub secret_name: String,
    pub revision: Option<Revision>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct History {
    pub identity: Identity,
    /// Newest first; revision gaps are normal when Helm prunes old history.
    pub revisions: Vec<RevisionRecord>,
}

/// All stored revisions for one release. No local Helm executable is required.
pub async fn history(client: &kube::Client, identity: &Identity) -> Result<History> {
    if identity.namespace.is_empty()
        || identity.name.is_empty()
        || !identity
            .name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
    {
        return Err(crate::Error::Manifest(
            "Choose a valid Helm release and namespace.".into(),
        ));
    }
    let resource =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk("", "v1", "Secret"), "secrets");
    let api: Api<DynamicObject> =
        Api::namespaced_with(client.clone(), &identity.namespace, &resource);
    let params = ListParams::default()
        .fields(&format!("type={RELEASE_TYPE}"))
        .labels(&format!("owner=helm,name={}", identity.name));
    let secrets = api.list(&params).await?;
    Ok(stored_history(identity, secrets.items.iter()))
}

fn stored_history<'a>(
    identity: &Identity,
    secrets: impl Iterator<Item = &'a DynamicObject>,
) -> History {
    let mut revisions: Vec<_> = secrets
        .filter(|secret| {
            secret.data["type"] == RELEASE_TYPE
                && secret.metadata.namespace.as_deref() == Some(identity.namespace.as_str())
                && (secret
                    .metadata
                    .labels
                    .as_ref()
                    .and_then(|labels| labels.get("name"))
                    == Some(&identity.name)
                    || secret.metadata.name.as_deref().is_some_and(|name| {
                        name.starts_with(&format!("sh.helm.release.v1.{}.v", identity.name))
                    }))
        })
        .map(|secret| {
            let name = secret.metadata.name.clone().unwrap_or_default();
            let number = secret
                .metadata
                .labels
                .as_ref()
                .and_then(|labels| labels.get("version"))
                .and_then(|number| number.parse().ok())
                .or_else(|| {
                    name.rsplit_once(".v")
                        .and_then(|(_, number)| number.parse().ok())
                })
                .unwrap_or(0);
            match read_revision(secret, identity) {
                Ok(revision) => RevisionRecord {
                    number: revision.summary.revision,
                    secret_name: name,
                    revision: Some(revision),
                    error: None,
                },
                Err(error) => RevisionRecord {
                    number,
                    secret_name: name,
                    revision: None,
                    error: Some(error),
                },
            }
        })
        .collect();
    revisions.sort_by(|a, b| {
        b.number
            .cmp(&a.number)
            .then_with(|| a.secret_name.cmp(&b.secret_name))
    });
    History {
        identity: identity.clone(),
        revisions,
    }
}

fn read_revision(
    secret: &DynamicObject,
    identity: &Identity,
) -> std::result::Result<Revision, String> {
    let payload = secret
        .data
        .pointer("/data/release")
        .and_then(Value::as_str)
        .ok_or_else(|| "This revision has no release payload.".to_string())?;
    let value = decode(payload).ok_or_else(|| {
        "This revision's release payload is damaged or has an unsupported encoding.".to_string()
    })?;
    let summary = summary(secret, &value)
        .ok_or_else(|| "This revision is missing its release or chart identity.".to_string())?;
    if summary.name != identity.name
        || summary.namespace != identity.namespace
        || summary.revision < 1
    {
        return Err(
            "The stored payload does not match this release or has an invalid revision.".into(),
        );
    }
    if secret
        .metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get("version"))
        .and_then(|version| version.parse::<i64>().ok())
        .is_some_and(|number| number != summary.revision)
    {
        return Err("The stored revision label does not match its payload.".into());
    }
    let manifest = match value.get("manifest") {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(manifest)) => manifest.clone(),
        _ => return Err("This revision has an invalid rendered manifest.".into()),
    };
    let values = value
        .get("config")
        .filter(|value| !value.is_null())
        .cloned()
        .unwrap_or_else(|| Value::Object(Default::default()));
    if !values.is_object() {
        return Err("This revision has invalid Helm values; a mapping was expected.".into());
    }
    Ok(Revision {
        chart_name: value
            .pointer("/chart/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        chart_version: value
            .pointer("/chart/metadata/version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into(),
        first_deployed: text(value.get("info"), "first_deployed"),
        description: text(value.get("info"), "description").unwrap_or_default(),
        notes: text(value.get("info"), "notes").unwrap_or_default(),
        summary,
        values,
        manifest,
    })
}

/// Every release in the cluster, newest revision only.
///
/// Helm keeps one Secret per revision, so a chart upgraded twenty times has
/// twenty Secrets. `helm list` shows the latest, and so does this.
pub async fn list(client: &kube::Client, namespace: Option<&str>) -> Result<Vec<Release>> {
    let resource =
        ApiResource::from_gvk_with_plural(&GroupVersionKind::gvk("", "v1", "Secret"), "secrets");

    let api: Api<DynamicObject> = match namespace {
        Some(namespace) => Api::namespaced_with(client.clone(), namespace, &resource),
        None => Api::all_with(client.clone(), &resource),
    };

    // Filtering server-side keeps this from listing every Secret in the
    // cluster, which on a large one is a lot of bytes for a short list.
    let params = ListParams::default().fields(&format!("type={RELEASE_TYPE}"));
    let secrets = api.list(&params).await?;

    Ok(latest(secrets.items.iter().filter_map(|secret| {
        read(secret).or_else(|| unreadable_summary(secret))
    })))
}

/// A damaged newest revision still needs an entry so its history can be opened.
fn unreadable_summary(secret: &DynamicObject) -> Option<Release> {
    if secret.data["type"] != RELEASE_TYPE {
        return None;
    }
    let labels = secret.metadata.labels.as_ref()?;
    let name = labels.get("name")?.clone();
    let revision = labels.get("version")?.parse::<i64>().ok()?;
    if revision < 1 || name.is_empty() {
        return None;
    }
    Some(Release {
        name,
        namespace: secret.metadata.namespace.clone().unwrap_or_default(),
        revision,
        status: "unreadable".into(),
        chart: "Unreadable revision".into(),
        app_version: String::new(),
        updated: None,
    })
}

/// Keeps the newest revision of each release.
fn latest(releases: impl Iterator<Item = Release>) -> Vec<Release> {
    let mut newest: HashMap<(String, String), Release> = HashMap::new();

    for release in releases {
        let key = (release.namespace.clone(), release.name.clone());
        match newest.get(&key) {
            Some(existing) if existing.revision >= release.revision => {}
            _ => {
                newest.insert(key, release);
            }
        }
    }

    let mut releases: Vec<Release> = newest.into_values().collect();
    releases
        .sort_by(|left, right| (&left.namespace, &left.name).cmp(&(&right.namespace, &right.name)));
    releases
}

/// Reads one release Secret.
fn read(secret: &DynamicObject) -> Option<Release> {
    let payload = secret.data.get("data")?.get("release")?.as_str()?;
    let release = decode(payload)?;

    summary(secret, &release)
}

fn summary(secret: &DynamicObject, release: &Value) -> Option<Release> {
    let info = release.get("info");
    let metadata = release.get("chart").and_then(|chart| chart.get("metadata"));

    let chart_name = text(metadata, "name")?;
    let chart_version = text(metadata, "version").unwrap_or_default();

    Some(Release {
        name: release.get("name")?.as_str()?.to_string(),
        namespace: release
            .get("namespace")
            .and_then(Value::as_str)
            .or(secret.metadata.namespace.as_deref())
            .unwrap_or_default()
            .to_string(),
        revision: release.get("version").and_then(Value::as_i64).unwrap_or(0),
        status: text(info, "status").unwrap_or_else(|| "unknown".to_string()),
        chart: if chart_version.is_empty() {
            chart_name
        } else {
            format!("{chart_name}-{chart_version}")
        },
        app_version: text(metadata, "appVersion").unwrap_or_default(),
        updated: text(info, "last_deployed"),
    })
}

fn text(node: Option<&Value>, field: &str) -> Option<String> {
    node?
        .get(field)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Unwraps Helm's payload: base64 until it stops being base64, then gzip when
/// present, then JSON.
///
/// Peeled rather than counted, because the number of layers is not a constant
/// anybody documents: the API encodes every Secret value, and Helm had already
/// encoded the gzip. Very old releases were not gzipped at all, which is why
/// the magic number is checked rather than assumed.
fn decode(payload: &str) -> Option<Value> {
    // Two is what a release actually has; the loop bound is a guard, not a
    // guess.
    const MAX_LAYERS: usize = 4;

    let mut bytes = payload.trim().as_bytes().to_vec();
    for _ in 0..MAX_LAYERS {
        if let Some(json) = inflate(&bytes) {
            return serde_json::from_slice(&json).ok();
        }
        match STANDARD.decode(&bytes) {
            Ok(inner) => bytes = inner,
            Err(_) => return None,
        }
    }
    None
}

/// The bytes as JSON, if they are JSON or gzipped JSON.
fn inflate(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        use std::io::Read as _;
        // An unreadable/hostile revision must not allocate without a bound.
        const MAX_JSON: u64 = 64 * 1024 * 1024;
        let decoder = flate2::read::GzDecoder::new(bytes);
        let mut json = Vec::new();
        decoder.take(MAX_JSON + 1).read_to_end(&mut json).ok()?;
        if json.len() as u64 > MAX_JSON {
            return None;
        }
        return Some(json);
    }

    bytes
        .iter()
        .find(|byte| !byte.is_ascii_whitespace())
        .filter(|byte| **byte == b'{')
        .map(|_| bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    fn release_json(name: &str, namespace: &str, revision: i64, status: &str) -> Value {
        json!({
            "name": name,
            "namespace": namespace,
            "version": revision,
            "info": { "status": status, "last_deployed": "2026-09-01T10:00:00Z" },
            "chart": { "metadata": {
                "name": "argo-workflows",
                "version": "0.45.0",
                "appVersion": "v3.6.2"
            }}
        })
    }

    fn secret(release: &Value, gzip: bool) -> DynamicObject {
        let json = serde_json::to_vec(release).expect("json");
        let payload = if gzip {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&json).expect("gzip");
            encoder.finish().expect("gzip")
        } else {
            json
        };

        // Twice, the way a real release Secret is: Helm encodes the payload
        // and the API encodes the Secret value on top of it.
        let encoded = STANDARD.encode(STANDARD.encode(payload));

        let resource = ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("", "v1", "Secret"),
            "secrets",
        );
        DynamicObject::new("sh.helm.release.v1.x.v1", &resource)
            .within(release["namespace"].as_str().unwrap_or("default"))
            .data(json!({ "type": RELEASE_TYPE, "data": { "release": encoded } }))
    }

    #[test]
    fn a_release_reads_the_way_helm_list_prints_it() {
        let parsed = read(&secret(
            &release_json("argo-workflows", "argo", 13, "deployed"),
            true,
        ))
        .expect("a release");

        assert_eq!(parsed.name, "argo-workflows");
        assert_eq!(parsed.namespace, "argo");
        assert_eq!(parsed.revision, 13);
        assert_eq!(parsed.status, "deployed");
        assert_eq!(parsed.chart, "argo-workflows-0.45.0");
        assert_eq!(parsed.app_version, "v3.6.2");
        assert_eq!(parsed.updated.as_deref(), Some("2026-09-01T10:00:00Z"));
    }

    /// Releases from old Helm versions are not compressed, which is why the
    /// gzip magic number is checked rather than assumed.
    #[test]
    fn an_uncompressed_release_still_reads() {
        let parsed = read(&secret(
            &release_json("old", "default", 1, "deployed"),
            false,
        ))
        .expect("a release");
        assert_eq!(parsed.name, "old");
    }

    /// Helm keeps one Secret per revision. A chart upgraded twenty times is
    /// one row, not twenty.
    #[test]
    fn only_the_newest_revision_of_each_release_is_listed() {
        let releases = latest(
            [
                release_json("argo-workflows", "argo", 10, "superseded"),
                release_json("argo-workflows", "argo", 13, "deployed"),
                release_json("argo-workflows", "argo", 11, "superseded"),
                release_json("traefik", "kube-system", 1, "deployed"),
            ]
            .iter()
            .filter_map(|release| read(&secret(release, true))),
        );

        assert_eq!(releases.len(), 2);
        let argo = &releases[0];
        assert_eq!(argo.namespace, "argo");
        assert_eq!(argo.revision, 13);
        assert_eq!(argo.status, "deployed");
    }

    /// The same release name in two namespaces is two releases.
    #[test]
    fn namespaces_keep_releases_apart() {
        let releases = latest(
            [
                release_json("shared", "a", 1, "deployed"),
                release_json("shared", "b", 2, "deployed"),
            ]
            .iter()
            .filter_map(|release| read(&secret(release, true))),
        );

        assert_eq!(releases.len(), 2);
        assert_eq!(releases[0].namespace, "a");
        assert_eq!(releases[1].namespace, "b");
    }

    /// The number of base64 layers is not something anybody documents, so
    /// both the real depth and a shallower one have to work.
    #[test]
    fn the_payload_is_peeled_rather_than_counted() {
        let release = release_json("layers", "default", 1, "deployed");
        let json = serde_json::to_vec(&release).expect("json");

        assert_eq!(
            decode(&STANDARD.encode(STANDARD.encode(&json))),
            Some(release.clone()),
            "two layers, as a Secret has"
        );
        assert_eq!(
            decode(&STANDARD.encode(&json)),
            Some(release),
            "one layer, if the caller already peeled one"
        );
    }

    /// A Secret that is not a release, or one that is corrupt, is skipped
    /// rather than taking the list down with it.
    #[test]
    fn unreadable_secrets_are_skipped() {
        let resource = ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("", "v1", "Secret"),
            "secrets",
        );

        let empty = DynamicObject::new("x", &resource);
        assert!(read(&empty).is_none());

        let nonsense = DynamicObject::new("x", &resource)
            .data(json!({ "data": { "release": "not base64 at all !!" } }));
        assert!(read(&nonsense).is_none());
    }

    #[test]
    fn a_chart_without_a_version_is_still_named() {
        let mut release = release_json("bare", "default", 1, "deployed");
        release["chart"]["metadata"]["version"] = json!("");
        let parsed = read(&secret(&release, true)).expect("a release");
        assert_eq!(parsed.chart, "argo-workflows");
    }

    #[test]
    fn history_preserves_payloads_pruned_gaps_and_damaged_revisions() {
        let identity = Identity {
            name: "web".into(),
            namespace: "default".into(),
        };
        let mut one = release_json("web", "default", 1, "superseded");
        one["config"] = json!({"server":{"port":8080},"password":"do-not-log"});
        one["manifest"] = json!("kind: Deployment\nmetadata:\n  name: web\n");
        let seven = release_json("web", "default", 7, "deployed");
        let mut objects = vec![secret(&seven, true), secret(&one, true)];
        for object in &mut objects {
            let number = read(object).unwrap().revision;
            object.metadata.name = Some(format!("sh.helm.release.v1.web.v{number}"));
            object.metadata.labels = Some(
                [
                    ("name".into(), "web".into()),
                    ("version".into(), number.to_string()),
                ]
                .into(),
            );
        }
        let mut broken = objects[0].clone();
        broken.metadata.name = Some("sh.helm.release.v1.web.v8".into());
        broken
            .metadata
            .labels
            .as_mut()
            .unwrap()
            .insert("version".into(), "8".into());
        broken.data["data"]["release"] = json!("bad payload");
        objects.push(broken);
        objects.push(secret(
            &release_json("elsewhere", "other", 2, "deployed"),
            true,
        ));
        let result = stored_history(&identity, objects.iter());
        assert_eq!(
            result
                .revisions
                .iter()
                .map(|record| record.number)
                .collect::<Vec<_>>(),
            [8, 7, 1]
        );
        assert!(result.revisions[0].revision.is_none());
        assert!(
            result.revisions[0]
                .error
                .as_ref()
                .unwrap()
                .contains("damaged")
        );
        let oldest = result.revisions[2].revision.as_ref().unwrap();
        assert_eq!(oldest.values, one["config"]);
        assert_eq!(oldest.manifest, one["manifest"].as_str().unwrap());
        let newest = result.revisions[1].revision.as_ref().unwrap();
        assert_eq!(newest.values, json!({}));
        assert_eq!(newest.manifest, "");
    }

    #[test]
    fn stored_identity_and_revision_number_are_validated_without_echoing_payloads() {
        let identity = Identity {
            name: "web".into(),
            namespace: "default".into(),
        };
        let mut object = secret(&release_json("other", "default", 1, "deployed"), true);
        assert!(
            read_revision(&object, &identity)
                .unwrap_err()
                .contains("does not match")
        );
        object = secret(&release_json("web", "default", 1, "deployed"), true);
        object.metadata.labels = Some([("version".into(), "2".into())].into());
        assert!(
            read_revision(&object, &identity)
                .unwrap_err()
                .contains("label")
        );
        let mut invalid = release_json("web", "default", 1, "deployed");
        invalid["config"] = json!(["sensitive-payload"]);
        let error = read_revision(&secret(&invalid, true), &identity).unwrap_err();
        assert!(error.contains("mapping") && !error.contains("sensitive-payload"));
    }

    #[test]
    fn a_damaged_latest_revision_remains_discoverable_in_the_release_list() {
        let mut object = secret(&release_json("web", "default", 3, "deployed"), true);
        object.metadata.labels = Some(
            [
                ("name".into(), "web".into()),
                ("version".into(), "3".into()),
            ]
            .into(),
        );
        object.data["data"]["release"] = json!("damaged");
        let fallback = unreadable_summary(&object).unwrap();
        assert_eq!(fallback.status, "unreadable");
        assert_eq!(fallback.revision, 3);
        assert_eq!(fallback.name, "web");
        let older = read(&secret(
            &release_json("web", "default", 2, "deployed"),
            true,
        ))
        .unwrap();
        assert_eq!(latest([older, fallback].into_iter())[0].revision, 3);
    }

    #[cfg(feature = "test-support")]
    #[tokio::test]
    async fn history_reads_only_its_namespace_and_reports_api_permissions() {
        let mut object = secret(&release_json("web", "default", 2, "deployed"), true);
        object.metadata.name = Some("sh.helm.release.v1.web.v2".into());
        object.metadata.labels = Some(
            [
                ("name".into(), "web".into()),
                ("owner".into(), "helm".into()),
            ]
            .into(),
        );
        let fixture =
            crate::test_support::Fixture::start(vec![serde_json::to_value(object).unwrap()]).await;
        let client =
            kube::Client::try_from(kube::Config::new(fixture.url.parse().unwrap())).unwrap();
        let identity = Identity {
            name: "web".into(),
            namespace: "default".into(),
        };
        let result = history(&client, &identity).await.unwrap();
        assert_eq!(result.revisions[0].number, 2);
        let requests = fixture.requests.lock().unwrap().clone();
        assert!(requests.iter().all(|(line, _)| line.starts_with("GET ")));
        assert!(requests[0].0.contains("/namespaces/default/secrets?"));
        assert!(
            requests[0].0.contains("labelSelector=") && requests[0].0.contains("fieldSelector=")
        );
        fixture.deny("secrets");
        let error = history(&client, &identity)
            .await
            .unwrap_err()
            .user_message();
        assert!(error.contains("403") && error.contains("secrets"));
    }
}
