use super::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

struct Server {
    client: kube::Client,
    requests: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(replies: Vec<(u16, String)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = kube::Config::new(
            format!("http://{}", listener.local_addr().unwrap())
                .parse()
                .unwrap(),
        );
        config.default_retry = false;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            let last = replies.last().unwrap().clone();
            let mut replies = VecDeque::from(replies);
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut header = Vec::new();
                while !header.ends_with(b"\r\n\r\n") {
                    header.push(socket.read_u8().await.unwrap());
                    assert!(header.len() < 8192);
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(header).unwrap());
                let (code, body) = replies.pop_front().unwrap_or_else(|| last.clone());
                let reason = if code == 200 { "OK" } else { "Error" };
                socket.write_all(format!("HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            client: kube::Client::try_from(config).unwrap(),
            requests,
            task,
        }
    }
    fn follow(&self, options: LogOptions) -> impl Stream<Item = LogEvent> + use<> {
        follow(
            self.client.clone(),
            "default".into(),
            "demo".into(),
            options,
            &tokio::runtime::Handle::current(),
        )
    }
}

fn failure(code: u16, message: &str) -> (u16, String) {
    (code, json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"BadRequest","code":code,"message":message}).to_string())
}
fn waiting(reason: &str) -> (u16, String) {
    failure(
        400,
        &format!("container \"app\" in pod \"demo\" is waiting to start: {reason}"),
    )
}
async fn next(events: &mut (impl Stream<Item = LogEvent> + Unpin)) -> LogEvent {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("log event timed out")
        .expect("log stream closed without an event")
}

#[tokio::test]
async fn waiting_containers_retry_then_deliver_logs_with_the_selected_options() {
    let server = Server::start(vec![
        waiting("PodInitializing"),
        waiting("ContainerCreating"),
        (200, "2026-10-23T00:00:00Z started\n".into()),
    ])
    .await;
    let options = LogOptions {
        container: Some("app".into()),
        timestamps: true,
        since_time: Some("2026-10-22T23:00:00Z".parse().unwrap()),
        ..Default::default()
    };
    let mut events = server.follow(options);
    assert!(matches!(next(&mut events).await, LogEvent::Waiting));
    // Repeated startup responses do not produce duplicate wait events/errors.
    assert!(matches!(next(&mut events).await, LogEvent::Opened));
    let LogEvent::Lines(lines) = next(&mut events).await else {
        panic!("expected container output");
    };
    assert_eq!(lines, ["2026-10-23T00:00:00Z started"]);
    assert!(matches!(next(&mut events).await, LogEvent::Closed));
    assert!(events.next().await.is_none());
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    for request in requests.iter() {
        let uri = request.split_whitespace().nth(1).unwrap();
        let url = url::Url::parse(&format!("http://fixture{uri}")).unwrap();
        assert_eq!(url.path(), "/api/v1/namespaces/default/pods/demo/log");
        let query: std::collections::BTreeMap<_, _> = url.query_pairs().collect();
        assert_eq!(query.get("container").map(|s| s.as_ref()), Some("app"));
        assert_eq!(query.get("follow").map(|s| s.as_ref()), Some("true"));
        assert_eq!(query.get("timestamps").map(|s| s.as_ref()), Some("true"));
        assert_eq!(
            query.get("sinceTime").map(|s| s.as_ref()),
            Some("2026-10-22T23:00:00Z")
        );
    }
}

#[test]
fn only_kubelets_startup_responses_are_waiting() {
    for reason in [
        "ContainerCreating",
        "PodInitializing",
        "image can't be pulled",
        "trying and failing to pull image",
    ] {
        let error = kube::Error::Api(serde_json::from_str(&waiting(reason).1).unwrap());
        assert!(waiting_to_start(&error));
    }
    let error = kube::Error::Api(
        serde_json::from_str(
            &failure(
                400,
                "container \"app\" in pod \"demo\" is waiting to start - no logs yet",
            )
            .1,
        )
        .unwrap(),
    );
    assert!(waiting_to_start(&error));
}

#[tokio::test]
async fn permissions_missing_pods_bad_container_names_and_server_errors_still_fail() {
    for (code, message) in [
        (401, "authentication required"),
        (403, "pods/log is forbidden"),
        (404, "pods \"demo\" not found"),
        (400, "container app is not valid for pod demo"),
        (400, "a container name must be specified for pod demo"),
        (
            500,
            "container \"app\" in pod \"demo\" is waiting to start: unrelated server error",
        ),
    ] {
        let server = Server::start(vec![failure(code, message)]).await;
        let mut events = server.follow(LogOptions::default());
        let LogEvent::Failed(error) = next(&mut events).await else {
            panic!("{code}: must not wait on {message}");
        };
        assert!(error.contains(message), "{error}");
        assert!(events.next().await.is_none());
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn previous_logs_never_wait_for_a_new_container_to_start() {
    for reply in [
        waiting("ContainerCreating"),
        failure(
            400,
            "previous terminated container \"app\" in pod \"demo\" not found",
        ),
    ] {
        let server = Server::start(vec![reply]).await;
        let mut events = server.follow(LogOptions {
            previous: true,
            ..Default::default()
        });
        assert!(matches!(next(&mut events).await, LogEvent::Failed(_)));
        assert!(events.next().await.is_none());
        assert_eq!(server.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn empty_log_responses_leave_the_waiting_state() {
    let server = Server::start(vec![waiting("ContainerCreating"), (200, String::new())]).await;
    let mut events = server.follow(LogOptions::default());
    assert!(matches!(next(&mut events).await, LogEvent::Waiting));
    assert!(matches!(next(&mut events).await, LogEvent::Opened));
    assert!(matches!(next(&mut events).await, LogEvent::Closed));
}

#[tokio::test]
async fn genuine_errors_after_waiting_are_not_hidden_or_retried() {
    let server = Server::start(vec![
        waiting("ContainerCreating"),
        failure(403, "logs forbidden"),
    ])
    .await;
    let mut events = server.follow(LogOptions::default());
    assert!(matches!(next(&mut events).await, LogEvent::Waiting));
    assert!(
        matches!(next(&mut events).await, LogEvent::Failed(error) if error.contains("logs forbidden"))
    );
    assert!(events.next().await.is_none());
    assert_eq!(server.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn closing_a_waiting_log_pane_cancels_further_requests() {
    let server = Server::start(vec![waiting("ContainerCreating")]).await;
    let mut events = server.follow(LogOptions::default());
    assert!(matches!(next(&mut events).await, LogEvent::Waiting));
    drop(events);
    tokio::time::sleep(START_RETRY + Duration::from_millis(50)).await;
    assert_eq!(server.requests.lock().unwrap().len(), 1);
}
