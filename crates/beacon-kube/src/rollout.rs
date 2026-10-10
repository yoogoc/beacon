//! Deployment revisions and an optimistic, template-only rollback.
use std::sync::Arc;

use crate::{ClusterSession, DynamicObject, Error, ObjectRef, Result, WatchKey};
use kube::{
    Api,
    api::{ApiResource, Patch, PatchParams},
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Revision {
    pub number: u64,
    pub replica_set: Arc<DynamicObject>,
    pub template: Value,
    pub current: bool,
    pub images: Vec<String>,
}

pub fn template(object: &DynamicObject) -> Value {
    let mut template = object
        .data
        .pointer("/spec/template")
        .cloned()
        .unwrap_or(Value::Null);
    if let Some(labels) = template
        .pointer_mut("/metadata/labels")
        .and_then(Value::as_object_mut)
    {
        labels.remove("pod-template-hash");
    }
    template
}

pub fn revisions(deployment: &DynamicObject, sets: &[Arc<DynamicObject>]) -> Vec<Revision> {
    let Some(uid) = deployment
        .metadata
        .uid
        .as_deref()
        .filter(|uid| !uid.is_empty())
    else {
        return vec![];
    };
    let current = template(deployment);
    let mut revisions: Vec<_> = sets
        .iter()
        .filter_map(|set| {
            if set.metadata.namespace != deployment.metadata.namespace
                || !set
                    .metadata
                    .owner_references
                    .as_deref()
                    .unwrap_or_default()
                    .iter()
                    .any(|owner| {
                        owner.uid == uid
                            && owner.api_version == "apps/v1"
                            && owner.kind == "Deployment"
                            && owner.controller == Some(true)
                    })
            {
                return None;
            }
            let number = set
                .metadata
                .annotations
                .as_ref()?
                .get("deployment.kubernetes.io/revision")?
                .parse()
                .ok()?;
            let template = template(set);
            let images = template
                .pointer("/spec/containers")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|container| container["image"].as_str().map(str::to_owned))
                .collect();
            Some(Revision {
                number,
                current: template == current,
                template,
                images,
                replica_set: set.clone(),
            })
        })
        .collect();
    revisions.sort_by(|a, b| {
        b.number.cmp(&a.number).then_with(|| {
            a.replica_set
                .metadata
                .name
                .cmp(&b.replica_set.metadata.name)
        })
    });
    revisions
}

pub async fn history(
    session: Arc<ClusterSession>,
    deployment: &DynamicObject,
) -> Result<Vec<Revision>> {
    let resource = session
        .discovery()
        .kinds()
        .iter()
        .find(|kind| kind.resource.group == "apps" && kind.resource.kind == "ReplicaSet")
        .ok_or_else(|| Error::Manifest("ReplicaSets are not available in this cluster.".into()))?
        .resource
        .clone();
    let sets = session
        .list_objects(WatchKey::all(resource).in_namespace(deployment.metadata.namespace.clone()))
        .await?;
    Ok(revisions(deployment, &sets))
}

#[derive(Clone, Debug)]
pub struct Rollback {
    pub target: ObjectRef,
    pub uid: String,
    pub resource_version: String,
    pub template: Value,
    pub revision: u64,
}

impl Rollback {
    pub fn prepare(deployment: &DynamicObject, revision: &Revision) -> Result<Self> {
        if revisions(deployment, std::slice::from_ref(&revision.replica_set)).is_empty()
            || !revision.template.is_object()
        {
            return Err(Error::Manifest(
                "This revision no longer belongs to the selected Deployment.".into(),
            ));
        }
        Ok(Self {
            target: ObjectRef::of(deployment),
            uid: deployment
                .metadata
                .uid
                .clone()
                .ok_or_else(|| Error::Manifest("Deployment UID is missing.".into()))?,
            resource_version: deployment
                .metadata
                .resource_version
                .clone()
                .ok_or_else(|| Error::Manifest("Deployment resource version is missing.".into()))?,
            template: revision.template.clone(),
            revision: revision.number,
        })
    }

    pub fn proposed(&self, deployment: &DynamicObject) -> Result<Value> {
        let mut proposed =
            serde_json::to_value(deployment).map_err(|error| Error::Manifest(error.to_string()))?;
        proposed["spec"]["template"] = self.template.clone();
        Ok(proposed)
    }

    fn patch(&self) -> Value {
        // Replace the whole template, including fields absent from the old revision.
        // A plain merge would silently retain newly added containers or env variables.
        let mut template = self.template.clone();
        template["$patch"] = json!("replace");
        json!({"metadata":{"uid": self.uid, "resourceVersion": self.resource_version}, "spec":{"template":template}})
    }

    pub async fn execute(&self, session: Arc<ClusterSession>) -> Result<DynamicObject> {
        let namespace = self
            .target
            .namespace
            .as_deref()
            .ok_or_else(|| Error::Manifest("Deployment namespace is missing.".into()))?;
        let resource =
            ApiResource::from_gvk(&crate::GroupVersionKind::gvk("apps", "v1", "Deployment"));
        let api: Api<DynamicObject> =
            Api::namespaced_with(session.client().clone(), namespace, &resource);
        Ok(api
            .patch(
                &self.target.name,
                &PatchParams::default(),
                &Patch::Strategic(self.patch()),
            )
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn object(kind: &str, uid: &str, image: &str) -> DynamicObject {
        serde_json::from_value(json!({"apiVersion":"apps/v1","kind":kind,"metadata":{"name":uid,"namespace":"default","uid":uid,"resourceVersion":"42"},"spec":{"replicas":3,"template":{"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"app","image":image}]}}}})).unwrap()
    }
    #[tokio::test]
    async fn rollback_sends_a_template_replacement_and_rejects_a_stale_preview() {
        let deployment = object("Deployment", "deployment", "new");
        let fixture =
            crate::test_support::Fixture::start(vec![serde_json::to_value(&deployment).unwrap()])
                .await;
        let session = Arc::new(ClusterSession::for_testing(
            crate::ClusterId::new("rollback-fixture"),
            fixture.url.clone(),
            vec![],
        ));
        let plan = Rollback {
            target: ObjectRef::of(&deployment),
            uid: "deployment".into(),
            resource_version: "42".into(),
            template: json!({"metadata":{"labels":{"app":"web"}},"spec":{"containers":[{"name":"app","image":"old"}]}}),
            revision: 1,
        };
        plan.execute(session.clone()).await.unwrap();
        let actual = fixture.object("Deployment", "deployment");
        assert_eq!(
            actual["spec"]["template"]["spec"]["containers"][0]["image"],
            "old"
        );
        assert_eq!(actual["spec"]["replicas"], 3);
        let error = plan.execute(session).await.unwrap_err().user_message();
        assert!(error.contains("409"));
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests[0]
                .0
                .starts_with("PATCH /apis/apps/v1/namespaces/default/deployments/deployment")
        );
        assert_eq!(requests[0].1["spec"]["template"]["$patch"], "replace");
    }
    #[test]
    fn history_uses_controller_uid_namespace_and_numeric_revision() {
        let deployment = object("Deployment", "deployment", "new");
        let mut old = object("ReplicaSet", "old", "old");
        old.metadata.annotations =
            Some([("deployment.kubernetes.io/revision".into(), "9".into())].into());
        old.metadata.owner_references = Some(serde_json::from_value(json!([{"apiVersion":"apps/v1","kind":"Deployment","name":"deployment","uid":"deployment","controller":true}])).unwrap());
        old.data["spec"]["template"]["metadata"]["labels"]["pod-template-hash"] = json!("hash");
        let mut current = old.clone();
        current.data["spec"]["template"]["spec"]["containers"][0]["image"] = json!("new");
        current
            .metadata
            .annotations
            .as_mut()
            .unwrap()
            .insert("deployment.kubernetes.io/revision".into(), "10".into());
        let mut foreign = old.clone();
        foreign.metadata.owner_references.as_mut().unwrap()[0].uid = "recreated".into();
        let mut other_namespace = old.clone();
        other_namespace.metadata.namespace = Some("other".into());
        let revisions = revisions(
            &deployment,
            &[
                Arc::new(old),
                Arc::new(current),
                Arc::new(foreign),
                Arc::new(other_namespace),
            ],
        );
        assert_eq!(
            revisions.iter().map(|r| r.number).collect::<Vec<_>>(),
            [10, 9]
        );
        assert!(revisions[0].current);
        assert!(!revisions[1].current);
        let plan = Rollback::prepare(&deployment, &revisions[1]).unwrap();
        let patch = plan.patch();
        assert_eq!(patch["metadata"]["uid"], "deployment");
        assert_eq!(patch["metadata"]["resourceVersion"], "42");
        assert_eq!(patch["spec"]["template"]["$patch"], "replace");
        assert!(patch["spec"].get("replicas").is_none());
        assert!(
            patch
                .pointer("/spec/template/metadata/labels/pod-template-hash")
                .is_none()
        );
        assert_eq!(plan.proposed(&deployment).unwrap()["spec"]["replicas"], 3);
    }
}
