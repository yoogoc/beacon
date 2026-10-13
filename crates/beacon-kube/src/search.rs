//! Metadata-only object catalog for cross-kind search. Never retains Secret contents.
use crate::{ClusterSession, Kind, ObjectRef, Result};
use kube::{
    Api,
    api::{DynamicObject, ListParams},
};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct Object {
    pub reference: ObjectRef,
    pub labels: BTreeMap<String, String>,
}

/// All namespaces, paginated to avoid server defaults silently truncating the catalog.
pub async fn objects(session: &ClusterSession, kind: &Kind) -> Result<Vec<Object>> {
    let api: Api<DynamicObject> = Api::all_with(session.client().clone(), &kind.resource);
    let mut params = ListParams::default().limit(500);
    let mut objects = Vec::new();
    loop {
        let page = api.list_metadata(&params).await?;
        for object in page.items {
            if let Some(name) = object.metadata.name {
                objects.push(Object {
                    reference: ObjectRef::new(object.metadata.namespace, name),
                    labels: object.metadata.labels.unwrap_or_default(),
                });
            }
        }
        match page.metadata.continue_.filter(|token| !token.is_empty()) {
            Some(token) => params = params.continue_token(&token),
            None => break,
        }
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ApiResource, ClusterId, GroupVersionKind, test_support::Fixture};
    use serde_json::json;
    #[tokio::test]
    async fn indexes_only_metadata_across_namespaces_and_surfaces_denied_resources() {
        let fixture = Fixture::start(vec![json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"a","namespace":"alpha","labels":{"app":"api"}},"data":{"password":"must-not-be-indexed"}}),json!({"apiVersion":"v1","kind":"Secret","metadata":{"name":"b","namespace":"beta"}})]).await;
        let kind = Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk("", "v1", "Secret"),
                "secrets",
            ),
            namespaced: true,
            verbs: vec!["list".into()],
        };
        let session = ClusterSession::for_testing(
            ClusterId::new("test"),
            fixture.url.clone(),
            vec![kind.clone()],
        );
        let objects = objects(&session, &kind).await.unwrap();
        assert_eq!(objects.len(), 2);
        assert_eq!(
            objects[0].labels.get("app").map(String::as_str),
            Some("api")
        );
        assert_ne!(
            objects[0].reference.namespace,
            objects[1].reference.namespace
        );
        assert!(!format!("{objects:?}").contains("must-not-be-indexed"));
        fixture.deny("secrets");
        assert!(
            super::objects(&session, &kind)
                .await
                .unwrap_err()
                .user_message()
                .contains("403")
        );
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|(request, _)| request.starts_with("GET"))
        );
    }
}
