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

    Ok(latest(secrets.items.iter().filter_map(read)))
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
        let mut decoder = flate2::read::GzDecoder::new(bytes);
        let mut json = Vec::new();
        decoder.read_to_end(&mut json).ok()?;
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
            .data(json!({ "data": { "release": encoded } }))
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
}
