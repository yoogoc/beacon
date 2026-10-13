use super::*;
use crate::{ApiResource, ClusterId, GroupVersionKind, Kind};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;

use crate::test_support::HelmFixture as Fake;
fn stored(number: i64) -> Value {
    let payload = json!({"name":"web","namespace":"default","version":number,"info":{"status":"deployed"},"chart":{"metadata":{"name":"web","version":"1.0.0","appVersion":"v1"}},"config":{"password":"private-password"},"manifest":"apiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: web\nspec:\n  replicas: 1\n"});
    json!({"apiVersion":"v1","kind":"Secret","type":"helm.sh/release.v1","metadata":{"name":format!("sh.helm.release.v1.web.v{number}"),"namespace":"default","labels":{"owner":"helm","name":"web","version":number.to_string()}},"data":{"release":STANDARD.encode(STANDARD.encode(payload.to_string()))}})
}
fn request(change: Change) -> Request {
    Request {
        identity: Identity {
            name: "web".into(),
            namespace: "default".into(),
        },
        change,
        values: json!({"password":"private-password","replicas":2}),
    }
}
fn chart_fixture() -> Chart {
    Chart {
        reference: "repo/web".into(),
        version: "2.0.0".into(),
        repository: String::new(),
    }
}
async fn fixture(
    fake: &Fake,
    objects: Vec<Value>,
) -> (crate::test_support::Fixture, Arc<ClusterSession>) {
    let fixture = crate::test_support::Fixture::start(objects).await;
    let kind = Kind {
        resource: ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("apps", "v1", "Deployment"),
            "deployments",
        ),
        namespaced: true,
        verbs: vec![],
    };
    let session = Arc::new(
        ClusterSession::for_testing(
            ClusterId::new("fixture-context"),
            fixture.url.clone(),
            vec![kind],
        )
        .with_helm_test_program(fake.program.clone()),
    );
    (fixture, session)
}
#[tokio::test]
async fn install_is_frozen_reviewed_and_pins_kubeconfig_context_proxy_and_values() {
    let fake = Fake::new();
    let (_api, session) = fixture(&fake, vec![]).await;
    assert_eq!(
        versions(session.clone(), "default".into(), "repo/web".into())
            .await
            .unwrap()[0]
            .version,
        "2.0.0"
    );
    let plan = Arc::new(
        prepare(session.clone(), request(Change::Install(chart_fixture())))
            .await
            .unwrap(),
    );
    assert_eq!(plan.baseline, 0);
    assert!(plan.after_manifest.contains("replicas: 2"));
    let before = std::fs::read_to_string(&fake.records).unwrap();
    assert!(!before.lines().any(|line| {
        let call: Value = serde_json::from_str(line).unwrap();
        call["args"]
            .as_array()
            .unwrap()
            .iter()
            .any(|arg| arg == "--wait")
    }));
    let (sender, mut events) = mpsc::unbounded();
    execute(session, plan.clone(), sender).await.unwrap();
    let calls = std::fs::read_to_string(&fake.records).unwrap();
    for call in calls
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
    {
        assert!(
            call["args"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "fixture-context")
        );
        assert_eq!(call["proxy"], "");
        assert!(call["config"].as_str().unwrap().contains("fixture-context"));
    }
    assert!(calls.contains(plan.archive.as_ref().unwrap().to_str().unwrap()));
    assert!(calls.contains(plan.values.as_ref().unwrap().to_str().unwrap()));
    assert!(!calls.contains("private-password"));
    use futures::StreamExt as _;
    assert!(events.next().await.unwrap().contains("Checking"));
}
#[tokio::test]
async fn changed_release_and_command_failure_do_not_silently_succeed() {
    let fake = Fake::new();
    let (api, session) = fixture(&fake, vec![stored(1)]).await;
    let plan = Arc::new(
        prepare(session.clone(), request(Change::Upgrade(chart_fixture())))
            .await
            .unwrap(),
    );
    api.put(stored(2));
    let (sender, _) = mpsc::unbounded();
    assert!(
        execute(session.clone(), plan, sender)
            .await
            .unwrap_err()
            .to_string()
            .contains("changed after review")
    );
    let plan = Arc::new(
        prepare(session.clone(), request(Change::Upgrade(chart_fixture())))
            .await
            .unwrap(),
    );
    std::fs::write(fake.directory.path().join("fail"), "").unwrap();
    let (sender, _) = mpsc::unbounded();
    let error = execute(session, plan, sender)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("mocked permission failure") && !error.contains("private-password"));
}
#[tokio::test]
async fn rollback_and_uninstall_are_explicit_and_keep_history() {
    let fake = Fake::new();
    let (_api, session) = fixture(&fake, vec![stored(1), stored(3)]).await;
    let rollback = Arc::new(
        prepare(session.clone(), request(Change::Rollback(1)))
            .await
            .unwrap(),
    );
    assert_eq!(rollback.baseline, 3);
    let (sender, _) = mpsc::unbounded();
    execute(session.clone(), rollback, sender).await.unwrap();
    let uninstall = Arc::new(
        prepare(session.clone(), request(Change::Uninstall))
            .await
            .unwrap(),
    );
    assert!(uninstall.after_manifest.is_empty());
    let (sender, _) = mpsc::unbounded();
    execute(session, uninstall, sender).await.unwrap();
    let calls = std::fs::read_to_string(fake.records).unwrap();
    assert!(
        calls.contains("rollback")
            && calls.contains("uninstall")
            && calls.contains("--keep-history")
    );
}
#[tokio::test]
async fn missing_helm_invalid_names_and_wrong_versions_are_actionable() {
    let fake = Fake::new();
    let (_api, session) = fixture(&fake, vec![]).await;
    let mut invalid = request(Change::Install(chart_fixture()));
    invalid.identity.name = "--another-release".into();
    assert!(prepare(session.clone(), invalid).await.is_err());
    let mut wrong = chart_fixture();
    wrong.version = "9.0.0".into();
    assert!(
        prepare(session, request(Change::Install(wrong)))
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("differs")
    );
    let fixture = crate::test_support::Fixture::start(vec![]).await;
    let session = Arc::new(
        ClusterSession::for_testing(ClusterId::new("no-helm"), fixture.url.clone(), vec![])
            .with_helm_test_program(fake.directory.path().join("missing")),
    );
    assert!(
        prepare(session, request(Change::Install(chart_fixture())))
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("Install Helm")
    );
}
