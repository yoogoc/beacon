//! Explain configured Ingress → Service → EndpointSlice → Pod paths.
use crate::{DynamicObject, ResourceStore};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Hop {
    pub group: &'static str,
    pub kind: &'static str,
    pub name: String,
    pub object: Option<Arc<DynamicObject>>,
}
impl Hop {
    fn object(group: &'static str, kind: &'static str, object: Arc<DynamicObject>) -> Self {
        Self {
            group,
            kind,
            name: object.metadata.name.clone().unwrap_or_default(),
            object: Some(object),
        }
    }
    fn missing(group: &'static str, kind: &'static str, name: String) -> Self {
        Self {
            group,
            kind,
            name,
            object: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Path {
    pub route: String,
    pub hops: [Option<Hop>; 4],
    pub notes: Vec<String>,
}

struct Backend {
    name: String,
    port: Value,
    route: String,
}
fn backends(ingress: &DynamicObject) -> Vec<Backend> {
    let mut backends = Vec::new();
    let mut add = |backend: &Value, route: String| {
        if let Some(name) = backend.pointer("/service/name").and_then(Value::as_str) {
            backends.push(Backend {
                name: name.into(),
                port: backend
                    .pointer("/service/port")
                    .cloned()
                    .unwrap_or(Value::Null),
                route,
            });
        }
    };
    if let Some(backend) = ingress.data.pointer("/spec/defaultBackend") {
        add(backend, "Default backend".into());
    }
    for rule in ingress
        .data
        .pointer("/spec/rules")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for path in rule
            .pointer("/http/paths")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            add(
                &path["backend"],
                format!(
                    "{}{}",
                    rule["host"].as_str().unwrap_or("*"),
                    path["path"].as_str().unwrap_or("/")
                ),
            );
        }
    }
    backends
}
fn selected(service: &DynamicObject, pod: &DynamicObject) -> bool {
    service.metadata.namespace == pod.metadata.namespace
        && service
            .data
            .pointer("/spec/selector")
            .and_then(Value::as_object)
            .is_some_and(|selector| {
                !selector.is_empty()
                    && selector.iter().all(|(key, value)| {
                        pod.metadata
                            .labels
                            .as_ref()
                            .and_then(|labels| labels.get(key))
                            .map(String::as_str)
                            == value.as_str()
                    })
            })
}
fn attached(service: &DynamicObject, slice: &DynamicObject) -> bool {
    service.metadata.namespace == slice.metadata.namespace
        && slice
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kubernetes.io/service-name"))
            == service.metadata.name.as_ref()
        && !slice
            .metadata
            .owner_references
            .as_deref()
            .unwrap_or_default()
            .iter()
            .any(|owner| {
                owner.kind == "Service"
                    && owner.api_version == "v1"
                    && Some(&owner.uid) != service.metadata.uid.as_ref()
            })
}
fn target_pod(
    endpoint: &Value,
    slice: &DynamicObject,
    pods: &ResourceStore,
) -> Option<Arc<DynamicObject>> {
    let reference = endpoint.get("targetRef")?;
    if reference["kind"] != "Pod" {
        return None;
    }
    let namespace = reference["namespace"]
        .as_str()
        .or(slice.metadata.namespace.as_deref());
    let name = reference["name"].as_str()?;
    pods.iter()
        .find(|(_, pod)| {
            pod.metadata.namespace.as_deref() == namespace
                && pod.metadata.name.as_deref() == Some(name)
                && reference["uid"]
                    .as_str()
                    .is_none_or(|uid| pod.metadata.uid.as_deref() == Some(uid))
        })
        .map(|(_, pod)| pod.clone())
}

/// Stores are namespace-scoped; callers must show incomplete loads separately.
pub fn paths(
    group: &str,
    kind: &str,
    object: Arc<DynamicObject>,
    stores: &[ResourceStore; 4],
) -> Vec<Path> {
    let [ingresses, services, slices, pods] = stores;
    let selected_services: Vec<_> = match (group, kind) {
        ("", "Service") => vec![object.clone()],
        ("", "Pod") => services
            .iter()
            .filter(|(_, service)| {
                selected(service, &object)
                    || slices.iter().any(|(_, slice)| {
                        attached(service, slice)
                            && slice.data["endpoints"].as_array().is_some_and(|endpoints| {
                                endpoints.iter().any(|endpoint| {
                                    target_pod(endpoint, slice, pods)
                                        .is_some_and(|pod| pod.metadata.uid == object.metadata.uid)
                                })
                            })
                    })
            })
            .map(|(_, object)| object.clone())
            .collect(),
        ("discovery.k8s.io", "EndpointSlice") => services
            .iter()
            .filter(|(_, service)| attached(service, &object))
            .map(|(_, object)| object.clone())
            .collect(),
        _ => services.iter().map(|(_, object)| object.clone()).collect(),
    };
    let mut roots = Vec::new();
    if group == "networking.k8s.io" && kind == "Ingress" {
        for backend in backends(&object) {
            let service = selected_services
                .iter()
                .find(|service| {
                    service.metadata.namespace == object.metadata.namespace
                        && service.metadata.name.as_deref() == Some(&backend.name)
                })
                .cloned();
            roots.push((Some(object.clone()), backend, service));
        }
        if roots.is_empty() {
            return vec![Path { route:"Ingress backend".into(), hops:[Some(Hop::object("networking.k8s.io","Ingress",object)),None,None,None], notes:vec!["No Service backend is configured. Resource backends require their own controller.".into()] }];
        }
    } else {
        for service in selected_services {
            let mut found = false;
            for (_, ingress) in ingresses.iter() {
                if ingress.metadata.namespace != service.metadata.namespace {
                    continue;
                }
                for backend in backends(ingress)
                    .into_iter()
                    .filter(|backend| service.metadata.name.as_deref() == Some(&backend.name))
                {
                    roots.push((Some(ingress.clone()), backend, Some(service.clone())));
                    found = true;
                }
            }
            if !found {
                roots.push((
                    None,
                    Backend {
                        name: service.metadata.name.clone().unwrap_or_default(),
                        port: Value::Null,
                        route: "Service backend".into(),
                    },
                    Some(service),
                ));
            }
        }
    }
    let mut paths = Vec::new();
    for (ingress, backend, service) in roots {
        let before = paths.len();
        let mut path = Path {
            route: backend.route,
            hops: [
                ingress.map(|ingress| Hop::object("networking.k8s.io", "Ingress", ingress)),
                None,
                None,
                (kind == "Pod").then(|| Hop::object("", "Pod", object.clone())),
            ],
            notes: vec![],
        };
        let Some(service) = service else {
            path.hops[1] = Some(Hop::missing("", "Service", backend.name));
            path.notes.push("Referenced Service does not exist.".into());
            paths.push(path);
            continue;
        };
        path.hops[1] = Some(Hop::object("", "Service", service.clone()));
        let ports = service
            .data
            .pointer("/spec/ports")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !backend.port.is_null()
            && !ports.iter().any(|port| {
                backend.port["name"]
                    .as_str()
                    .is_some_and(|name| port["name"] == name)
                    || backend.port["number"]
                        .as_i64()
                        .is_some_and(|number| port["port"] == number)
            })
        {
            path.notes
                .push("Ingress backend port does not match any Service port.".into());
        }
        if service.data.pointer("/spec/type").and_then(Value::as_str) == Some("ExternalName") {
            path.notes.push(format!(
                "ExternalName: {}",
                service
                    .data
                    .pointer("/spec/externalName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ));
            paths.push(path);
            continue;
        }
        let selected_pods: Vec<_> = pods
            .iter()
            .filter(|(_, pod)| selected(&service, pod))
            .map(|(_, pod)| pod)
            .collect();
        if service
            .data
            .pointer("/spec/selector")
            .and_then(Value::as_object)
            .is_some_and(|selector| !selector.is_empty())
            && selected_pods.is_empty()
        {
            path.notes.push("Service selector matches no Pods.".into());
        }
        let endpoints: Vec<_> = slices
            .iter()
            .filter(|(_, slice)| {
                attached(&service, slice)
                    && (kind != "EndpointSlice" || slice.metadata.uid == object.metadata.uid)
            })
            .map(|(_, slice)| slice.clone())
            .collect();
        if endpoints.is_empty() {
            path.notes
                .push("No EndpointSlices are published for this Service.".into());
            paths.push(path);
            continue;
        }
        for slice in endpoints {
            let mut branch = path.clone();
            branch.hops[2] = Some(Hop::object(
                "discovery.k8s.io",
                "EndpointSlice",
                slice.clone(),
            ));
            let endpoints = slice.data["endpoints"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if endpoints.is_empty() {
                branch.notes.push("EndpointSlice has no endpoints.".into());
                paths.push(branch);
                continue;
            }
            for endpoint in endpoints {
                let mut leaf = branch.clone();
                let target = target_pod(&endpoint, &slice, pods);
                if kind == "Pod"
                    && target
                        .as_ref()
                        .is_none_or(|pod| pod.metadata.uid != object.metadata.uid)
                {
                    continue;
                }
                if endpoint.pointer("/conditions/ready") == Some(&Value::Bool(false)) {
                    leaf.notes.push("Endpoint is not ready.".into());
                }
                let addresses = endpoint["addresses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ");
                leaf.notes.push(format!("Endpoint: {addresses}"));
                if let Some(pod) = target {
                    if service
                        .data
                        .pointer("/spec/selector")
                        .and_then(Value::as_object)
                        .is_some_and(|selector| !selector.is_empty())
                        && !selected(&service, &pod)
                    {
                        leaf.notes
                            .push("Endpoint Pod does not match the Service selector.".into());
                    }
                    for port in &ports {
                        let protocol = port["protocol"].as_str().unwrap_or("TCP");
                        let mut expected = port["targetPort"].as_i64().or_else(|| {
                            port["targetPort"]
                                .is_null()
                                .then(|| port["port"].as_i64())
                                .flatten()
                        });
                        if let Some(name) = port["targetPort"].as_str() {
                            let declared = pod
                                .data
                                .pointer("/spec/containers")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .flat_map(|container| {
                                    container["ports"].as_array().into_iter().flatten()
                                })
                                .find(|port| {
                                    port["name"] == name
                                        && port["protocol"].as_str().unwrap_or("TCP") == protocol
                                });
                            expected = declared.and_then(|port| port["containerPort"].as_i64());
                            if declared.is_none() {
                                leaf.notes.push(format!("Named targetPort {name} ({protocol}) is not declared by this Pod."));
                            }
                        }
                        if let Some(expected) = expected {
                            let published =
                                slice.data["ports"].as_array().into_iter().flatten().find(
                                    |published| {
                                        published["name"].as_str() == port["name"].as_str()
                                            && published["protocol"].as_str().unwrap_or("TCP")
                                                == protocol
                                    },
                                );
                            match published {
                                None => leaf.notes.push(format!("Service port {} ({protocol}) is absent from this EndpointSlice.", port["port"])),
                                Some(published) if published["port"].as_i64().is_some_and(|actual| actual != expected) => leaf.notes.push(format!("EndpointSlice port {} does not match targetPort {expected} ({protocol}).", published["port"])),
                                _ => {}
                            }
                        }
                    }
                    leaf.hops[3] = Some(Hop::object("", "Pod", pod));
                } else if endpoint.pointer("/targetRef/kind").and_then(Value::as_str) == Some("Pod")
                {
                    leaf.hops[3] = Some(Hop::missing(
                        "",
                        "Pod",
                        endpoint
                            .pointer("/targetRef/name")
                            .and_then(Value::as_str)
                            .unwrap_or("Unknown")
                            .into(),
                    ));
                    leaf.notes
                        .push("Referenced Pod is missing or its UID has changed.".into());
                } else {
                    leaf.notes
                        .push("External endpoint; no Pod reference.".into());
                }
                paths.push(leaf);
            }
        }
        if kind == "Pod" && paths.len() == before {
            path.notes.push(
                "This Pod matches the Service selector but is not published in its endpoints."
                    .into(),
            );
            paths.push(path);
        }
    }
    if paths.is_empty() {
        let mut hops = [None, None, None, None];
        let (stage, group, name, note) = if kind == "EndpointSlice" {
            (
                2,
                "discovery.k8s.io",
                "EndpointSlice",
                "No Service is associated with this EndpointSlice.",
            )
        } else {
            (
                3,
                "",
                "Pod",
                "This Pod is not published in any Service endpoints.",
            )
        };
        hops[stage] = Some(Hop::object(group, name, object));
        paths.push(Path {
            route: "Selected resource".into(),
            hops,
            notes: vec![note.into()],
        });
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Delta;
    use serde_json::json;
    #[test]
    fn selected_pods_retain_service_links_and_validate_published_target_ports() {
        let pod: Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"metadata":{"name":"web","namespace":"default","uid":"pod","labels":{"app":"web"}},"spec":{"containers":[{"name":"app","ports":[{"name":"http","containerPort":8080}]}]}})).unwrap());
        let service: Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"metadata":{"name":"web","namespace":"default","uid":"svc"},"spec":{"selector":{"app":"web"},"ports":[{"name":"web","port":80,"targetPort":"http"}]}})).unwrap());
        let mut slice: DynamicObject = serde_json::from_value(json!({"metadata":{"name":"slice","namespace":"default","uid":"slice","labels":{"kubernetes.io/service-name":"web"}},"ports":[{"name":"web","port":8081}],"endpoints":[{"addresses":["10.0.0.1"],"targetRef":{"kind":"Pod","name":"other","uid":"other"}}]})).unwrap();
        let mut stores = std::array::from_fn(|_| ResourceStore::new());
        stores[1].apply(Delta::Reset(vec![service]));
        stores[2].apply(Delta::Reset(vec![Arc::new(slice.clone())]));
        stores[3].apply(Delta::Reset(vec![pod.clone()]));
        let unpublished = paths("", "Pod", pod.clone(), &stores);
        assert_eq!(unpublished[0].hops[1].as_ref().unwrap().name, "web");
        assert_eq!(unpublished[0].hops[3].as_ref().unwrap().name, "web");
        assert!(
            unpublished[0]
                .notes
                .iter()
                .any(|note| note.contains("not published"))
        );
        slice.data["endpoints"][0]["targetRef"] = json!({"kind":"Pod","name":"web","uid":"pod"});
        stores[2].apply(Delta::Reset(vec![Arc::new(slice)]));
        let published = paths("", "Pod", pod, &stores);
        assert!(
            published[0]
                .notes
                .iter()
                .any(|note| note.contains("does not match targetPort 8080"))
        );
    }
    #[test]
    fn paths_detect_broken_ports_unready_endpoints_and_replaced_pods() {
        let ingress:Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"metadata":{"name":"web","namespace":"default","uid":"ing"},"spec":{"rules":[{"host":"web.test","http":{"paths":[{"path":"/","backend":{"service":{"name":"web","port":{"number":81}}}}]}}]}})).unwrap());
        let service:Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"metadata":{"name":"web","namespace":"default","uid":"svc"},"spec":{"selector":{"app":"web"},"ports":[{"port":80,"targetPort":"http"}]}})).unwrap());
        let slice:Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"metadata":{"name":"web-slice","namespace":"default","uid":"slice","labels":{"kubernetes.io/service-name":"web"}},"endpoints":[{"addresses":["10.0.0.1"],"conditions":{"ready":false},"targetRef":{"kind":"Pod","name":"web-pod","namespace":"default","uid":"old-pod"}}]})).unwrap());
        let pod:Arc<DynamicObject> = Arc::new(serde_json::from_value(json!({"metadata":{"name":"web-pod","namespace":"default","uid":"new-pod","labels":{"app":"web"}},"spec":{"containers":[{"name":"app","ports":[{"name":"http","containerPort":8080}]}]}})).unwrap());
        let mut stores = std::array::from_fn(|_| ResourceStore::new());
        for (store, objects) in
            stores
                .iter_mut()
                .zip([vec![ingress.clone()], vec![service], vec![slice], vec![pod]])
        {
            store.apply(Delta::Reset(objects));
        }
        let paths = paths("networking.k8s.io", "Ingress", ingress, &stores);
        assert_eq!(paths.len(), 1);
        let notes = paths[0].notes.join("\n");
        assert!(notes.contains("port does not match"));
        assert!(notes.contains("not ready"));
        assert!(notes.contains("UID has changed"));
        assert!(paths[0].hops[3].as_ref().unwrap().object.is_none());
    }
}
