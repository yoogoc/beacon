use super::*;

#[tokio::test]
#[ignore = "requires the local Postman debug-container simulation"]
async fn postman_simulation_stops_only_the_requested_debug_container_and_confirms_status() {
    let base =
        std::env::var("BEACON_DEBUG_SIMULATION_URL").expect("local /kubernetes simulation URL");
    let session = Arc::new(ClusterSession::for_testing(
        crate::ClusterId::new("debug-stop-simulation"),
        base,
        vec![],
    ));
    for name in ["stop-ready", "stop-denied", "stop-stuck"] {
        let object: Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":format!("uid-{name}")}})).unwrap());
        create(
            session.clone(),
            object.clone(),
            Options {
                name: "debugger".into(),
                image: "busybox:stable".into(),
                target: Some("app".into()),
                shell: "/bin/sh".into(),
                profile: Profile::Restricted,
            },
        )
        .await
        .unwrap();
        let result = stop(session.clone(), object.clone(), "debugger".into()).await;
        if name == "stop-ready" {
            assert_eq!(result.unwrap(), StopOutcome::Stopped);
            assert_eq!(
                stop(session.clone(), object, "debugger".into())
                    .await
                    .unwrap(),
                StopOutcome::AlreadyStopped
            );
        } else if name == "stop-stuck" {
            assert!(
                result
                    .unwrap_err()
                    .user_message()
                    .contains("Timed out stopping")
            );
        } else {
            assert!(result.is_err());
        }
    }
}

fn pod(command: Vec<String>, state: Value) -> Value {
    json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"web","namespace":"default","uid":"pod-uid","resourceVersion":"42"},"spec":{"containers":[{"name":"app","image":"fixture"}],"ephemeralContainers":[{"name":"debugger","image":"busybox","command":command}]},"status":{"phase":"Running","ephemeralContainerStatuses":[{"name":"debugger","image":"busybox","imageID":"fixture","ready":false,"restartCount":0,"state":state}]}})
}

#[test]
fn only_the_exact_managed_command_can_be_stopped() {
    let mut container = json!({"name":"debugger","command":command("/bin/sh", "debugger")});
    assert!(supports_stop(&container));
    container["args"] = json!(["extra"]);
    assert!(!supports_stop(&container));
    container.as_object_mut().unwrap().remove("args");
    container["command"][4] = json!("/tmp/other-container");
    assert!(!supports_stop(&container));
    container["command"] = json!(["/bin/sh", "-c", "while true; do sleep 3600; done"]);
    assert!(!supports_stop(&container));
}

#[tokio::test]
async fn replaced_pods_normal_containers_and_legacy_commands_cannot_receive_stop_exec() {
    let original = pod(command("/bin/sh", "debugger"), json!({"running":{}}));
    let fixture = crate::test_support::Fixture::start(vec![original.clone()]).await;
    let session = Arc::new(ClusterSession::for_testing(
        crate::ClusterId::new("stop-guards"),
        fixture.url.clone(),
        vec![],
    ));
    let object: Arc<DynamicObject> = Arc::new(serde_json::from_value(original.clone()).unwrap());
    assert!(
        stop(session.clone(), object.clone(), "app".into())
            .await
            .unwrap_err()
            .user_message()
            .contains("ephemeral container")
    );
    let legacy = pod(
        vec![
            "/bin/sh".into(),
            "-c".into(),
            "while true; do sleep 3600; done".into(),
        ],
        json!({"running":{}}),
    );
    fixture.put(legacy);
    assert!(
        stop(session.clone(), object.clone(), "debugger".into())
            .await
            .unwrap_err()
            .user_message()
            .contains("safe stop support")
    );
    let mut replaced = original;
    replaced["metadata"]["uid"] = json!("replacement");
    fixture.put(replaced);
    assert!(
        stop(session, object, "debugger".into())
            .await
            .unwrap_err()
            .user_message()
            .contains("replaced")
    );
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(line, _)| line.starts_with("GET ") && !line.contains("/exec"))
    );
}

#[tokio::test]
async fn already_stopped_is_idempotent_and_waiting_is_not_reported_as_success() {
    let original = pod(
        command("/bin/sh", "debugger"),
        json!({"terminated":{"exitCode":0}}),
    );
    let fixture = crate::test_support::Fixture::start(vec![original.clone()]).await;
    let session = Arc::new(ClusterSession::for_testing(
        crate::ClusterId::new("stop-idempotent"),
        fixture.url.clone(),
        vec![],
    ));
    let object: Arc<DynamicObject> = Arc::new(serde_json::from_value(original).unwrap());
    assert_eq!(
        stop(session.clone(), object.clone(), "debugger".into())
            .await
            .unwrap(),
        StopOutcome::AlreadyStopped
    );
    fixture.put(pod(
        command("/bin/sh", "debugger"),
        json!({"waiting":{"reason":"ContainerCreating"}}),
    ));
    assert!(
        stop(session, object, "debugger".into())
            .await
            .unwrap_err()
            .user_message()
            .contains("not running yet")
    );
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(line, _)| !line.contains("/exec"))
    );
}

#[cfg(unix)]
#[test]
fn the_actual_keepalive_exits_on_its_own_stop_request_without_touching_another_container() {
    use std::{process::Command, time::Instant};
    let directory = tempfile::tempdir().unwrap();
    let own = directory.path().join("own");
    let other = directory.path().join("other");
    let launch = |control: &std::path::Path| {
        Command::new("/bin/sh")
            .args(["-c", KEEPALIVE, MARKER])
            .arg(control)
            .spawn()
            .unwrap()
    };
    let mut first = launch(&own);
    let mut second = launch(&other);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !(own.is_dir() && other.is_dir()) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = Command::new("/bin/sh")
        .args(["-c", STOP, MARKER])
        .arg(&own)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), ACK);
    while first.try_wait().unwrap().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    let stopped = first
        .try_wait()
        .unwrap()
        .is_some_and(|status| status.success());
    let other_running = second.try_wait().unwrap().is_none();
    let _ = first.kill();
    let _ = second.kill();
    let _ = first.wait();
    let _ = second.wait();
    assert!(
        stopped,
        "the cooperative request must stop the actual main process"
    );
    assert!(
        other_running,
        "another container's keepalive must remain running"
    );
    assert!(!own.exists());
}
