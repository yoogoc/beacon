use super::*;
use crate::{ClusterId, ClusterSession, test_support::Fixture};
use std::sync::Arc;

fn deployment(name: &str, namespace: &str) -> Value {
    json!({"apiVersion":"apps/v1","kind":"Deployment",
        "metadata":{"name":name,"namespace":namespace,"uid":format!("uid-{name}"),"resourceVersion":"42"},
        "spec":{"replicas":3,"strategy":{"type":"RollingUpdate"},"selector":{"matchLabels":{"app":name}},
            "template":{"metadata":{"labels":{"app":name},"annotations":{"keep":"unchanged"}},
                "spec":{"containers":[{"name":"app","image":"nginx:stable","ports":[{"containerPort":80}]}]}}},
        "status":{"readyReplicas":3}})
}
fn resource() -> ApiResource {
    ApiResource::from_gvk_with_plural(
        &kube::core::GroupVersionKind::gvk("apps", "v1", "Deployment"),
        "deployments",
    )
}
fn target(object: &Value) -> DeleteTarget {
    DeleteTarget {
        reference: ObjectRef::new(
            object["metadata"]["namespace"].as_str().map(str::to_owned),
            object["metadata"]["name"].as_str().unwrap(),
        ),
        uid: object["metadata"]["uid"].as_str().unwrap().into(),
    }
}
fn session(fixture: &Fixture) -> Arc<ClusterSession> {
    Arc::new(ClusterSession::for_testing(
        ClusterId::new("bulk-restart-localhost"),
        fixture.url.clone(),
        vec![],
    ))
}

#[tokio::test]
async fn bulk_restart_preserves_templates_and_checks_each_namespace() {
    let objects = [deployment("web", "default"), deployment("worker", "team")];
    let fixture = Fixture::start(objects.to_vec()).await;
    let results = session(&fixture)
        .restart_many(resource(), objects.iter().map(target).collect())
        .await;
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|(_, result)| result.is_ok()));
    let mut timestamp = None;
    for original in &objects {
        let mut expected = original.clone();
        let actual = fixture.object("Deployment", original["metadata"]["name"].as_str().unwrap());
        let stamp = actual["spec"]["template"]["metadata"]["annotations"]["kubectl.kubernetes.io/restartedAt"].as_str().unwrap().to_owned();
        stamp.parse::<k8s_openapi::jiff::Timestamp>().unwrap();
        if let Some(previous) = &timestamp {
            assert_eq!(previous, &stamp);
        }
        timestamp = Some(stamp.clone());
        expected["spec"]["template"]["metadata"]["annotations"]["kubectl.kubernetes.io/restartedAt"] =
            json!(stamp);
        expected["metadata"]["resourceVersion"] = actual["metadata"]["resourceVersion"].clone();
        assert_eq!(actual, expected);
    }
    let requests = fixture.requests.lock().unwrap();
    for namespace in ["default", "team"] {
        assert!(requests.iter().any(
            |(request, body)| request.contains("/selfsubjectrulesreviews")
                && body["spec"]["namespace"] == namespace
        ));
    }
    let patches: Vec<_> = requests
        .iter()
        .filter(|(request, _)| request.starts_with("PATCH"))
        .collect();
    assert_eq!(patches.len(), 2);
    for (_, patch) in patches {
        assert_eq!(patch["metadata"]["resourceVersion"], "42");
        assert!(
            patch["metadata"]["uid"]
                .as_str()
                .unwrap()
                .starts_with("uid-")
        );
        assert_eq!(patch["spec"]["template"].as_object().unwrap().len(), 1);
        assert!(patch["spec"]["template"]["metadata"]["annotations"]["kubectl.kubernetes.io/restartedAt"].is_string());
    }
}

#[tokio::test]
async fn bulk_restart_isolates_named_permission_paused_and_replaced_targets() {
    let healthy = deployment("healthy", "default");
    let mut paused = deployment("paused", "default");
    paused["spec"]["paused"] = json!(true);
    let old = deployment("replaced", "default");
    let mut replacement = old.clone();
    replacement["metadata"]["uid"] = json!("replacement-uid");
    let denied = deployment("denied", "default");
    let fixture = Fixture::start(vec![
        healthy.clone(),
        paused.clone(),
        replacement.clone(),
        denied.clone(),
    ])
    .await;
    fixture.rules(json!([
        {"verbs":["get"],"apiGroups":["apps"],"resources":["deployments"]},
        {"verbs":["patch"],"apiGroups":["apps"],"resources":["deployments"],"resourceNames":["healthy","paused","replaced"]}
    ]));
    let results = session(&fixture)
        .restart_many(
            resource(),
            [&healthy, &paused, &old, &denied]
                .into_iter()
                .map(target)
                .collect(),
        )
        .await;
    let result = |name: &str| {
        &results
            .iter()
            .find(|(target, _)| target.reference.name == name)
            .unwrap()
            .1
    };
    assert!(result("healthy").is_ok());
    assert!(
        result("paused")
            .as_ref()
            .unwrap_err()
            .user_message()
            .contains("paused")
    );
    assert!(
        result("replaced")
            .as_ref()
            .unwrap_err()
            .user_message()
            .contains("replaced")
    );
    assert!(
        result("denied")
            .as_ref()
            .unwrap_err()
            .user_message()
            .contains("Forbidden: cannot patch")
    );
    for original in [&paused, &replacement, &denied] {
        assert_eq!(
            fixture.object("Deployment", original["metadata"]["name"].as_str().unwrap()),
            *original
        );
    }
    let requests = fixture.requests.lock().unwrap();
    let patches: Vec<_> = requests
        .iter()
        .filter(|(request, _)| request.starts_with("PATCH"))
        .collect();
    assert_eq!(patches.len(), 1);
    assert!(patches[0].0.contains("/deployments/healthy"));
}

#[tokio::test]
async fn bulk_restart_requires_get_and_does_not_report_api_failures_as_success() {
    let object = deployment("web", "default");
    let fixture = Fixture::start(vec![object.clone()]).await;
    fixture.rules(json!([{"verbs":["patch"],"apiGroups":["apps"],"resources":["deployments"]}]));
    let results = session(&fixture)
        .restart_many(resource(), vec![target(&object)])
        .await;
    assert!(
        results[0]
            .1
            .as_ref()
            .unwrap_err()
            .user_message()
            .contains("Forbidden: cannot get")
    );
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("GET") || request.starts_with("PATCH"))
    );
    fixture.rules(json!([{"verbs":["*"],"apiGroups":["*"],"resources":["*"]}]));
    fixture.deny("deployments");
    let results = session(&fixture)
        .restart_many(resource(), vec![target(&object)])
        .await;
    assert!(
        results[0]
            .1
            .as_ref()
            .unwrap_err()
            .user_message()
            .contains("403")
    );
    assert_eq!(fixture.object("Deployment", "web"), object);
}

#[tokio::test]
async fn bulk_restart_rejects_unidentified_terminating_and_other_kinds() {
    let mut object = deployment("web", "default");
    object["metadata"]["deletionTimestamp"] = json!("2026-10-14T00:00:00Z");
    let fixture = Fixture::start(vec![object.clone()]).await;
    let session = session(&fixture);
    let selected = target(&object);
    assert!(
        restart_selected(session.write_client(), &resource(), &selected, "now")
            .await
            .unwrap_err()
            .user_message()
            .contains("terminating")
    );
    let mut unidentified = selected.clone();
    unidentified.uid.clear();
    assert!(
        restart_selected(session.write_client(), &resource(), &unidentified, "now")
            .await
            .is_err()
    );
    let pod = ApiResource::from_gvk_with_plural(
        &kube::core::GroupVersionKind::gvk("", "v1", "Pod"),
        "pods",
    );
    assert!(
        restart_selected(session.write_client(), &pod, &selected, "now")
            .await
            .is_err()
    );
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.starts_with("PATCH"))
    );
}
