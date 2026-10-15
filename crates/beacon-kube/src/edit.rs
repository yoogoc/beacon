//! Reviewed edits of existing resources, pinned to a UID and resource version.
use crate::{Applied, Error, Result};
use kube::{
    Api, Client,
    api::{ApiResource, DynamicObject, Patch, PatchParams},
};
use serde_json::{Value, json};

#[derive(Clone)]
pub struct Plan {
    pub before: Value,
    pub after: Value,
}

fn writable(mut object: Value) -> Value {
    if let Some(fields) = object.as_object_mut() {
        fields.remove("status");
    }
    if let Some(metadata) = object.get_mut("metadata").and_then(Value::as_object_mut) {
        for field in [
            "managedFields",
            "generation",
            "creationTimestamp",
            "deletionTimestamp",
            "deletionGracePeriodSeconds",
        ] {
            metadata.remove(field);
        }
    }
    object
}

/// Carry only the user's edits onto a newly reviewed snapshot. Unedited fields
/// keep their latest values. Arrays are reviewed as whole values, not rebased by index.
fn overlay(base: &Value, draft: &Value, current: &mut Value) {
    if base == draft {
        return;
    }
    if let (Some(base), Some(draft)) = (base.as_object(), draft.as_object()) {
        if !current.is_object() {
            *current = json!({});
        }
        let current = current.as_object_mut().unwrap();
        for (key, value) in draft {
            match base.get(key) {
                Some(before) if before != value => overlay(
                    before,
                    value,
                    current.entry(key.clone()).or_insert(Value::Null),
                ),
                None => {
                    current.insert(key.clone(), value.clone());
                }
                _ => {}
            }
        }
        for key in base.keys().filter(|key| !draft.contains_key(*key)) {
            current.remove(key);
        }
    } else {
        *current = draft.clone();
    }
}

impl Plan {
    pub fn review(base: &Value, draft: Value, current: Value, review_latest: bool) -> Result<Self> {
        for path in [
            "/apiVersion",
            "/kind",
            "/metadata/name",
            "/metadata/namespace",
        ] {
            if base.pointer(path) != draft.pointer(path)
                || base.pointer(path) != current.pointer(path)
            {
                return Err(Error::Manifest("Keep the resource's apiVersion, kind, name and namespace unchanged when saving YAML.".into()));
            }
        }
        let identity = |path: &str| {
            base.pointer(path).and_then(Value::as_str).filter(|value| !value.is_empty()).map(str::to_owned)
            .ok_or_else(|| Error::Manifest("The editor has no resource UID or version. Reload the resource before saving.".into()))
        };
        let uid = identity("/metadata/uid")?;
        let version = identity("/metadata/resourceVersion")?;
        if current.pointer("/metadata/uid").and_then(Value::as_str) != Some(uid.as_str()) {
            return Err(Error::Manifest("This resource was replaced after the editor opened. Your draft was kept; open the replacement resource separately.".into()));
        }
        for (path, value) in [
            ("/metadata/uid", &uid),
            ("/metadata/resourceVersion", &version),
        ] {
            if draft
                .pointer(path)
                .is_some_and(|edited| edited.as_str() != Some(value.as_str()))
            {
                return Err(Error::Manifest("metadata.uid and metadata.resourceVersion are read-only. Keep the editor's original values.".into()));
            }
        }
        if current
            .pointer("/metadata/resourceVersion")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(Error::Manifest(
                "The current resource has no resourceVersion.".into(),
            ));
        }
        if !review_latest
            && current
                .pointer("/metadata/resourceVersion")
                .and_then(Value::as_str)
                != Some(version.as_str())
        {
            return Err(Error::EditConflict("The resource changed in the cluster. Your draft was kept. Review the latest version before saving.".into()));
        }
        let before = writable(current);
        let mut draft = writable(draft);
        // Identity comes from the snapshot, never from editable YAML.
        for field in ["uid", "resourceVersion"] {
            draft["metadata"][field] = base["metadata"][field].clone();
        }
        let mut after = before.clone();
        overlay(&writable(base.clone()), &draft, &mut after);
        for field in ["uid", "resourceVersion"] {
            after["metadata"][field] = before["metadata"][field].clone();
        }
        Ok(Self { before, after })
    }

    pub fn patch(&self) -> json_patch::Patch {
        let mut patch: json_patch::Patch = serde_json::from_value(json!([
            {"op":"test","path":"/metadata/uid","value":self.before["metadata"]["uid"]},
            {"op":"test","path":"/metadata/resourceVersion","value":self.before["metadata"]["resourceVersion"]}
        ])).expect("fixed JSON Patch identity tests");
        patch
            .0
            .extend(json_patch::diff(&self.before, &self.after).0);
        patch
    }

    pub async fn execute(&self, client: &Client, resource: &ApiResource) -> Result<Applied> {
        if self.before == self.after {
            return Err(Error::Manifest("No changes to save.".into()));
        }
        let name = self.before["metadata"]["name"]
            .as_str()
            .ok_or_else(|| Error::Manifest("Resource name is missing.".into()))?;
        if self.before["apiVersion"] != resource.api_version || self.before["kind"] != resource.kind
        {
            return Err(Error::Manifest(
                "The reviewed resource does not match this endpoint.".into(),
            ));
        }
        let api: Api<DynamicObject> = match self.before["metadata"]["namespace"].as_str() {
            Some(namespace) => Api::namespaced_with(client.clone(), namespace, resource),
            None => Api::all_with(client.clone(), resource),
        };
        let params = PatchParams {
            field_manager: Some(crate::ops::FIELD_MANAGER.into()),
            ..Default::default()
        };
        match api
            .patch(name, &params, &Patch::<Value>::Json(self.patch()))
            .await
        {
            Ok(saved) => {
                tracing::info!(kind = %resource.kind, name, "saved resource edits");
                Ok(Applied::Ok(Box::new(saved)))
            }
            Err(kube::Error::Api(status))
                if status.code == 409
                    || (status.code == 422
                        && (status.message.contains("test failed")
                            || status.message.contains("test operation"))) =>
            {
                Err(Error::EditConflict(format!(
                    "The resource changed after review. Your draft was kept. Review the latest version before saving.\n{} (HTTP {})",
                    status.message, status.code
                )))
            }
            Err(error) => Err(Error::Api(error)),
        }
    }
}

#[cfg(test)]
mod tests;
