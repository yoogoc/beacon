use super::*;
use crate::{ApiResource, ClusterId, GroupVersionKind, test_support::Fixture};
use serde_json::{Value, json};
use std::io::Read;

fn kind() -> Kind {
    Kind {
        resource: ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("", "v1", "ConfigMap"),
            "configmaps",
        ),
        namespaced: true,
        verbs: vec!["get".into()],
    }
}
fn object(ns: &str, name: &str) -> Value {
    json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":ns,"uid":format!("{ns}/{name}"),"resourceVersion":"7","labels":{"app":"test"},"managedFields":[{"manager":"test"}]},"data":{"settings.yaml":"server:\n  port: 8080\n"},"status":{"observed":true}})
}
fn target(object: &Value) -> DeleteTarget {
    DeleteTarget {
        reference: ObjectRef::new(
            object["metadata"]["namespace"].as_str().map(str::to_string),
            object["metadata"]["name"].as_str().unwrap(),
        ),
        uid: object["metadata"]["uid"].as_str().unwrap().into(),
    }
}
fn session(fixture: &Fixture) -> Arc<ClusterSession> {
    Arc::new(ClusterSession::for_testing(
        ClusterId::new("export-test"),
        fixture.url.clone(),
        vec![kind()],
    ))
}
#[tokio::test]
async fn single_yaml_reads_the_full_object_and_preserves_content() {
    let object = object("default", "settings");
    let fixture = Fixture::start(vec![object.clone()]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.yaml");
    save(
        session(&fixture),
        kind(),
        vec![target(&object)],
        path.clone(),
    )
    .await
    .unwrap();
    let value: Value = serde_saphyr::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut expected = object;
    expected["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("managedFields");
    assert_eq!(value, expected);
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(url, _)| url.starts_with("GET "))
    );
}
#[tokio::test]
async fn zip_has_one_compressed_yaml_per_resource_and_disambiguates_names() {
    // The last pair produces the same flat namespace/name combination.
    let objects = vec![
        object("one", "same"),
        object("two", "same"),
        object("a-b", "c"),
        object("a", "b-c"),
    ];
    let fixture = Fixture::start(objects.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resources.zip");
    save(
        session(&fixture),
        kind(),
        objects.iter().map(target).collect(),
        path.clone(),
    )
    .await
    .unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    assert_eq!(archive.len(), objects.len());
    let mut names = HashSet::new();
    for (index, original) in objects.iter().enumerate() {
        let mut file = archive.by_index(index).unwrap();
        assert_eq!(file.compression(), CompressionMethod::Deflated);
        assert!(file.name().ends_with(".yaml") && !file.name().contains('/'));
        assert!(names.insert(file.name().to_owned()));
        let mut text = String::new();
        file.read_to_string(&mut text).unwrap();
        let exported: Value = serde_saphyr::from_str(&text).unwrap();
        assert_eq!(
            exported["metadata"]["namespace"],
            original["metadata"]["namespace"]
        );
        assert_eq!(exported["metadata"]["name"], original["metadata"]["name"]);
        assert_eq!(exported["data"], original["data"]);
    }
}
#[tokio::test]
async fn failed_reads_leave_existing_files_intact_without_partial_archives() {
    let first = object("default", "a");
    let second = object("default", "b");
    let fixture = Fixture::start(vec![first.clone(), second.clone()]).await;
    fixture.deny("get/configmaps/b");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resources.zip");
    std::fs::write(&path, b"previous export").unwrap();
    let result = save(
        session(&fixture),
        kind(),
        vec![target(&first), target(&second)],
        path.clone(),
    )
    .await;
    assert!(
        matches!(result,Err(Failure::Resource { target: ObjectRef { name, .. }, .. }) if name == "b")
    );
    assert_eq!(std::fs::read(path).unwrap(), b"previous export");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
#[tokio::test]
async fn replaced_resources_and_empty_selection_are_not_exported() {
    let object = object("default", "a");
    let mut selected = target(&object);
    selected.uid = "replaced-uid".into();
    let fixture = Fixture::start(vec![object]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resource.yaml");
    assert!(
        save(session(&fixture), kind(), vec![selected], path.clone())
            .await
            .is_err()
    );
    assert!(!path.exists());
    assert!(matches!(
        save(session(&fixture), kind(), vec![], path.clone()).await,
        Err(Failure::Empty)
    ));
    assert!(!path.exists());
}
#[tokio::test]
async fn secret_exports_keep_the_original_values() {
    let secret = json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"credentials","namespace":"default","uid":"secret"},"type":"Opaque","data":{"password":"c2VjcmV0"}});
    let kind = Kind {
        resource: ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk("", "v1", "Secret"),
            "secrets",
        ),
        ..kind()
    };
    let fixture = Fixture::start(vec![secret.clone()]).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret.yaml");
    save(session(&fixture), kind, vec![target(&secret)], path.clone())
        .await
        .unwrap();
    let value: Value = serde_saphyr::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert_eq!(value["data"]["password"], "c2VjcmV0");
}
#[test]
fn filenames_are_portable_and_identify_namespace_and_kind() {
    let reference = ObjectRef::new(Some("default".into()), "settings");
    assert_eq!(
        filename(&kind(), &reference),
        "configmap-default-settings.yaml"
    );
    let dangerous = ObjectRef::new(
        Some("../bad\\ns".into()),
        format!("../../{}", "x".repeat(300)),
    );
    let filename = filename(&kind(), &dangerous);
    assert!(!filename.contains('/') && !filename.contains('\\') && filename.len() <= 185);
}
