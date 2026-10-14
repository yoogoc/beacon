use super::*;
use crate::test_support::Fixture;
use serde_json::json;
fn pod(name: &str, tier: &str, ready: bool) -> serde_json::Value {
    json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"team","uid":name,"labels":{"app":"web","tier":tier}},"spec":{"containers":[{"name":"app","image":"fixture","ports":[{"name":"http","containerPort":8080,"protocol":"TCP"}]}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":if ready {"True"} else {"False"}}]}})
}
fn service() -> serde_json::Value {
    json!({"apiVersion":"v1","kind":"Service","metadata":{"name":"web","namespace":"team"},"spec":{"selector":{"app":"web"},"ports":[{"port":80,"targetPort":"http","protocol":"TCP"},{"port":53,"protocol":"UDP"}]}})
}
fn client(fixture: &Fixture) -> kube::Client {
    kube::Client::try_from(kube::Config::new(fixture.url.parse().unwrap())).unwrap()
}
#[tokio::test]
async fn service_named_ports_and_deployment_expressions_choose_only_ready_selected_pods() {
    let fixture=Fixture::start(vec![service(),pod("a-unready","frontend",false),pod("b-backend","backend",true),pod("c-frontend","frontend",true),json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"team"},"spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"tier","operator":"In","values":["frontend"]}]},"template":{"spec":{"containers":[]}}}})]).await;
    let target = Target {
        kind: TargetKind::Service,
        namespace: "team".into(),
        name: "web".into(),
        port: 80,
    };
    let route = target.resolve(client(&fixture)).await.unwrap();
    assert_eq!(route.pod, "b-backend");
    assert_eq!(route.port, 8080);
    let target = Target {
        kind: TargetKind::Deployment,
        port: 9000,
        ..target
    };
    let route = target.resolve(client(&fixture)).await.unwrap();
    assert_eq!(route.pod, "c-frontend");
    assert_eq!(route.port, 9000);
    let requests = fixture.requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .all(|(request, _)| request.starts_with("GET"))
    );
    assert!(
        requests
            .iter()
            .any(|(request, _)| request.contains("labelSelector=") && request.contains("tier"))
    );
}
#[tokio::test]
async fn invalid_service_ports_selectors_and_permissions_do_not_fall_back_to_arbitrary_pods() {
    let fixture = Fixture::start(vec![service(), pod("web-1", "frontend", true)]).await;
    let mut target = Target {
        kind: TargetKind::Service,
        namespace: "team".into(),
        name: "web".into(),
        port: 53,
    };
    assert!(
        target
            .resolve(client(&fixture))
            .await
            .unwrap_err()
            .to_string()
            .contains("TCP")
    );
    target.port = 80;
    fixture.deny("pods");
    assert!(
        target
            .resolve(client(&fixture))
            .await
            .unwrap_err()
            .user_message()
            .contains("403")
    );
    let mut no_selector = service();
    no_selector["spec"]["selector"] = json!({});
    fixture.put(no_selector);
    assert!(
        target
            .resolve(client(&fixture))
            .await
            .unwrap_err()
            .to_string()
            .contains("selector")
    );
}
#[tokio::test]
async fn listener_survives_backend_replacement_conflicts_are_clear_and_disconnect_prevents_restore()
{
    let fixture = Fixture::start(vec![service(), pod("web-1", "frontend", true)]).await;
    let forwards = Forwards::default();
    let runtime = tokio::runtime::Handle::current();
    let target = Target {
        kind: TargetKind::Service,
        namespace: "team".into(),
        name: "web".into(),
        port: 80,
    };
    let opened = forwards
        .open_target(client(&fixture), &runtime, target.clone(), 0)
        .await
        .unwrap();
    assert!(
        forwards
            .open_target(
                client(&fixture),
                &runtime,
                target.clone(),
                opened.local_port
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("already in use")
    );
    forwards.close_for_pod("team", "web-1");
    assert_eq!(forwards.list().len(), 1);
    fixture.put(pod("web-1", "frontend", false));
    fixture.put(pod("web-2", "frontend", true));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if forwards.list()[0].pod == "web-2" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(forwards.list()[0].local_port, opened.local_port);
    fixture.put(pod("web-2", "frontend", false));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if forwards.list()[0].reconnecting.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fixture.put(pod("web-3", "frontend", true));
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if forwards.list()[0].pod == "web-3" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(forwards.list()[0].reconnecting.is_none());
    forwards.stop_all();
    tokio::task::yield_now().await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, opened.local_port))
        .await
        .unwrap();
    drop(listener);
    assert!(
        forwards
            .open_target(client(&fixture), &runtime, target, opened.local_port)
            .await
            .is_err()
    );
    assert!(forwards.is_empty());
}
