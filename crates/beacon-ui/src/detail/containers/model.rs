use crate::overview::{Cell, FieldRows, Table};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Category {
    #[default]
    Ports,
    Health,
    Environment,
    Mounts,
}
impl Category {
    pub(super) const ALL: [Self; 4] = [Self::Ports, Self::Health, Self::Environment, Self::Mounts];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Ports => "Ports & resources",
            Self::Health => "Health checks",
            Self::Environment => "Environment variables",
            Self::Mounts => "Volume mounts",
        }
    }
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::Ports => "container-tab-ports",
            Self::Health => "container-tab-health",
            Self::Environment => "container-tab-environment",
            Self::Mounts => "container-tab-mounts",
        }
    }
}

pub(in crate::detail) struct Probe {
    pub title: &'static str,
    pub target: String,
    pub port: Option<String>,
    pub period: u64,
    pub timeout: u64,
    pub details: FieldRows,
}

pub(in crate::detail) struct Configuration {
    pub ports: Table,
    pub probes: Vec<Probe>,
    pub environment: Table,
    pub environment_sources: Table,
    pub mounts: Table,
    pub extra: FieldRows,
}

fn string(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}
fn array<'a>(value: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    value
        .get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}
fn meaningful(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Object(value) => value.values().any(meaningful),
        Value::Array(value) => value.iter().any(meaningful),
        _ => true,
    }
}
fn reference(kind: &str, reference: &Value) -> String {
    format!(
        "{kind}/{} · {}",
        string(&reference["name"]),
        string(&reference["key"])
    )
}

impl Configuration {
    pub(in crate::detail) fn of(container: &Value) -> Self {
        let has_host_ports = array(container, "ports").any(|port| port.get("hostPort").is_some());
        let ports = Table {
            headers: if has_host_ports {
                vec!["Name", "Container port", "Protocol", "Host port"]
            } else {
                vec!["Name", "Container port", "Protocol"]
            },
            rows: array(container, "ports")
                .map(|port| {
                    let mut cells = vec![
                        Cell::text(port["name"].as_str().unwrap_or("—")),
                        Cell::text(string(&port["containerPort"])),
                        Cell::text(port["protocol"].as_str().unwrap_or("TCP")),
                    ];
                    if has_host_ports {
                        let host = port["hostIP"].as_str().filter(|ip| !ip.is_empty());
                        cells.push(Cell::text(match (host, port.get("hostPort")) {
                            (Some(host), Some(number)) => format!("{host}:{}", string(number)),
                            (_, Some(number)) => string(number),
                            _ => "—".into(),
                        }));
                    }
                    cells
                })
                .collect(),
        };
        let environment = Table {
            headers: vec!["Name", "Value / source"],
            rows: array(container, "env")
                .map(|entry| {
                    let value = if let Some(value) = entry.get("value") {
                        string(value)
                    } else if let Some(source) = entry.get("valueFrom") {
                        if let Some(reference_value) = source.get("secretKeyRef") {
                            reference("Secret", reference_value)
                        } else if let Some(reference_value) = source.get("configMapKeyRef") {
                            reference("ConfigMap", reference_value)
                        } else if let Some(field) = source.pointer("/fieldRef/fieldPath") {
                            string(field)
                        } else if let Some(resource) = source.get("resourceFieldRef") {
                            let name = string(&resource["containerName"]);
                            let divisor = resource["divisor"].as_str().unwrap_or("1");
                            format!(
                                "{}{} / {divisor}",
                                if name.is_empty() {
                                    String::new()
                                } else {
                                    format!("{name} · ")
                                },
                                string(&resource["resource"])
                            )
                        } else {
                            string(source)
                        }
                    } else {
                        String::new()
                    };
                    vec![Cell::text(string(&entry["name"])), Cell::text(value)]
                })
                .collect(),
        };
        let environment_sources = Table {
            headers: vec!["Source", "Prefix"],
            rows: array(container, "envFrom")
                .map(|source| {
                    let name = if let Some(reference) = source.get("secretRef") {
                        format!("Secret/{}", string(&reference["name"]))
                    } else if let Some(reference) = source.get("configMapRef") {
                        format!("ConfigMap/{}", string(&reference["name"]))
                    } else {
                        string(source)
                    };
                    vec![
                        Cell::text(name),
                        Cell::text(
                            source["prefix"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .unwrap_or("—"),
                        ),
                    ]
                })
                .collect(),
        };
        let mounts = Table {
            headers: vec!["Volume", "Mount path", "Access"],
            rows: array(container, "volumeMounts")
                .map(|mount| {
                    let mut path = string(&mount["mountPath"]);
                    for key in [
                        "subPath",
                        "subPathExpr",
                        "mountPropagation",
                        "recursiveReadOnly",
                    ] {
                        if let Some(value) = mount.get(key).filter(|value| meaningful(value)) {
                            path.push_str(&format!("\n{key}: {}", string(value)));
                        }
                    }
                    vec![
                        Cell::text(string(&mount["name"])),
                        Cell::text(path),
                        Cell::text(if mount["readOnly"].as_bool() == Some(true) {
                            "Read only"
                        } else {
                            "Read / write"
                        }),
                    ]
                })
                .chain(array(container, "volumeDevices").map(|device| {
                    vec![
                        Cell::text(string(&device["name"])),
                        Cell::text(string(&device["devicePath"])),
                        Cell::text("Block device"),
                    ]
                }))
                .collect(),
        };
        let probes = [
            ("livenessProbe", "Liveness probe"),
            ("readinessProbe", "Readiness probe"),
            ("startupProbe", "Startup probe"),
        ]
        .into_iter()
        .filter_map(|(key, title)| {
            container
                .get(key)
                .filter(|value| meaningful(value))
                .map(|probe| Probe::of(title, probe, container))
        })
        .collect();
        let mut extra = container.clone();
        if let Value::Object(fields) = &mut extra {
            for key in [
                "name",
                "image",
                "ports",
                "livenessProbe",
                "readinessProbe",
                "startupProbe",
                "env",
                "envFrom",
                "volumeMounts",
                "volumeDevices",
            ] {
                fields.remove(key);
            }
            if let Some(Value::Object(resources)) = fields.get_mut("resources") {
                resources.remove("requests");
                resources.remove("limits");
            }
            fields.retain(|_, value| meaningful(value));
        }
        Self {
            ports,
            probes,
            environment,
            environment_sources,
            mounts,
            extra: if meaningful(&extra) {
                crate::overview::rows(&extra)
            } else {
                vec![]
            },
        }
    }
}

impl Probe {
    fn of(title: &'static str, probe: &Value, container: &Value) -> Self {
        let mut port = None;
        let mut details = vec![];
        let target = if let Some(http) = probe.get("httpGet") {
            port = http.get("port").map(|port| resolve_port(port, container));
            let path = http["path"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or("/");
            let scheme = http["scheme"].as_str().unwrap_or("HTTP");
            let host = http["host"].as_str().unwrap_or("");
            if http.get("httpHeaders").is_some_and(meaningful) {
                details.push((
                    "HTTP headers".into(),
                    Some(
                        array(http, "httpHeaders")
                            .map(|header| {
                                format!("{}: {}", string(&header["name"]), string(&header["value"]))
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                ));
            }
            format!("{scheme} GET {host}{path}")
        } else if let Some(tcp) = probe.get("tcpSocket") {
            port = tcp.get("port").map(|port| resolve_port(port, container));
            let host = tcp["host"].as_str().unwrap_or("");
            if host.is_empty() {
                "TCP".into()
            } else {
                format!("TCP · {host}")
            }
        } else if let Some(grpc) = probe.get("grpc") {
            port = grpc.get("port").map(string);
            let service = grpc["service"].as_str().unwrap_or("");
            if service.is_empty() {
                "gRPC".into()
            } else {
                format!("gRPC · {service}")
            }
        } else if let Some(exec) = probe.get("exec") {
            array(exec, "command")
                .map(string)
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            string(probe)
        };
        for (field, label, default) in [
            ("initialDelaySeconds", "Initial delay", 0),
            ("successThreshold", "Success threshold", 1),
            ("failureThreshold", "Failure threshold", 3),
        ] {
            details.push((
                label.into(),
                Some(probe[field].as_u64().unwrap_or(default).to_string()),
            ));
        }
        if let Some(seconds) = probe.get("terminationGracePeriodSeconds") {
            details.push((
                "Termination grace period (seconds)".into(),
                Some(string(seconds)),
            ));
        }
        Self {
            title,
            target,
            port,
            period: probe["periodSeconds"].as_u64().unwrap_or(10),
            timeout: probe["timeoutSeconds"].as_u64().unwrap_or(1),
            details,
        }
    }
}

fn resolve_port(port: &Value, container: &Value) -> String {
    if let Some(name) = port.as_str()
        && let Some(number) = array(container, "ports")
            .find(|candidate| candidate["name"].as_str() == Some(name))
            .and_then(|candidate| candidate.get("containerPort"))
    {
        return format!("{name} · {}", string(number));
    }
    string(port)
}

pub(super) fn image_parts(image: &str) -> (&str, Option<&str>) {
    if let Some((name, digest)) = image.split_once('@') {
        return (name, Some(digest));
    }
    let slash = image.rfind('/');
    if let Some(colon) = image.rfind(':')
        && slash.is_none_or(|slash| colon > slash)
    {
        return (&image[..colon], Some(&image[colon + 1..]));
    }
    (image, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn probe_handlers_resolve_named_ports_and_keep_per_probe_settings() {
        let config = Configuration::of(
            &json!({"ports":[{"name":"http","containerPort":4000}], "livenessProbe":{"httpGet":{"path":"/health","port":"http"},"failureThreshold":5},"readinessProbe":{"tcpSocket":{"port":8080},"timeoutSeconds":2},"startupProbe":{"grpc":{"port":9000,"service":"ready"}}}),
        );
        assert_eq!(config.probes.len(), 3);
        assert_eq!(config.probes[0].target, "HTTP GET /health");
        assert_eq!(config.probes[0].port.as_deref(), Some("http · 4000"));
        assert_eq!(config.probes[0].period, 10);
        assert_eq!(config.probes[0].timeout, 1);
        assert_eq!(config.probes[0].details[2].1.as_deref(), Some("5"));
        assert_eq!(config.probes[1].target, "TCP");
        assert_eq!(config.probes[1].timeout, 2);
        assert_eq!(config.probes[2].target, "gRPC · ready");
    }

    #[test]
    fn exec_probe_preserves_command_and_http_headers_stay_with_the_probe() {
        let config = Configuration::of(
            &json!({"livenessProbe":{"exec":{"command":["sh","-c","test -f /ready"]}},"readinessProbe":{"httpGet":{"port":80,"httpHeaders":[{"name":"X-Check","value":"ready"}]}}}),
        );
        assert_eq!(config.probes[0].target, "sh -c test -f /ready");
        assert_eq!(config.probes[1].target, "HTTP GET /");
        assert_eq!(
            config.probes[1].details[0],
            ("HTTP headers".into(), Some("X-Check: ready".into()))
        );
    }

    #[test]
    fn structured_sections_do_not_repeat_in_additional_fields_or_resolve_secrets() {
        let config = Configuration::of(
            &json!({"name":"main", "image":"registry:5000/app:tag", "ports":[{"name":"http","containerPort":4000,"hostPort":80}], "resources":{}, "env":[{"name":"TOKEN","valueFrom":{"secretKeyRef":{"name":"auth","key":"token"}}},{"name":"NODE","valueFrom":{"fieldRef":{"fieldPath":"spec.nodeName"}}}],"envFrom":[{"configMapRef":{"name":"app"},"prefix":"APP_"}],"volumeMounts":[{"name":"config","mountPath":"/config","readOnly":true,"subPath":"app.yaml"}],"command":["/app/server"],"securityContext":{}}),
        );
        assert_eq!(config.ports.rows.len(), 1);
        assert_eq!(config.ports.headers.len(), 4);
        assert_eq!(
            config.environment.rows[0][1].value.as_deref(),
            Some("Secret/auth · token")
        );
        assert_eq!(
            config.environment.rows[1][1].value.as_deref(),
            Some("spec.nodeName")
        );
        assert_eq!(
            config.environment_sources.rows[0][1].value.as_deref(),
            Some("APP_")
        );
        assert!(
            config.mounts.rows[0][1]
                .value
                .as_ref()
                .unwrap()
                .contains("subPath: app.yaml")
        );
        assert_eq!(config.mounts.rows[0][2].value.as_deref(), Some("Read only"));
        assert_eq!(config.extra.len(), 1);
        assert!(config.extra[0].0.starts_with("command"));
    }

    #[test]
    fn absent_sections_are_empty_and_image_registry_ports_are_not_tags() {
        let config = Configuration::of(&json!({"name":"empty","resources":{}}));
        assert!(
            config.ports.rows.is_empty()
                && config.probes.is_empty()
                && config.environment.rows.is_empty()
                && config.mounts.rows.is_empty()
                && config.extra.is_empty()
        );
        assert_eq!(
            image_parts("registry:5000/team/app"),
            ("registry:5000/team/app", None)
        );
        assert_eq!(
            image_parts("registry:5000/team/app:v1"),
            ("registry:5000/team/app", Some("v1"))
        );
        assert_eq!(image_parts("app@sha256:123"), ("app", Some("sha256:123")));
    }
}
