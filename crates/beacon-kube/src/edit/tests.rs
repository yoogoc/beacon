use super::*;
use crate::{ClusterId, ClusterSession, test_support::Fixture};
use std::sync::Arc;

fn original() -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":"edit","namespace":"default","uid":"original-uid","resourceVersion":"7","labels":{"app":"fixture"},"managedFields":[{"manager":"other"}],"creationTimestamp":"2026-10-15T00:00:00Z"},"data":{"setting":"before","untouched":"keep","remove":"old"},"binaryData":{"bytes":"/w=="},"status":{"phase":"Ready"}})
}
fn resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &kube::core::GroupVersionKind::gvk("", "v1", "ConfigMap"),
        "configmaps",
    )
}
fn session(fixture: &Fixture) -> Arc<ClusterSession> {
    Arc::new(ClusterSession::for_testing(
        ClusterId::new("save-edit-localhost"),
        fixture.url.clone(),
        vec![],
    ))
}

#[tokio::test]
async fn yaml_patch_saves_only_reviewed_changes_and_preserves_unedited_fields() {
    let base = original();
    let mut draft = base.clone();
    draft["data"]["setting"] = json!("after\n配置=value");
    draft["data"].as_object_mut().unwrap().remove("remove");
    draft["metadata"]["annotations"] = json!({"example.io/a~b":"value"});
    let plan = Plan::review(&base, draft, base.clone(), false).unwrap();
    let fixture = Fixture::start(vec![base.clone()]).await;
    let session = session(&fixture);
    assert!(matches!(
        session
            .clone()
            .save_patch(resource(), plan.clone())
            .await
            .unwrap(),
        Applied::Ok(_)
    ));
    let mut expected = base.clone();
    expected["data"]["setting"] = json!("after\n配置=value");
    expected["data"].as_object_mut().unwrap().remove("remove");
    expected["metadata"]["annotations"] = json!({"example.io/a~b":"value"});
    expected["metadata"]["resourceVersion"] = json!("8");
    assert_eq!(fixture.object("ConfigMap", "edit"), expected);
    let requests = fixture.requests.lock().unwrap();
    let (line, patch) = requests
        .iter()
        .find(|(line, _)| line.starts_with("PATCH"))
        .unwrap();
    assert!(line.contains("fieldManager=beacon") && !line.contains("force"));
    let operations = patch.as_array().unwrap();
    assert_eq!(operations.len(), 5); // two preconditions and three actual edits
    assert_eq!(
        operations[0],
        json!({"op":"test","path":"/metadata/uid","value":"original-uid"})
    );
    assert_eq!(
        operations[1],
        json!({"op":"test","path":"/metadata/resourceVersion","value":"7"})
    );
    assert!(operations.iter().skip(2).all(|operation| !matches!(
        operation["path"].as_str(),
        Some("/data/untouched" | "/binaryData" | "/status" | "/metadata/managedFields")
    )));
}

#[tokio::test]
async fn yaml_patch_rejects_changes_after_review_and_replacement_uids_atomically() {
    for replaced in [false, true] {
        let base = original();
        let mut draft = base.clone();
        draft["data"]["setting"] = json!("mine");
        let plan = Plan::review(&base, draft, base.clone(), false).unwrap();
        let mut current = base.clone();
        current["metadata"]["resourceVersion"] = json!("8");
        current["data"]["setting"] = json!("someone else's value");
        if replaced {
            current["metadata"]["uid"] = json!("replacement-uid");
        }
        let fixture = Fixture::start(vec![current.clone()]).await;
        let error = session(&fixture)
            .save_patch(resource(), plan)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::EditConflict(_)));
        assert!(!error.user_message().contains("owner"));
        assert_eq!(fixture.object("ConfigMap", "edit"), current);
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            1,
            "a failed write must not be retried"
        );
    }
}

#[test]
fn yaml_patch_review_latest_keeps_unedited_live_changes_and_requires_matching_identity() {
    let base = original();
    let mut draft = base.clone();
    draft["data"]["setting"] = json!("mine");
    let mut current = base.clone();
    current["metadata"]["resourceVersion"] = json!("8");
    current["data"]["untouched"] = json!("new live value");
    current["data"]["new-key"] = json!("new live key");
    assert!(matches!(
        Plan::review(&base, draft.clone(), current.clone(), false),
        Err(Error::EditConflict(_))
    ));
    let plan = Plan::review(&base, draft.clone(), current.clone(), true).unwrap();
    assert_eq!(plan.after["data"]["setting"], "mine");
    assert_eq!(plan.after["data"]["untouched"], "new live value");
    assert_eq!(plan.after["data"]["new-key"], "new live key");
    assert_eq!(plan.after["metadata"]["resourceVersion"], "8");
    current["metadata"]["uid"] = json!("replaced");
    assert!(Plan::review(&base, draft.clone(), current, true).is_err());
    for path in ["name", "namespace", "uid", "resourceVersion"] {
        let mut changed = draft.clone();
        changed["metadata"][path] = json!("different");
        assert!(
            Plan::review(&base, changed, base.clone(), true).is_err(),
            "accepted changed {path}"
        );
    }
}

#[test]
fn yaml_patch_handles_null_deletions_arrays_and_escaped_keys_without_rebasing_array_indices() {
    let mut base = original();
    base["spec"] = json!({"nullable":"old","remove":1,"example.io/a~b":0,"containers":[{"name":"a","image":"old"},{"name":"b","image":"keep"}]});
    let mut draft = base.clone();
    draft["spec"]["nullable"] = Value::Null;
    draft["spec"].as_object_mut().unwrap().remove("remove");
    draft["spec"]["example.io/a~b"] = json!(1);
    draft["spec"]["containers"][0]["image"] = json!("new");
    let mut current = base.clone();
    current["metadata"]["resourceVersion"] = json!("8");
    current["spec"]["containers"] =
        json!([{"name":"b","image":"changed"},{"name":"a","image":"old"}]);
    let plan = Plan::review(&base, draft.clone(), current.clone(), true).unwrap();
    let mut actual = current.clone();
    json_patch::patch(&mut actual, &plan.patch()).unwrap();
    assert_eq!(actual["spec"], draft["spec"]);
    assert!(actual["spec"].get("nullable").unwrap().is_null());
    assert!(actual["spec"].get("remove").is_none());
    assert_eq!(
        actual["metadata"]["managedFields"],
        current["metadata"]["managedFields"]
    );
}

#[tokio::test]
async fn yaml_patch_distinguishes_validation_failures_from_version_conflicts() {
    let base = original();
    let mut draft = base.clone();
    draft["data"]["setting"] = json!("after");
    let plan = Plan::review(&base, draft, base.clone(), false).unwrap();
    let fixture = Fixture::start(vec![base.clone()]).await;
    for (code, message, stale) in [
        (422, "invalid data field", false),
        (403, "Forbidden", false),
        (
            422,
            "testing value /metadata/resourceVersion failed: test failed",
            true,
        ),
        (409, "resource changed after review", true),
    ] {
        fixture.patch_error(Some(json!({"apiVersion":"v1","kind":"Status","code":code,"reason":"Invalid","message":message})));
        let error = session(&fixture)
            .save_patch(resource(), plan.clone())
            .await
            .unwrap_err();
        assert_eq!(matches!(error, Error::EditConflict(_)), stale);
        assert!(error.user_message().contains(message));
        assert_eq!(fixture.object("ConfigMap", "edit"), base);
    }
}

#[tokio::test]
async fn ssa_only_reports_real_field_manager_conflicts_as_ownership() {
    let base = original();
    let fixture = Fixture::start(vec![base.clone()]).await;
    let session = session(&fixture);
    let errors = [
        (
            json!({"message":"the object has been modified; apply your changes to the latest version","code":409,"reason":"Conflict","details":{"causes":[{"reason":"ResourceVersionConflict","field":"metadata.resourceVersion"}]}}),
            false,
        ),
        (
            json!({"message":"Apply failed with 1 conflict: conflict with \"argocd-controller\" using v1: .data.setting","code":409,"reason":"Conflict"}),
            true,
        ),
        (
            json!({"message":"structured field conflict","code":409,"reason":"Conflict","details":{"causes":[{"reason":"FieldManagerConflict","field":".data.setting","message":"conflict with \"kubectl\""}]}}),
            true,
        ),
        (
            json!({"message":"a webhook refused this update","code":409,"reason":"Conflict"}),
            false,
        ),
    ];
    for (error, owner) in errors {
        fixture.patch_error(Some(error));
        let result = session
            .clone()
            .run(
                crate::Operation::Apply,
                resource(),
                Some("default".into()),
                "edit".into(),
                Some(base.clone()),
                false,
            )
            .await;
        if owner {
            let Applied::Conflict(conflict) = result.unwrap() else {
                panic!("missing ownership conflict");
            };
            assert_eq!(conflict.fields, [".data.setting"]);
            assert!(!conflict.managers.is_empty());
        } else {
            assert!(
                matches!(result, Err(Error::EditConflict(_))),
                "non-owner 409 was misreported: {result:?}"
            );
        }
    }
    assert_eq!(fixture.object("ConfigMap", "edit"), base);
}
