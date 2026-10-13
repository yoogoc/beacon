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
    denied: Arc<Mutex<std::collections::HashSet<String>>>,
    rules: Arc<Mutex<Value>>,
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
        let denied = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let denied_resources = denied.clone();
        let rules = Arc::new(Mutex::new(
            json!([{"verbs":["*"],"apiGroups":["*"],"resources":["*"]}]),
        ));
        let granted_rules = rules.clone();
        let recorded = requests.clone();
        let changes = events.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream,_)) = accepted else { break; };
                        let store = store.clone(); let recorded = recorded.clone(); let changes = changes.clone(); let exec_policy = exec_policy.clone(); let denied_resources = denied_resources.clone(); let granted_rules = granted_rules.clone();
                        connections.spawn(async move { serve(stream,store,recorded,changes,exec_policy,denied_resources,granted_rules).await; });
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
            denied,
            rules,
            events,
            task,
        }
    }
    pub fn refuse_exec(&self) {
        self.refuse_exec
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn deny(&self, resource: &str) {
        self.denied.lock().unwrap().insert(resource.to_owned());
    }
    pub fn rules(&self, resource_rules: Value) {
        *self.rules.lock().unwrap() = resource_rules;
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

/// Only the RFC 6902 operations used by reviewed Workflow changes are needed by
/// this fixture. Apply them to a private copy so failed tests never partially
/// persist a patch, just as an API server rejects the entire request.
fn workflow_patch(object: &mut Value, operations: &Value) -> std::result::Result<(), String> {
    let operations = operations.as_array().ok_or("JSON Patch must be an array")?;
    for operation in operations {
        let path = operation["path"]
            .as_str()
            .ok_or("JSON Patch path is missing")?;
        let action = operation["op"]
            .as_str()
            .ok_or("JSON Patch operation is missing")?;
        if action == "test" {
            if object.pointer(path) != operation.get("value") {
                return Err(format!("JSON Patch test failed at {path}"));
            }
            continue;
        }
        let (parent, key) = path.rsplit_once('/').ok_or("JSON Patch path is invalid")?;
        let key = key.replace("~1", "/").replace("~0", "~");
        let parent = object
            .pointer_mut(parent)
            .ok_or("JSON Patch parent does not exist")?;
        let value = operation.get("value").cloned();
        match (action, parent) {
            ("add", Value::Object(map)) => {
                map.insert(key, value.ok_or("JSON Patch value is missing")?);
            }
            ("remove", Value::Object(map)) => {
                map.remove(&key).ok_or("JSON Patch member does not exist")?;
            }
            ("add", Value::Array(array)) => {
                let index = if key == "-" {
                    array.len()
                } else {
                    key.parse().map_err(|_| "JSON Patch index is invalid")?
                };
                if index > array.len() {
                    return Err("JSON Patch array index is out of bounds".into());
                }
                array.insert(index, value.ok_or("JSON Patch value is missing")?);
            }
            ("remove", Value::Array(array)) => {
                let index: usize = key.parse().map_err(|_| "JSON Patch index is invalid")?;
                if index >= array.len() {
                    return Err("JSON Patch array index is out of bounds".into());
                }
                array.remove(index);
            }
            _ => return Err("unsupported JSON Patch operation in fixture".into()),
        }
    }
    Ok(())
}
async fn serve(
    mut stream: TcpStream,
    objects: Arc<Mutex<Vec<Value>>>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    events: broadcast::Sender<Value>,
    refuse_exec: Arc<std::sync::atomic::AtomicBool>,
    denied: Arc<Mutex<std::collections::HashSet<String>>>,
    rules: Arc<Mutex<Value>>,
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
    if denied.lock().unwrap().contains(resource) {
        response(&mut stream, "403 Forbidden", json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","message":format!("Fixture refuses {resource}"),"code":403}).to_string(), "application/json").await;
        return;
    }
    let mutation_refused = {
        let denied = denied.lock().unwrap();
        let verb = method.to_ascii_lowercase();
        denied.contains(&format!("{verb}/{resource}"))
            || name.is_some_and(|name| denied.contains(&format!("{verb}/{resource}/{name}")))
    };
    if mutation_refused {
        response(&mut stream, "403 Forbidden", json!({"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","message":format!("Fixture refuses {method} {resource}"),"code":403}).to_string(), "application/json").await;
        return;
    }
    let kind = match resource {
        "pods" if parts.get(1) == Some(&"metrics.k8s.io") => "PodMetrics",
        "nodes" if parts.get(1) == Some(&"metrics.k8s.io") => "NodeMetrics",
        "pods" => "Pod",
        "nodes" => "Node",
        "events" => "Event",
        "daemonsets" => "DaemonSet",
        "statefulsets" => "StatefulSet",
        "services" => "Service",
        "endpointslices" => "EndpointSlice",
        "ingresses" => "Ingress",
        "deployments" => "Deployment",
        "replicasets" => "ReplicaSet",
        "namespaces" => "Namespace",
        "configmaps" => "ConfigMap",
        "secrets" => "Secret",
        "roles" => "Role",
        "rolebindings" => "RoleBinding",
        "clusterroles" => "ClusterRole",
        "clusterrolebindings" => "ClusterRoleBinding",
        "jobs" => "Job",
        "cronjobs" => "CronJob",
        "poddisruptionbudgets" => "PodDisruptionBudget",
        "persistentvolumeclaims" => "PersistentVolumeClaim",
        "persistentvolumes" => "PersistentVolume",
        "workflows" => "Workflow",
        "workflowtemplates" => "WorkflowTemplate",
        "clusterworkflowtemplates" => "ClusterWorkflowTemplate",
        "cronworkflows" => "CronWorkflow",
        _ => "Unknown",
    };
    let selector = query
        .get("labelSelector")
        .and_then(|query| crate::labels::LabelSelector::parse(query).ok());
    let node_field = query
        .get("fieldSelector")
        .and_then(|selector| selector.strip_prefix("spec.nodeName="));
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
            && node_field.is_none_or(|node| {
                object.pointer("/spec/nodeName").and_then(Value::as_str) == Some(node)
            })
    };
    if resource == "selfsubjectrulesreviews" {
        let granted = rules.lock().unwrap().clone();
        response(&mut stream,"200 OK",json!({"apiVersion":"authorization.k8s.io/v1","kind":"SelfSubjectRulesReview","status":{"incomplete":false,"resourceRules":granted,"nonResourceRules":[]}}).to_string(),"application/json").await;
        return;
    }
    if matches!(
        resource,
        "selfsubjectaccessreviews" | "subjectaccessreviews" | "selfsubjectreviews"
    ) {
        let kind = match resource {
            "selfsubjectaccessreviews" => "SelfSubjectAccessReview",
            "subjectaccessreviews" => "SubjectAccessReview",
            _ => "SelfSubjectReview",
        };
        let status=objects.lock().unwrap().iter().find(|object|object["kind"]==kind).map(|object|object["status"].clone()).unwrap_or_else(|| if kind=="SelfSubjectReview"{json!({"userInfo":{"username":"alice","groups":["team","system:authenticated"]}})}else{json!({"allowed":true,"reason":"Fixture permission granted"})});
        response(&mut stream,"200 OK",json!({"apiVersion":if kind=="SelfSubjectReview"{"authentication.k8s.io/v1"}else{"authorization.k8s.io/v1"},"kind":kind,"spec":request["spec"],"status":status}).to_string(),"application/json").await;
        return;
    }
    if method == "POST" && resource == "pods" && parts.get(index + 2) == Some(&"eviction") {
        let name = name.unwrap_or_default();
        let refused = {
            let policy = denied.lock().unwrap();
            policy.contains("pods/eviction") || policy.contains(&format!("pods/{name}/eviction"))
        };
        if refused {
            response(&mut stream,"403 Forbidden",json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":403,"message":"Fixture refuses pods/eviction"}).to_string(),"application/json").await;
            return;
        }
        let object = objects
            .lock()
            .unwrap()
            .iter()
            .find(|object| {
                matches(object)
                    && object.pointer("/metadata/name").and_then(Value::as_str) == Some(name)
            })
            .cloned();
        let Some(object) = object else {
            response(&mut stream,"404 Not Found",json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"NotFound","code":404,"message":"Pod already gone"}).to_string(),"application/json").await;
            return;
        };
        if request["apiVersion"] != "policy/v1"
            || request["kind"] != "Eviction"
            || request.pointer("/deleteOptions/preconditions/uid")
                != object.pointer("/metadata/uid")
        {
            response(&mut stream,"409 Conflict",json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","code":409,"message":"Eviction requires reviewed UID in deleteOptions"}).to_string(),"application/json").await;
            return;
        }
        let pod: crate::DynamicObject = serde_json::from_value(object.clone()).unwrap();
        let budgets: Vec<crate::DynamicObject> = objects
            .lock()
            .unwrap()
            .iter()
            .filter(|object| object["kind"] == "PodDisruptionBudget")
            .filter_map(|object| serde_json::from_value(object.clone()).ok())
            .filter(|pdb| crate::node_maintenance::pdb_matches(pdb, &pod))
            .collect();
        if budgets.len() > 1 {
            response(&mut stream,"500 Internal Server Error",json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"InternalError","code":500,"message":"multiple PodDisruptionBudgets match this Pod"}).to_string(),"application/json").await;
            return;
        }
        if budgets
            .iter()
            .any(|pdb| crate::node_maintenance::pdb_blocks(pdb, &pod))
        {
            response(&mut stream,"429 Too Many Requests",json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"TooManyRequests","code":429,"message":"PodDisruptionBudget blocks eviction"}).to_string(),"application/json").await;
            return;
        }
        if !query.contains_key("dryRun") {
            objects.lock().unwrap().retain(|existing| {
                !matches(existing)
                    || existing.pointer("/metadata/uid") != object.pointer("/metadata/uid")
            });
            let _ = events.send(json!({"type":"DELETED","object":object}));
        }
        response(
            &mut stream,
            "201 Created",
            json!({"apiVersion":"v1","kind":"Status","status":"Success","code":201}).to_string(),
            "application/json",
        )
        .await;
        return;
    }
    if method == "POST" && resource == "jobs" && name.is_none() {
        let mut object = request.clone();
        if object["apiVersion"] != "batch/v1"
            || object["kind"] != "Job"
            || object.pointer("/spec/selector").is_some()
            || object.pointer("/metadata/uid").is_some()
            || object.pointer("/metadata/resourceVersion").is_some()
            || object.pointer("/status").is_some()
        {
            response(&mut stream, "422 Unprocessable Entity", json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":422,"reason":"Invalid","message":"new Job retains old execution identity"}).to_string(), "application/json").await;
            return;
        }
        let generated = format!(
            "{}{:05}",
            object
                .pointer("/metadata/generateName")
                .and_then(Value::as_str)
                .unwrap_or("job-"),
            objects.lock().unwrap().len()
        );
        let name = object
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or(&generated)
            .to_owned();
        object["metadata"]["name"] = json!(name);
        object["metadata"]["namespace"] = json!(namespace.unwrap_or("default"));
        object["metadata"]["uid"] = json!(format!("uid-{name}"));
        object["metadata"]["resourceVersion"] = json!("1");
        object["spec"]["selector"] =
            json!({"matchLabels":{"batch.kubernetes.io/controller-uid":format!("uid-{name}")}});
        object["spec"]["template"]["metadata"]["labels"]["batch.kubernetes.io/controller-uid"] =
            json!(format!("uid-{name}"));
        object["spec"]["template"]["metadata"]["labels"]["batch.kubernetes.io/job-name"] =
            json!(name);
        if !query.contains_key("dryRun") {
            objects.lock().unwrap().push(object.clone());
            let _ = events.send(json!({"type":"ADDED","object":object.clone()}));
        }
        response(
            &mut stream,
            "201 Created",
            object.to_string(),
            "application/json",
        )
        .await;
        return;
    }
    if method == "POST" && resource == "workflows" && name.is_none() {
        let mut object = request.clone();
        let owner_valid = object
            .pointer("/metadata/ownerReferences")
            .is_none_or(|owners| {
                owners.as_array().is_some_and(|owners| {
                    owners.len() == 1
                        && owners[0]["kind"] == "CronWorkflow"
                        && owners[0]["controller"] == true
                        && owners[0]["uid"].as_str().is_some_and(|uid| !uid.is_empty())
                        && objects.lock().unwrap().iter().any(|source| {
                            source["kind"] == "CronWorkflow"
                                && source["apiVersion"] == owners[0]["apiVersion"]
                                && source.pointer("/metadata/name") == owners[0].get("name")
                                && source.pointer("/metadata/uid") == owners[0].get("uid")
                                && source
                                    .pointer("/metadata/namespace")
                                    .and_then(Value::as_str)
                                    == namespace
                        })
                })
            });
        if object["apiVersion"] != "argoproj.io/v1alpha1"
            || object["kind"] != "Workflow"
            || object.pointer("/metadata/uid").is_some()
            || object.pointer("/metadata/resourceVersion").is_some()
            || !owner_valid
            || object.pointer("/status").is_some()
            || object
                .pointer("/metadata/namespace")
                .and_then(Value::as_str)
                != namespace
            || !object["spec"].is_object()
        {
            response(&mut stream, "422 Unprocessable Entity", json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":422,"reason":"Invalid","message":"new Workflow retains old execution identity or has an invalid namespace/spec"}).to_string(), "application/json").await;
            return;
        }
        let generated = {
            let store = objects.lock().unwrap();
            let prefix = object
                .pointer("/metadata/generateName")
                .and_then(Value::as_str)
                .unwrap_or("workflow-");
            let mut serial = store.len();
            loop {
                let candidate = format!("{prefix}{serial:05}");
                if !store.iter().any(|existing| {
                    matches(existing)
                        && existing.pointer("/metadata/name").and_then(Value::as_str)
                            == Some(&candidate)
                }) {
                    break candidate;
                }
                serial += 1;
            }
        };
        let name = object
            .pointer("/metadata/name")
            .and_then(Value::as_str)
            .unwrap_or(&generated)
            .to_owned();
        if objects.lock().unwrap().iter().any(|existing| {
            matches(existing)
                && existing.pointer("/metadata/name").and_then(Value::as_str) == Some(&name)
        }) {
            response(&mut stream, "409 Conflict", json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":409,"reason":"AlreadyExists","message":"Workflow already exists"}).to_string(), "application/json").await;
            return;
        }
        object["metadata"]["name"] = json!(name);
        object["metadata"]["uid"] = json!(format!("uid-{name}"));
        object["metadata"]["resourceVersion"] = json!("1");
        object["status"] = json!({"phase":"Pending"});
        if !query.contains_key("dryRun") {
            objects.lock().unwrap().push(object.clone());
            let _ = events.send(json!({"type":"ADDED","object":object.clone()}));
        }
        response(
            &mut stream,
            "201 Created",
            object.to_string(),
            "application/json",
        )
        .await;
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
    if method == "DELETE" && kind == "Pod" && parts.get(index + 2).is_none() {
        if request.pointer("/preconditions/uid").is_some()
            && request.pointer("/preconditions/uid") != object.pointer("/metadata/uid")
        {
            response(&mut stream, "409 Conflict", json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":409,"reason":"Conflict","message":"Pod UID changed after review"}).to_string(), "application/json").await;
            return;
        }
        if !query.contains_key("dryRun") {
            objects.lock().unwrap().retain(|existing| {
                !matches(existing)
                    || existing.pointer("/metadata/uid") != object.pointer("/metadata/uid")
            });
            let _ = events.send(json!({"type":"DELETED","object":object}));
        }
        response(
            &mut stream,
            "200 OK",
            json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200}).to_string(),
            "application/json",
        )
        .await;
        return;
    }
    if method == "PATCH" {
        if kind == "Workflow" && request.is_array() {
            let identity_tests =
                ["/metadata/uid", "/metadata/resourceVersion"]
                    .iter()
                    .all(|path| {
                        request.as_array().unwrap().iter().any(|operation| {
                            operation["op"] == "test" && operation["path"] == *path
                        })
                    });
            if !identity_tests {
                response(&mut stream, "409 Conflict", json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":409,"reason":"Conflict","message":"Workflow patch requires reviewed UID and resourceVersion tests"}).to_string(), "application/json").await;
                return;
            }
            if let Err(message) = workflow_patch(&mut object, &request) {
                response(&mut stream, "409 Conflict", json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":409,"reason":"Conflict","message":message}).to_string(), "application/json").await;
                return;
            }
        } else {
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
            } else if kind == "Node" {
                if let Some(unschedulable) = request.pointer("/spec/unschedulable") {
                    object["spec"]["unschedulable"] = unschedulable.clone();
                }
            } else if kind == "CronJob" {
                if let Some(suspend) = request.pointer("/spec/suspend") {
                    object["spec"]["suspend"] = suspend.clone();
                }
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
        }
        if !query.contains_key("dryRun") {
            object["metadata"]["resourceVersion"] = if kind == "Workflow" {
                json!(
                    (object
                        .pointer("/metadata/resourceVersion")
                        .and_then(Value::as_str)
                        .and_then(|version| version.parse::<u64>().ok())
                        .unwrap_or(0)
                        + 1)
                    .to_string()
                )
            } else {
                json!("2")
            };
            {
                let mut store = objects.lock().unwrap();
                let existing = store
                    .iter_mut()
                    .find(|existing| {
                        existing["kind"] == kind
                            && existing.pointer("/metadata/name").and_then(Value::as_str)
                                == Some(name)
                            && matches(existing)
                    })
                    .unwrap();
                *existing = object.clone();
            }
            let _ = events.send(json!({"type":"MODIFIED","object":object.clone()}));
        }
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
