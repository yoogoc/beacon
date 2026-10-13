//! Pure preparation runs off the foreground thread; no concealed value enters an editor.
use super::{Document, Page};
use beacon_kube::helm::{History, Revision};
use gpui_kit::component::diff::DiffFile;
use serde_json::Value;
use sha2::{Digest as _, Sha256};

pub(super) struct Prepared {
    pub text: String,
    pub files: Option<Vec<DiffFile>>,
}
pub(super) fn conceal(value: &mut Value) {
    match value {
        Value::Object(map) => map.values_mut().for_each(conceal),
        Value::Array(items) => items.iter_mut().for_each(conceal),
        _ => {
            let digest = Sha256::digest(value.to_string().as_bytes());
            *value = Value::String(format!("[concealed · sha256:{digest:x}]"));
        }
    }
}
pub(super) fn secret_documents(value: &mut Value) -> bool {
    if value["kind"] == "Secret" {
        for field in ["data", "stringData"] {
            if let Some(data) = value.get_mut(field).and_then(Value::as_object_mut) {
                for value in data.values_mut() {
                    conceal(value);
                }
            }
        }
        if let Some(annotations) = value
            .pointer_mut("/metadata/annotations")
            .and_then(Value::as_object_mut)
        {
            annotations.remove("kubectl.kubernetes.io/last-applied-configuration");
        }
        return true;
    }
    match value {
        Value::Object(map) => map
            .values_mut()
            .map(secret_documents)
            .fold(false, |a, b| a | b),
        Value::Array(items) => items
            .iter_mut()
            .map(secret_documents)
            .fold(false, |a, b| a | b),
        _ => false,
    }
}
fn document(revision: &Revision, kind: Document, reveal: bool) -> Result<String, String> {
    match kind {
        Document::Values => {
            let mut values = revision.values.clone();
            if !reveal {
                conceal(&mut values);
            }
            crate::yaml_review::canonical_yaml(values)
        }
        Document::Manifest => {
            if revision.manifest.trim().is_empty() {
                return Ok("# This revision contains no rendered resources.\n".into());
            }
            if reveal {
                return Ok(revision.manifest.clone());
            }
            let mut documents: Vec<Value> = serde_saphyr::from_multiple(&revision.manifest)
                .map_err(|_| "This manifest cannot be safely concealed. Reveal values to inspect the stored text.".to_string())?;
            let mut concealed = false;
            for value in &mut documents {
                concealed |= secret_documents(value);
            }
            if !concealed {
                return Ok(revision.manifest.clone());
            }
            documents
                .into_iter()
                .filter(|value| !value.is_null())
                .map(crate::yaml_review::canonical_yaml)
                .collect::<Result<Vec<_>, _>>()
                .map(|documents| documents.join("---\n"))
        }
    }
}
fn revision(history: &History, number: i64) -> Result<&Revision, String> {
    let record = history.revisions.iter().find(|record| record.number == number)
        .ok_or_else(|| format!("Revision {number} is not available. Helm may have pruned its history; refresh to read the current revisions."))?;
    record.revision.as_ref().ok_or_else(|| {
        record
            .error
            .clone()
            .unwrap_or_else(|| "This revision cannot be read.".into())
    })
}
pub(super) fn prepare(
    history: &History,
    page: Page,
    kind: Document,
    selected: i64,
    from: i64,
    to: i64,
    reveal: bool,
) -> Result<Prepared, String> {
    if history.revisions.is_empty() {
        return Err("No stored revisions were found. This release may have been removed.".into());
    }
    if page == Page::Compare {
        if from == to {
            return Err("Choose two different revisions to compare.".into());
        }
        let before = document(revision(history, from)?, kind, reveal)?;
        let after = document(revision(history, to)?, kind, reveal)?;
        let preview = crate::yaml_review::Preview::text(&before, after)?;
        let files = preview
            .files()
            .iter()
            .cloned()
            .map(|file| file.with_language("yaml"))
            .collect();
        return Ok(Prepared {
            text: String::new(),
            files: Some(files),
        });
    }
    Ok(Prepared {
        text: document(revision(history, selected)?, kind, reveal)?,
        files: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use beacon_kube::{
        Release,
        helm::{Identity, RevisionRecord},
    };
    use serde_json::json;
    fn revision_fixture(number: i64) -> Revision {
        Revision {
            summary: Release {
                name: "web".into(),
                namespace: "default".into(),
                revision: number,
                status: "deployed".into(),
                chart: "web-1.0".into(),
                app_version: "1.0".into(),
                updated: None,
            },
            chart_name: "web".into(),
            chart_version: "1.0".into(),
            first_deployed: None,
            description: String::new(),
            notes: String::new(),
            hooks: vec![],
            values: json!({"password":"private-value", "replicas": number}),
            manifest: format!(
                "apiVersion: v1\nkind: Secret\nmetadata:\n  name: credentials\ndata:\n  token: private-base64-{number}\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: {number}\n"
            ),
        }
    }
    #[test]
    fn concealed_values_and_multidocument_secrets_keep_diff_identity_without_disclosing_content() {
        let a = revision_fixture(1);
        let b = revision_fixture(2);
        for kind in [Document::Values, Document::Manifest] {
            let hidden = document(&a, kind, false).unwrap();
            assert!(!hidden.contains("private-value") && !hidden.contains("private-base64"));
            assert!(hidden.contains("concealed"));
            assert_ne!(hidden, document(&b, kind, false).unwrap());
            let revealed = document(&a, kind, true).unwrap();
            assert!(revealed.contains(if kind == Document::Values {
                "private-value"
            } else {
                "private-base64"
            }));
        }
        assert!(
            document(&a, Document::Manifest, false)
                .unwrap()
                .contains("Deployment")
        );
    }
    #[test]
    fn comparisons_reject_missing_damaged_and_identical_revisions() {
        let history = History {
            identity: Identity {
                name: "web".into(),
                namespace: "default".into(),
            },
            revisions: vec![
                RevisionRecord {
                    number: 3,
                    secret_name: "broken".into(),
                    revision: None,
                    error: Some("Damaged revision".into()),
                },
                RevisionRecord {
                    number: 2,
                    secret_name: "v2".into(),
                    revision: Some(revision_fixture(2)),
                    error: None,
                },
                RevisionRecord {
                    number: 1,
                    secret_name: "v1".into(),
                    revision: Some(revision_fixture(1)),
                    error: None,
                },
            ],
        };
        let prepared =
            prepare(&history, Page::Compare, Document::Manifest, 2, 1, 2, false).unwrap();
        assert!(
            prepared
                .files
                .unwrap()
                .iter()
                .any(|file| file.additions() > 0)
        );
        assert!(prepare(&history, Page::Compare, Document::Values, 2, 2, 2, false).is_err());
        assert!(
            prepare(&history, Page::Compare, Document::Values, 2, 1, 3, false)
                .err()
                .unwrap()
                .contains("Damaged")
        );
        assert!(
            prepare(&history, Page::Values, Document::Values, 99, 1, 2, false)
                .err()
                .unwrap()
                .contains("not available")
        );
    }
    #[test]
    fn unsafe_yaml_is_withheld_and_empty_revisions_are_explicit() {
        let mut release = revision_fixture(1);
        release.manifest = "kind: Secret\ndata: [unfinished".into();
        assert!(document(&release, Document::Manifest, false).is_err());
        assert_eq!(
            document(&release, Document::Manifest, true).unwrap(),
            release.manifest
        );
        release.manifest.clear();
        assert!(
            document(&release, Document::Manifest, false)
                .unwrap()
                .contains("no rendered resources")
        );
        release.values = json!({});
        assert_eq!(
            document(&release, Document::Values, false).unwrap().trim(),
            "{}"
        );
    }
}
