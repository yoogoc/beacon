use super::*;
use crate::sources::{self, Preferences, Source, SourceKind};
use serde_json::{Value, json};
use std::{fs, sync::Arc};

fn target(value: Value) -> ConnectionConfig {
    ConnectionConfig {
        context: "selected".into(),
        config: Arc::new(serde_json::from_value(value).unwrap()),
    }
}
fn config() -> Value {
    json!({
        "apiVersion": "v1", "kind": "Config", "current-context": "unrelated",
        "clusters": [
            {"name": "selected-cluster", "cluster": {
                "server": "https://selected.invalid", "tls-server-name": "kubernetes",
                "proxy-url": "http://localhost:8080", "disable-compression": true
            }},
            {"name": "unrelated-cluster", "cluster": {"server": "https://other.invalid", "certificate-authority": "/never-read-this"}}
        ],
        "contexts": [
            {"name": "selected", "context": {"cluster": "selected-cluster", "user": "selected-user", "namespace": "payments"}},
            {"name": "unrelated", "context": {"cluster": "unrelated-cluster", "user": "unrelated-user"}}
        ],
        "users": [
            {"name": "selected-user", "user": {"token": "selected-secret"}},
            {"name": "unrelated-user", "user": {"token": "never-export-this", "client-key": "/never-read-this"}}
        ],
        "extensions": [{"name": "unrelated", "extension": {"secret": "never-export-this"}}]
    })
}
fn exported(target: &ConnectionConfig) -> Value {
    serde_json::to_value(standalone(target).unwrap()).unwrap()
}

#[test]
fn cluster_export_contains_only_the_chosen_context_and_its_credentials() {
    let target = target(config());
    let before = serde_json::to_value(&*target.config).unwrap();
    let result = exported(&target);
    assert_eq!(result["current-context"], "selected");
    assert_eq!(result["contexts"].as_array().unwrap().len(), 1);
    assert_eq!(result["clusters"].as_array().unwrap().len(), 1);
    assert_eq!(result["users"].as_array().unwrap().len(), 1);
    assert_eq!(result["contexts"][0]["context"]["namespace"], "payments");
    assert_eq!(
        result["clusters"][0]["cluster"],
        before["clusters"][0]["cluster"]
    );
    assert_eq!(result["users"][0]["user"]["token"], "selected-secret");
    assert!(!result.to_string().contains("never-export-this"));
    assert_eq!(serde_json::to_value(&*target.config).unwrap(), before);
}

#[test]
fn cluster_export_embeds_relative_certificate_key_and_token_files_in_private_yaml() {
    let directory = tempfile::tempdir().unwrap();
    for (name, bytes) in [
        ("ca.pem", b"CA\n".as_slice()),
        ("cert.pem", b"CERT\n"),
        ("key.pem", b"KEY\n"),
        ("token", b"  bearer-token\n"),
    ] {
        fs::write(directory.path().join(name), bytes).unwrap();
    }
    let mut value = config();
    value["clusters"][0]["cluster"]["certificate-authority"] = "ca.pem".into();
    value["users"][0]["user"] =
        json!({"client-certificate": "cert.pem", "client-key": "key.pem", "tokenFile": "token"});
    let source = directory.path().join("source.yaml");
    let original = serde_saphyr::to_string(&value).unwrap();
    fs::write(&source, &original).unwrap();
    let target = ConnectionConfig {
        context: "selected".into(),
        config: Arc::new(Kubeconfig::read_from(&source).unwrap()),
    };
    let elsewhere = tempfile::tempdir().unwrap();
    let destination = elsewhere.path().join("export.yaml");
    save(&target, &destination).unwrap();
    assert_eq!(fs::read_to_string(&source).unwrap(), original);
    for name in ["ca.pem", "cert.pem", "key.pem", "token"] {
        fs::remove_file(directory.path().join(name)).unwrap();
    }
    let result = serde_json::to_value(Kubeconfig::read_from(&destination).unwrap()).unwrap();
    let cluster = &result["clusters"][0]["cluster"];
    assert_eq!(
        cluster["certificate-authority-data"],
        STANDARD.encode(b"CA\n")
    );
    assert!(cluster.get("certificate-authority").is_none());
    let user = &result["users"][0]["user"];
    assert_eq!(user["client-certificate-data"], STANDARD.encode(b"CERT\n"));
    assert_eq!(user["client-key-data"], STANDARD.encode(b"KEY\n"));
    assert_eq!(user["token"], "bearer-token");
    for field in ["client-certificate", "client-key", "tokenFile"] {
        assert!(user.get(field).is_none());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn cluster_export_prefers_embedded_data_and_never_runs_exec_plugins() {
    let mut value = config();
    value["clusters"][0]["cluster"]["certificate-authority"] = "/missing-ca".into();
    value["clusters"][0]["cluster"]["certificate-authority-data"] =
        STANDARD.encode(b"inline-ca").into();
    value["users"][0]["user"] = json!({
        "token": "inline-token", "tokenFile": "/missing-token",
        "client-certificate-data": STANDARD.encode(b"inline-cert"), "client-certificate": "/missing-cert",
        "client-key-data": STANDARD.encode(b"inline-key"), "client-key": "/missing-key",
        "exec": {"apiVersion": "client.authentication.k8s.io/v1", "command": "never-run-this-helper",
            "args": ["eks", "get-token", "--cluster-name", "demo"],
            "env": [{"name": "AWS_PROFILE", "value": "team"}], "interactiveMode": "IfAvailable", "provideClusterInfo": true}
    });
    let result = exported(&target(value.clone()));
    assert_eq!(
        result["users"][0]["user"]["exec"],
        value["users"][0]["user"]["exec"]
    );
    assert_eq!(result["users"][0]["user"]["token"], "inline-token");
    assert_eq!(
        result["users"][0]["user"]["client-key-data"],
        STANDARD.encode(b"inline-key")
    );
    assert!(result["users"][0]["user"].get("tokenFile").is_none());
}

#[test]
fn cluster_export_uses_the_exact_source_when_context_names_collide() {
    let directory = tempfile::tempdir().unwrap();
    let mut preferences = Preferences {
        system_enabled: false,
        sources: vec![],
    };
    for (id, server) in [
        ("first", "https://first.invalid"),
        ("second", "https://second.invalid"),
    ] {
        let mut value = config();
        value["clusters"][0]["cluster"]["server"] = server.into();
        let path = directory.path().join(format!("{id}.yaml"));
        fs::write(&path, serde_saphyr::to_string(&value).unwrap()).unwrap();
        preferences.sources.push(Source {
            id: id.into(),
            name: id.into(),
            kind: SourceKind::File { path },
            enabled: true,
            excluded: Default::default(),
        });
    }
    let catalog = sources::load(&preferences, directory.path(), &[]);
    let mut servers = vec![];
    for source in &catalog.statuses {
        if let Some(context) = source
            .contexts
            .iter()
            .find(|c| c.entry.id.context_name() == "selected")
        {
            let result = exported(&catalog.targets[&context.entry.id]);
            servers.push(
                result["clusters"][0]["cluster"]["server"]
                    .as_str()
                    .unwrap()
                    .to_owned(),
            );
            assert_eq!(result["current-context"], "selected");
        }
    }
    assert_eq!(servers, ["https://first.invalid", "https://second.invalid"]);
}

#[test]
fn cluster_export_failures_preserve_the_destination_and_leave_no_partial_files() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("existing.yaml");
    fs::write(&destination, b"keep existing file").unwrap();
    let mut value = config();
    value["users"][0]["user"] = json!({"client-key": directory.path().join("missing-key")});
    assert!(matches!(
        save(&target(value), &destination),
        Err(Failure::Reference {
            field: "client-key",
            ..
        })
    ));
    assert_eq!(fs::read(&destination).unwrap(), b"keep existing file");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    assert!(matches!(
        save(
            &target(config()),
            &directory.path().join("missing/export.yaml")
        ),
        Err(Failure::File(_))
    ));
}

#[test]
fn cluster_export_validates_references_and_allows_anonymous_contexts() {
    let mut value = config();
    value["contexts"] = json!([]);
    assert!(matches!(standalone(&target(value)), Err(Failure::Context)));
    let mut value = config();
    value["clusters"] = json!([]);
    assert!(matches!(standalone(&target(value)), Err(Failure::Cluster)));
    let mut value = config();
    value["users"] = json!([]);
    assert!(matches!(standalone(&target(value)), Err(Failure::User)));
    for user in [Value::Null, "".into()] {
        let mut value = config();
        value["contexts"][0]["context"]["user"] = user;
        assert_eq!(exported(&target(value))["users"], json!([]));
    }
}

#[test]
fn cluster_export_rejects_oversized_references_and_invalid_token_encoding() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("credential");
    let file = File::create(&path).unwrap();
    file.set_len(MAX_FILE_BYTES + 1).unwrap();
    let mut value = config();
    value["users"][0]["user"] = json!({"tokenFile": path});
    assert!(matches!(
        standalone(&target(value.clone())),
        Err(Failure::TooLarge("tokenFile"))
    ));
    fs::write(path, [0xff, 0xfe]).unwrap();
    assert!(matches!(
        standalone(&target(value)),
        Err(Failure::TokenEncoding)
    ));
}

#[test]
fn cluster_export_suggests_portable_filenames_for_arns_and_custom_names() {
    for name in [
        "CON",
        "..",
        "集群",
        "arn:aws:eks:us-east-1:123:cluster/dev",
        "a/b\\c:?*. ",
        &"long".repeat(80),
    ] {
        let filename = filename(name);
        assert!(filename.starts_with("kubeconfig-"));
        assert!(filename.ends_with(".yaml"));
        assert!(filename.len() < 150);
        assert!(!filename.contains(['/', '\\', ':', '?', '*', ' ']));
    }
}
