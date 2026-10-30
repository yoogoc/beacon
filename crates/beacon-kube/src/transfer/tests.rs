use super::*;
use crate::{ApiResource, ClusterId, GroupVersionKind, test_support::Fixture};
use serde_json::json;
fn kind(group: &str, name: &str, plural: &str) -> Kind {
    Kind {
        resource: ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk(group, "v1", name),
            plural,
        ),
        namespaced: !matches!(
            name,
            "ClusterRole" | "ClusterRoleBinding" | "PersistentVolume" | "Node" | "Namespace"
        ),
        verbs: vec![
            "get".into(),
            "create".into(),
            "delete".into(),
            "list".into(),
            "watch".into(),
        ],
    }
}
fn kinds() -> Vec<Kind> {
    vec![
        kind("", "ConfigMap", "configmaps"),
        kind("", "Secret", "secrets"),
        kind("", "ServiceAccount", "serviceaccounts"),
        kind("", "Service", "services"),
        kind("", "Pod", "pods"),
        kind("apps", "Deployment", "deployments"),
        kind("rbac.authorization.k8s.io", "ClusterRole", "clusterroles"),
        kind("rbac.authorization.k8s.io", "RoleBinding", "rolebindings"),
    ]
}
fn config(name: &str) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":"source","uid":format!("source-{name}"),"resourceVersion":"7","creationTimestamp":"2026-10-01T00:00:00Z","managedFields":[],"finalizers":[]},"data":{"config":"value"}})
}
fn selection(value: &Value) -> Selection {
    let object: DynamicObject = serde_json::from_value(value.clone()).unwrap();
    let kind = kinds()
        .into_iter()
        .find(|k| Some(k.resource.kind.as_str()) == value["kind"].as_str())
        .unwrap();
    Selection {
        kind,
        reference: ObjectRef::of(&object),
        uid: object.metadata.uid,
        name: object.metadata.name.unwrap(),
        copy_only: false,
    }
}
fn options(moving: bool) -> Options {
    Options {
        namespace: Some("target".into()),
        moving,
        secret_consent: false,
    }
}
async fn fixtures(
    objects: Vec<Value>,
) -> (Fixture, Arc<ClusterSession>, Fixture, Arc<ClusterSession>) {
    let a = Fixture::start(objects).await;
    let b = Fixture::start(vec![]).await;
    let left = Arc::new(ClusterSession::for_testing(
        ClusterId::new("source-context"),
        a.url.clone(),
        kinds(),
    ));
    let right = Arc::new(ClusterSession::for_testing(
        ClusterId::new("target-context"),
        b.url.clone(),
        kinds(),
    ));
    (a, left, b, right)
}
fn writes(fixture: &Fixture, verb: &str) -> Vec<(String, Value)> {
    fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(url, _)| url.starts_with(verb) && !url.contains("dryRun="))
        .cloned()
        .collect()
}
fn deletes(fixture: &Fixture) -> Vec<(String, Value)> {
    writes(fixture, "DELETE")
}

#[tokio::test]
async fn cross_cluster_move_is_reviewed_create_only_then_version_guarded_delete() {
    let object = config("a");
    let (a, left, b, right) = fixtures(vec![object.clone()]).await;
    let mut batch = Batch::check(
        left.clone(),
        right.clone(),
        vec![selection(&object)],
        options(true),
    )
    .await;
    assert!(batch.ready(), "{:?}", batch.items()[0].problem);
    assert!(writes(&b, "POST /api/v1/namespaces/target/configmaps").is_empty());
    assert!(deletes(&a).is_empty());
    batch.create().await.unwrap();
    assert!(batch.can_cleanup());
    assert!(
        left.clone()
            .get_object(
                kind("", "ConfigMap", "configmaps").resource,
                Some("source".into()),
                "a".into()
            )
            .await
            .is_ok()
    );
    let target = right
        .clone()
        .get_object(
            kind("", "ConfigMap", "configmaps").resource,
            Some("target".into()),
            "a".into(),
        )
        .await
        .unwrap();
    assert_eq!(target.data["data"]["config"], "value");
    assert_eq!(target.metadata.namespace.as_deref(), Some("target"));
    assert_ne!(target.metadata.uid.as_deref(), Some("source-a"));
    assert!(target.metadata.finalizers.is_none());
    batch.cleanup().await.unwrap();
    assert!(batch.complete());
    let requests = deletes(&a);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1["preconditions"],
        json!({"uid":"source-a","resourceVersion":"7"})
    );
}
#[tokio::test]
async fn same_cluster_copy_keeps_sources_and_never_accepts_cleanup() {
    let object = config("a");
    let fixture = Fixture::start(vec![object.clone()]).await;
    let session = Arc::new(ClusterSession::for_testing(
        ClusterId::new("one"),
        fixture.url.clone(),
        kinds(),
    ));
    let mut batch = Batch::check(
        session.clone(),
        session,
        vec![selection(&object)],
        options(false),
    )
    .await;
    assert!(batch.ready());
    batch.create().await.unwrap();
    assert!(batch.items()[0].verified);
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&fixture).is_empty());
}
#[tokio::test]
async fn conflicts_and_same_endpoint_aliases_never_overwrite() {
    let object = config("a");
    let (a, left, b, right) = fixtures(vec![object.clone()]).await;
    let mut existing = object.clone();
    existing["metadata"]["namespace"] = json!("target");
    b.put(existing);
    let batch = Batch::check(left.clone(), right, vec![selection(&object)], options(true)).await;
    assert!(!batch.ready());
    assert!(batch.items()[0].conflict);
    assert!(writes(&b, "POST /api/v1/namespaces/target/configmaps").is_empty());
    let alias = Arc::new(ClusterSession::for_testing(
        ClusterId::new("different-name"),
        format!("{}/", a.url),
        kinds(),
    ));
    let mut opts = options(true);
    opts.namespace = Some("source".into());
    let batch = Batch::check(left, alias, vec![selection(&object)], opts).await;
    assert!(!batch.ready());
    assert!(
        batch.items()[0]
            .problem
            .as_ref()
            .unwrap()
            .contains("identical")
    );
}
#[tokio::test]
async fn any_source_version_change_blocks_the_entire_cleanup_phase() {
    let first = config("a");
    let second = config("b");
    let (a, left, _b, right) = fixtures(vec![first.clone(), second.clone()]).await;
    let mut batch = Batch::check(
        left,
        right,
        vec![selection(&first), selection(&second)],
        options(true),
    )
    .await;
    batch.create().await.unwrap();
    let mut changed = second;
    changed["metadata"]["resourceVersion"] = json!("8");
    changed["data"]["config"] = json!("newer");
    a.put(changed);
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&a).is_empty());
}
#[tokio::test]
async fn target_replacement_or_config_edits_block_source_deletion() {
    let object = config("a");
    let (a, left, b, right) = fixtures(vec![object.clone()]).await;
    let mut batch = Batch::check(left, right, vec![selection(&object)], options(true)).await;
    batch.create().await.unwrap();
    let original = b.object("ConfigMap", "a");
    let mut changed = original.clone();
    changed["metadata"]["annotations"]["example.org/routing"] = json!("changed");
    b.put(changed.clone());
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&a).is_empty());
    let mut changed = original;
    changed["data"]["config"] = json!("changed");
    b.put(changed.clone());
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&a).is_empty());
    changed["metadata"]["uid"] = json!("replacement");
    b.put(changed);
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&a).is_empty());
}
#[tokio::test]
async fn partial_creation_and_delete_failures_retry_only_failed_entries() {
    let first = config("a");
    let second = config("b");
    let (a, left, b, right) = fixtures(vec![first.clone(), second.clone()]).await;
    let mut batch = Batch::check(
        left,
        right,
        vec![selection(&first), selection(&second)],
        options(true),
    )
    .await;
    assert!(batch.ready());
    b.deny("post/configmaps/b");
    batch.create().await.unwrap();
    assert!(!batch.can_cleanup());
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&a).is_empty());
    b.allow("post/configmaps/b");
    batch.create().await.unwrap();
    assert!(batch.can_cleanup());
    let posts = writes(&b, "POST /api/v1/namespaces/target/configmaps");
    assert_eq!(
        posts
            .iter()
            .filter(|(_, body)| body["metadata"]["name"] == "a")
            .count(),
        1
    );
    a.deny("delete/configmaps/b");
    batch.cleanup().await.unwrap();
    assert!(!batch.complete());
    assert_eq!(batch.items().iter().filter(|item| item.deleted).count(), 1);
    a.allow("delete/configmaps/b");
    batch.cleanup().await.unwrap();
    assert!(batch.complete());
    assert_eq!(
        deletes(&a)
            .iter()
            .filter(|(url, _)| url.starts_with("DELETE /api/v1/namespaces/source/configmaps/a"))
            .count(),
        1
    );
}
#[tokio::test]
async fn source_finalizers_remain_pending_and_are_never_forced() {
    let mut object = config("a");
    object["metadata"]["finalizers"] = json!(["fixture.example/hold"]);
    let (a, left, _b, right) = fixtures(vec![object.clone()]).await;
    let mut batch = Batch::check(left, right, vec![selection(&object)], options(true)).await;
    batch.create().await.unwrap();
    batch.cleanup().await.unwrap();
    assert!(batch.items()[0].delete_requested);
    assert!(!batch.complete());
    batch.cleanup().await.unwrap();
    assert_eq!(deletes(&a).len(), 1);
    assert_eq!(
        a.object("ConfigMap", "a")["metadata"]["finalizers"],
        json!(["fixture.example/hold"])
    );
}
#[tokio::test]
async fn dependencies_are_explicit_rewritten_and_retained_on_move() {
    let config = config("settings");
    let workload = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"source","uid":"web-uid","resourceVersion":"1"},"spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"app","image":"example","envFrom":[{"configMapRef":{"name":"settings"}}]}]}}}});
    let (a, left, b, right) = fixtures(vec![config.clone(), workload.clone()]).await;
    let checked = Batch::check(
        left.clone(),
        right.clone(),
        vec![selection(&workload)],
        options(true),
    )
    .await;
    assert!(!checked.ready());
    assert_eq!(checked.dependencies().len(), 1);
    let mut dependency = selection(&config);
    dependency.copy_only = true;
    dependency.name = "settings-copy".into();
    let mut batch = Batch::check(
        left,
        right,
        vec![selection(&workload), dependency],
        options(true),
    )
    .await;
    assert!(
        batch.ready(),
        "{:?}",
        batch.items().iter().map(|i| &i.problem).collect::<Vec<_>>()
    );
    let web = batch
        .items()
        .iter()
        .find(|i| i.selection().name == "web")
        .unwrap();
    assert_eq!(
        web.preview()
            .unwrap()
            .pointer("/spec/template/spec/containers/0/envFrom/0/configMapRef/name")
            .unwrap(),
        "settings-copy"
    );
    batch.create().await.unwrap();
    assert!(!batch.can_cleanup());
    let mut ready = b.object("Deployment", "web");
    ready["metadata"]["generation"] = json!(1);
    ready["status"] = json!({"observedGeneration":1,"updatedReplicas":1,"availableReplicas":1});
    b.put(ready);
    batch.verify().await;
    assert!(batch.can_cleanup());
    batch.cleanup().await.unwrap();
    assert!(batch.complete());
    assert!(a.object("ConfigMap", "settings").is_object());
    assert_eq!(deletes(&a).len(), 1);
}
#[tokio::test]
async fn secret_consent_masking_and_generated_token_rejection() {
    let secret = json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"auth","namespace":"source","uid":"auth-uid","resourceVersion":"1"},"type":"Opaque","data":{"token":"c2VjcmV0"},"stringData":{"other":"do-not-render"}});
    let (_a, left, _b, right) = fixtures(vec![secret.clone()]).await;
    let batch = Batch::check(
        left.clone(),
        right.clone(),
        vec![selection(&secret)],
        options(false),
    )
    .await;
    assert!(!batch.ready());
    let preview = batch.items()[0].preview().unwrap().to_string();
    assert!(!preview.contains("c2VjcmV0"));
    assert!(!preview.contains("do-not-render"));
    let mut opts = options(false);
    opts.secret_consent = true;
    let batch = Batch::check(left, right, vec![selection(&secret)], opts).await;
    assert!(batch.ready());
    let mut token: DynamicObject = serde_json::from_value(secret).unwrap();
    token.data["type"] = json!("kubernetes.io/service-account-token");
    assert!(unsupported(&kind("", "Secret", "secrets"), Some(&token)).is_some());
}
#[test]
fn service_addresses_and_nested_metadata_are_normalized() {
    let object:DynamicObject=serde_json::from_value(json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"web","namespace":"source","uid":"old","resourceVersion":"4","finalizers":["example"],"annotations":{"meta.helm.sh/release-name":"old","kubectl.kubernetes.io/last-applied-configuration":"secret-history"}},"spec":{"selector":{"app":"web"},"clusterIP":"10.0.0.5","clusterIPs":["10.0.0.5"],"ipFamilies":["IPv4"],"healthCheckNodePort":30000,"ports":[{"port":80,"nodePort":30001}]},"status":{"loadBalancer":{}}})).unwrap();
    let value = manifest::prepare(
        &kind("", "Service", "services"),
        &object,
        "new-web",
        Some("target"),
    )
    .unwrap();
    assert!(value.get("status").is_none());
    assert!(value.pointer("/metadata/uid").is_none());
    assert!(value.pointer("/metadata/finalizers").is_none());
    assert!(value.pointer("/spec/clusterIP").is_none());
    assert!(value.pointer("/spec/ports/0/nodePort").is_none());
    assert!(!value.to_string().contains("secret-history"));
    let mut headless = object;
    headless.data["spec"]["clusterIP"] = json!("None");
    headless.data["spec"]["clusterIPs"] = json!(["None"]);
    let value = manifest::prepare(
        &kind("", "Service", "services"),
        &headless,
        "new-web",
        Some("target"),
    )
    .unwrap();
    assert_eq!(value["spec"]["clusterIP"], "None");
}

#[tokio::test]
async fn uncertain_posts_only_recover_the_same_operation_and_reviewed_configuration() {
    let object = config("a");
    let (a, left, b, right) = fixtures(vec![object.clone()]).await;
    let mut batch = Batch::check(left, right, vec![selection(&object)], options(true)).await;
    let mut target = batch.items[0].manifest.clone().unwrap();
    target["metadata"]["uid"] = json!("ours");
    target["metadata"]["resourceVersion"] = json!("1");
    b.put(target.clone());
    batch.items[0].target = TargetState::Uncertain;
    batch.create().await.unwrap();
    assert!(batch.items[0].verified);
    assert!(writes(&b, "POST /api/v1/namespaces/target/configmaps").is_empty());
    // A lost response is not permission to adopt a concurrently changed object.
    batch.items[0].target = TargetState::Uncertain;
    batch.items[0].target_data = None;
    target["data"]["config"] = json!("edited-after-post");
    b.put(target);
    batch.create().await.unwrap();
    assert!(!batch.can_cleanup());
    assert!(batch.cleanup().await.is_err());
    assert!(deletes(&a).is_empty());
}
#[tokio::test]
async fn api_mismatch_and_recreated_sources_fail_before_writes() {
    let object = config("a");
    let (a, left, b, _right) = fixtures(vec![object.clone()]).await;
    let right = Arc::new(ClusterSession::for_testing(
        ClusterId::new("no-api"),
        b.url.clone(),
        vec![],
    ));
    let batch = Batch::check(left.clone(), right, vec![selection(&object)], options(true)).await;
    assert!(!batch.ready());
    let right = Arc::new(ClusterSession::for_testing(
        ClusterId::new("api"),
        b.url.clone(),
        kinds(),
    ));
    let mut recreated = object.clone();
    recreated["metadata"]["uid"] = json!("replacement");
    a.put(recreated);
    let batch = Batch::check(left, right, vec![selection(&object)], options(true)).await;
    assert!(!batch.ready());
    assert!(writes(&b, "POST").is_empty());
}
#[tokio::test]
async fn cluster_roles_can_be_included_as_cluster_scoped_binding_dependencies() {
    let role = json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"ClusterRole","metadata":{"name":"reader","uid":"reader","resourceVersion":"1"},"rules":[{"verbs":["get"],"apiGroups":[""],"resources":["pods"]}]});
    let binding = json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":"read","namespace":"source","uid":"binding","resourceVersion":"1"},"roleRef":{"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"reader"},"subjects":[{"kind":"User","name":"example"}]});
    let (_a, left, _b, right) = fixtures(vec![role.clone(), binding.clone()]).await;
    let mut dependency = selection(&role);
    dependency.copy_only = true;
    let mut batch = Batch::check(
        left,
        right,
        vec![selection(&binding), dependency],
        options(true),
    )
    .await;
    assert!(
        batch.ready(),
        "{:?}",
        batch.items.iter().map(|i| &i.problem).collect::<Vec<_>>()
    );
    batch.create().await.unwrap();
    assert!(batch.can_cleanup());
    assert_eq!(
        batch
            .items
            .iter()
            .find(|i| i.selection.kind.resource.kind == "ClusterRole")
            .unwrap()
            .preview()
            .unwrap()
            .pointer("/metadata/namespace"),
        None
    );
}
