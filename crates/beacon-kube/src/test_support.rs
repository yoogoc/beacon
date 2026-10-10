//! Local HTTP fixtures for API and foreground integration checks.
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::broadcast,
};

pub struct Fixture {
    pub url: String,
    pub requests: Arc<Mutex<Vec<(String, Value)>>>,
    objects: Arc<Mutex<Vec<Value>>>,
    refuse_exec: Arc<std::sync::atomic::AtomicBool>,
    events: broadcast::Sender<Value>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    pub async fn start(objects: Vec<Value>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let objects = Arc::new(Mutex::new(objects));
        let requests = Arc::new(Mutex::new(vec![]));
        let (events, _) = broadcast::channel(64);
        let store = objects.clone();
        let refuse_exec = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let exec_policy = refuse_exec.clone();
        let recorded = requests.clone();
        let changes = events.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream,_)) = accepted else { break; };
                        let store = store.clone(); let recorded = recorded.clone(); let changes = changes.clone(); let exec_policy = exec_policy.clone();
                        connections.spawn(async move { serve(stream,store,recorded,changes,exec_policy).await; });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            url,
            requests,
            objects,
            refuse_exec,
            events,
            task,
        }
    }
    pub fn refuse_exec(&self) {
        self.refuse_exec
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn put(&self, object: Value) {
        let mut objects = self.objects.lock().unwrap();
        objects.retain(|old| {
            old["apiVersion"] != object["apiVersion"]
                || old["kind"] != object["kind"]
                || old.pointer("/metadata/name") != object.pointer("/metadata/name")
                || old.pointer("/metadata/namespace") != object.pointer("/metadata/namespace")
        });
        objects.push(object.clone());
        drop(objects);
        let _ = self.events.send(json!({"type":"ADDED","object":object}));
    }
    pub fn watchers(&self) -> usize {
        self.events.receiver_count()
    }
    pub fn object(&self, kind: &str, name: &str) -> Value {
        self.objects
            .lock()
            .unwrap()
            .iter()
            .find(|object| {
                object["kind"] == kind
                    && object.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
            })
            .unwrap()
            .clone()
    }
}

async fn response(stream: &mut TcpStream, status: &str, body: String, content_type: &str) {
    let header = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(header.as_bytes()).await;
}
async fn serve(
    mut stream: TcpStream,
    objects: Arc<Mutex<Vec<Value>>>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    events: broadcast::Sender<Value>,
    refuse_exec: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        let Ok(byte) = stream.read_u8().await else {
            return;
        };
        header.push(byte);
        if header.len() > 16384 {
            return;
        }
    }
    let header = String::from_utf8(header).unwrap();
    let line = header.lines().next().unwrap_or("").to_owned();
    let length = header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, length)| length.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    if stream.read_exact(&mut body).await.is_err() {
        return;
    }
    let request = serde_json::from_slice::<Value>(&body).unwrap_or(Value::Null);
    requests
        .lock()
        .unwrap()
        .push((line.clone(), request.clone()));
    let mut words = line.split_whitespace();
    let method = words.next().unwrap_or("");
    let path = words.next().unwrap_or("/");
    let url = url::Url::parse(&format!("http://fixture{path}")).unwrap();
    if url.path().ends_with("/exec") && refuse_exec.load(std::sync::atomic::Ordering::Relaxed) {
        response(&mut stream,"403 Forbidden",json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","message":"Fixture refuses WebSocket exec","code":403}).to_string(),"application/json").await;
        return;
    }
    let query: std::collections::BTreeMap<_, _> = url
        .query_pairs()
        .map(|(a, b)| (a.into_owned(), b.into_owned()))
        .collect();
    let parts: Vec<_> = url
        .path()
        .split('/')
        .filter(|part| !part.is_empty())
        .collect();
    let start = if parts.first() == Some(&"apis") { 3 } else { 2 };
    let (index, namespace) = if parts.get(start) == Some(&"namespaces") && parts.len() > start + 2 {
        (start + 2, parts.get(start + 1).copied())
    } else {
        (start, None)
    };
    let resource = parts.get(index).copied().unwrap_or("");
    let name = parts.get(index + 1).copied();
    let kind = match resource {
        "pods" => "Pod",
        "services" => "Service",
        "endpointslices" => "EndpointSlice",
        "ingresses" => "Ingress",
        "deployments" => "Deployment",
        "replicasets" => "ReplicaSet",
        "namespaces" => "Namespace",
        "configmaps" => "ConfigMap",
        "secrets" => "Secret",
        _ => "Unknown",
    };
    let selector = query
        .get("labelSelector")
        .and_then(|query| crate::labels::LabelSelector::parse(query).ok());
    let matches = |object: &Value| {
        object["kind"] == kind
            && namespace.is_none_or(|namespace| {
                object
                    .pointer("/metadata/namespace")
                    .and_then(Value::as_str)
                    == Some(namespace)
            })
            && selector.as_ref().is_none_or(|selector| {
                let labels = object
                    .pointer("/metadata/labels")
                    .cloned()
                    .and_then(|labels| serde_json::from_value(labels).ok());
                selector.matches(labels.as_ref())
            })
    };
    if resource == "selfsubjectrulesreviews" {
        response(&mut stream,"200 OK",json!({"apiVersion":"authorization.k8s.io/v1","kind":"SelfSubjectRulesReview","status":{"incomplete":false,"resourceRules":[{"verbs":["*"],"apiGroups":["*"],"resources":["*"]}],"nonResourceRules":[]}}).to_string(),"application/json").await;
        return;
    }
    if query.get("watch").is_some_and(|watch| watch == "true") {
        let mut receiver = events.subscribe();
        if stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n").await.is_err() { return; }
        loop {
            let mut closed = [0];
            let event = tokio::select! { event = receiver.recv() => match event {Ok(event)=>event,Err(_)=>return}, _ = stream.read(&mut closed) => return };
            if matches(&event["object"]) {
                let body = format!("{event}\n");
                let chunk = format!("{:x}\r\n{body}\r\n", body.len());
                if stream.write_all(chunk.as_bytes()).await.is_err() {
                    return;
                }
            }
        }
    }
    if parts.last() == Some(&"log") {
        response(
            &mut stream,
            "200 OK",
            "2026-10-10T00:00:00Z fixture log\n".into(),
            "text/plain",
        )
        .await;
        return;
    }
    let listed: Vec<_> = objects
        .lock()
        .unwrap()
        .iter()
        .filter(|object| matches(object))
        .cloned()
        .collect();
    let Some(name) = name else {
        response(&mut stream,"200 OK",json!({"apiVersion":"v1","kind":"List","metadata":{"resourceVersion":"1"},"items":listed}).to_string(),"application/json").await;
        return;
    };
    let Some(mut object) = listed
        .into_iter()
        .find(|object| object.pointer("/metadata/name").and_then(Value::as_str) == Some(name))
    else {
        response(&mut stream,"404 Not Found",json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":404,"message":"fixture resource missing","reason":"NotFound"}).to_string(),"application/json").await;
        return;
    };
    if method == "PATCH" {
        if request.pointer("/metadata/uid") != object.pointer("/metadata/uid")
            || request.pointer("/metadata/resourceVersion")
                != object.pointer("/metadata/resourceVersion")
        {
            response(&mut stream,"409 Conflict",json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":409,"reason":"Conflict","message":"resource changed after preview"}).to_string(),"application/json").await;
            return;
        }
        if parts.last() == Some(&"ephemeralcontainers") {
            let containers = request
                .pointer("/spec/ephemeralContainers")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if !object["spec"]["ephemeralContainers"].is_array() {
                object["spec"]["ephemeralContainers"] = json!([]);
            }
            let statuses = containers.iter().map(|container|json!({"name":container["name"],"restartCount":0,"ready":true,"image":container["image"],"imageID":"fixture","state":{"running":{}}})).collect::<Vec<_>>();
            object["spec"]["ephemeralContainers"]
                .as_array_mut()
                .unwrap()
                .extend(containers);
            object["status"]["ephemeralContainerStatuses"] = json!(statuses);
        } else if let Some(template) = request.pointer("/spec/template") {
            let mut template = template.clone();
            template.as_object_mut().unwrap().remove("$patch");
            object["spec"]["template"] = template;
        } else if matches!(kind, "ConfigMap" | "Secret") {
            if request["apiVersion"] != object["apiVersion"]
                || request["kind"] != object["kind"]
                || request.pointer("/metadata/managedFields").is_some()
            {
                response(&mut stream,"422 Unprocessable Entity",json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":422,"reason":"Invalid","message":"apply requires apiVersion and kind without managedFields"}).to_string(),"application/json").await;
                return;
            }
            for field in ["data", "binaryData"] {
                if let Some(values) = request.get(field) {
                    object[field] = values.clone();
                }
            }
        }
        object["metadata"]["resourceVersion"] = json!("2");
        {
            let mut store = objects.lock().unwrap();
            let existing = store
                .iter_mut()
                .find(|existing| {
                    existing["kind"] == kind
                        && existing.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
                        && matches(existing)
                })
                .unwrap();
            *existing = object.clone();
        }
        let _ = events.send(json!({"type":"MODIFIED","object":object.clone()}));
    }
    response(
        &mut stream,
        "200 OK",
        object.to_string(),
        "application/json",
    )
    .await;
}

/// Isolated container-file fixture. Its kubectl substitute executes only against
/// this temporary directory; no host kubeconfig or live API is consulted.
#[cfg(unix)]
pub struct FileFixture {
    directory: tempfile::TempDir,
    pub root: std::path::PathBuf,
    pub program: std::path::PathBuf,
}
#[cfg(unix)]
impl Default for FileFixture {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg(unix)]
impl FileFixture {
    pub fn new() -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("container");
        std::fs::create_dir(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        let tools = directory.path().join("tools");
        std::fs::create_dir(&tools).unwrap();
        let shim = r#"#!/usr/bin/env python3
import os, sys, stat
p=sys.argv[-1]
if os.path.basename(sys.argv[0]) == 'mv':
    source,dest=sys.argv[-2:]
    if '-n' in sys.argv and os.path.lexists(dest): sys.exit(0)
    os.rename(source,dest); sys.exit(0)
s=os.lstat(p)
if os.path.basename(sys.argv[0]) == 'readlink':
    sys.stdout.write(os.readlink(p))
else:
    if '%F' not in sys.argv[2]: print(s.st_size)
    else:
        kind='directory' if stat.S_ISDIR(s.st_mode) else 'symbolic link' if stat.S_ISLNK(s.st_mode) else 'regular file' if stat.S_ISREG(s.st_mode) else 'fifo'
        print(f'{kind}|{s.st_size}|{int(s.st_mtime)}|{stat.S_IMODE(s.st_mode):o}|{s.st_uid}:{s.st_gid}')
"#;
        for tool in ["stat", "readlink", "mv"] {
            let p = tools.join(tool);
            std::fs::write(&p, shim).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let program = directory.path().join("kubectl-fixture");
        let script = format!(
            r#"#!/usr/bin/env python3
import os,sys,json
root={root}
args=sys.argv[1:]
assert '--context' in args and '--kubeconfig' in args and '--container' in args
assert '-t' not in args and os.environ.get('KUBECTL_REMOTE_COMMAND_WEBSOCKETS')=='false'
assert os.path.isfile(args[args.index('--kubeconfig')+1])
argv=args[args.index('--')+1:]
assert argv[:2]==['sh','-c']
for i,value in enumerate(argv[4:],4):
    if value.startswith('/'):
        argv[i]=value if value.startswith(root) else root+'/'+value.lstrip('/')
os.environ['PATH']={tools}+os.pathsep+os.environ['PATH']
os.environ['PYTHONDONTWRITEBYTECODE']='1'
with open({calls},'a') as f: f.write(json.dumps(argv)+'\n')
os.execv('/bin/sh',argv)
"#,
            root = serde_json::to_string(root.to_str().unwrap()).unwrap(),
            tools = serde_json::to_string(tools.to_str().unwrap()).unwrap(),
            calls = serde_json::to_string(directory.path().join("calls.jsonl").to_str().unwrap())
                .unwrap()
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            directory,
            root,
            program,
        }
    }
    pub fn calls(&self) -> String {
        std::fs::read_to_string(self.directory.path().join("calls.jsonl")).unwrap_or_default()
    }
    pub fn pod(&self) -> Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"file-pod","namespace":"default","uid":"file-uid","resourceVersion":"1"},"spec":{"containers":[{"name":"app","image":"fixture"},{"name":"sidecar","image":"fixture"}],"initContainers":[{"name":"finished-init","image":"fixture"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","image":"fixture","imageID":"fixture","ready":true,"restartCount":0,"containerID":"fixture://app","state":{"running":{}}},{"name":"sidecar","image":"fixture","imageID":"fixture","ready":true,"restartCount":0,"containerID":"fixture://sidecar","state":{"running":{}}}],"initContainerStatuses":[{"name":"finished-init","image":"fixture","imageID":"fixture","ready":false,"restartCount":0,"containerID":"fixture://init","state":{"terminated":{"exitCode":0}}}]}})
    }
}
