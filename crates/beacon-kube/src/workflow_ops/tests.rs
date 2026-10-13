use super::*;
use crate::{ApiResource, ClusterId, GroupVersionKind, test_support::Fixture};

fn kind(name: &str, plural: &str) -> Kind {
    Kind {
        resource: ApiResource::from_gvk_with_plural(
            &GroupVersionKind::gvk(argo::GROUP, "v1alpha1", name),
            plural,
        ),
        namespaced: name != "ClusterWorkflowTemplate",
        verbs: ["get", "list", "watch", "create", "patch", "delete"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
    }
}

fn connect(fixture: &Fixture) -> Arc<ClusterSession> {
    Arc::new(ClusterSession::for_testing(
        ClusterId::new("argo-operations-fixture"),
        fixture.url.clone(),
        vec![
            kind("Workflow", "workflows"),
            kind("WorkflowTemplate", "workflowtemplates"),
            kind("ClusterWorkflowTemplate", "clusterworkflowtemplates"),
            kind("CronWorkflow", "cronworkflows"),
            Kind {
                resource: resources::pod(),
                namespaced: true,
                verbs: vec!["get".into(), "list".into(), "delete".into()],
            },
        ],
    ))
}

fn dynamic(value: Value) -> Arc<DynamicObject> {
    Arc::new(serde_json::from_value(value).unwrap())
}

fn template() -> Value {
    json!({
        "apiVersion":"argoproj.io/v1alpha1", "kind":"WorkflowTemplate",
        "metadata":{
            "name":"run-template", "namespace":"team-a", "uid":"template-uid", "resourceVersion":"8",
            "labels":{"team":"platform","workflows.argoproj.io/phase":"Succeeded","workflows.argoproj.io/completed":"true"},
            "annotations":{"note":"keep","kubectl.kubernetes.io/last-applied-configuration":"old"},
            "managedFields":[{}], "creationTimestamp":"2026-10-01T00:00:00Z"
        },
        "spec":{
            "entrypoint":"main",
            "arguments":{
                "parameters":[
                    {"name":"message","value":"hello","description":"Message to print"},
                    {"name":"mode","default":"fast","enum":["fast","safe"]},
                    {"name":"required"},
                    {"name":"dynamic","valueFrom":{"configMapKeyRef":{"name":"settings","key":"runtime"}}}
                ],
                "artifacts":[{"name":"input","s3":{"bucket":"test","key":"input"}}]
            },
            "workflowMetadata":{"labels":{"created-from":"template"},"annotations":{"template-note":"keep"}},
            "templates":[{"name":"main","container":{"image":"busybox","args":["{{workflow.parameters.message}}"]}},{"name":"alternative","container":{"image":"alpine"}}]
        }
    })
}

fn supplied() -> Inputs {
    Inputs {
        values: BTreeMap::from([("required".into(), "".into())]),
        ..Default::default()
    }
}

fn workflow(phase: &str) -> Value {
    json!({
        "apiVersion":"argoproj.io/v1alpha1", "kind":"Workflow",
        "metadata":{
            "name":"run", "namespace":"team-a", "uid":"workflow-uid", "resourceVersion":"8", "generateName":"run-",
            "labels":{"team":"platform"}, "annotations":{"workflows.argoproj.io/pod-name-format":"v1"}
        },
        "spec":{"entrypoint":"main","templates":[{"name":"main","container":{"image":"busybox"}}],"arguments":{"parameters":[{"name":"message","value":"hello"}]}},
        "status":{"phase":phase,"message":"execution status","startedAt":"2026-10-01T00:00:00Z","outputs":{"parameters":[{"name":"keep","value":"retained"}]}}
    })
}

fn failed_workflow() -> Value {
    let mut value = workflow("Failed");
    value["metadata"]["labels"]["workflows.argoproj.io/completed"] = json!("true");
    value["metadata"]["labels"]["workflows.argoproj.io/workflow-archiving-status"] =
        json!("Archived");
    value["spec"]["shutdown"] = json!("Stop");
    value["spec"]["activeDeadlineSeconds"] = json!(0);
    value["status"]["finishedAt"] = json!("2026-10-01T00:02:00Z");
    value["status"]["conditions"] = json!([{"type":"Completed","status":"True"}]);
    value["status"]["nodes"] = json!({
        "run":{"id":"run","name":"run","type":"DAG","phase":"Failed","children":["run-ok","run-fail"],"outboundNodes":["run-fail"]},
        "run-ok":{"id":"run-ok","name":"run.ok","displayName":"ok","type":"Pod","phase":"Succeeded","boundaryID":"run","outputs":{"parameters":[{"name":"result","value":"keep-success"}]}},
        "run-fail":{"id":"run-fail","name":"run.fail","displayName":"fail","type":"Pod","phase":"Failed","boundaryID":"run","message":"nonzero exit"},
        "run-on-exit":{"id":"run-on-exit","name":"run.onExit","type":"Steps","phase":"Succeeded","children":["run-exit"]},
        "run-exit":{"id":"run-exit","name":"run.onExit[0].cleanup","type":"Pod","phase":"Succeeded","boundaryID":"run-on-exit"}
    });
    value
}

fn pod(name: &str) -> Value {
    json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"team-a","uid":format!("uid-{name}"),"resourceVersion":"1","labels":{"workflows.argoproj.io/workflow":"run"},"ownerReferences":[{"apiVersion":"argoproj.io/v1alpha1","kind":"Workflow","name":"run","uid":"workflow-uid","controller":true}]},"spec":{"containers":[{"name":"main","image":"busybox"}]},"status":{"phase":"Succeeded"}})
}

fn writes(fixture: &Fixture) -> Vec<(String, Value)> {
    fixture
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(line, _)| {
            !line.contains("dryRun=")
                && (line.starts_with("POST /apis/argoproj.io/v1alpha1/")
                    || line.starts_with("PATCH /apis/argoproj.io/v1alpha1/")
                    || line.starts_with("DELETE /api/v1/"))
        })
        .cloned()
        .collect()
}

#[test]
fn template_submission_validates_parameters_and_keeps_runtime_references() {
    let source = dynamic(template());
    let definitions = parameters("WorkflowTemplate", &source, Action::Submit).unwrap();
    assert_eq!(definitions.len(), 4);
    assert_eq!(definitions[1].value.as_deref(), Some("fast"));
    assert_eq!(definitions[1].choices, ["fast", "safe"]);
    assert!(definitions[2].required);
    assert!(definitions[3].runtime);
    assert!(!definitions[3].required);
    assert!(
        new_workflow(
            "WorkflowTemplate",
            &source,
            &Inputs::default(),
            Action::Submit
        )
        .unwrap_err()
        .user_message()
        .contains("provide a value for required")
    );

    let mut inputs = supplied();
    inputs.entrypoint = "alternative".into();
    inputs.values.insert("message".into(), "overridden".into());
    let manifest = new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit).unwrap();
    assert_eq!(
        manifest["spec"]["workflowTemplateRef"],
        json!({"name":"run-template","clusterScope":false})
    );
    assert_eq!(manifest["spec"]["entrypoint"], "alternative");
    let parameters = manifest["spec"]["arguments"]["parameters"]
        .as_array()
        .unwrap();
    assert_eq!(parameters[0]["value"], "overridden");
    assert_eq!(
        parameters[1]["value"], "fast",
        "default is an actual submitted parameter value"
    );
    assert_eq!(
        parameters[2]["value"], "",
        "explicit empty string is supplied, not missing"
    );
    assert_eq!(
        parameters[3]["valueFrom"],
        source.data["spec"]["arguments"]["parameters"][3]["valueFrom"]
    );
    assert!(parameters[3].get("value").is_none());
    assert_eq!(
        manifest["spec"]["arguments"]["artifacts"],
        source.data["spec"]["arguments"]["artifacts"]
    );
    assert!(manifest.pointer("/spec/templates").is_none());
    assert_eq!(manifest["metadata"]["labels"]["created-from"], "template");
    assert_eq!(manifest["metadata"]["annotations"]["note"], "keep");
    for pointer in [
        "/metadata/name",
        "/metadata/uid",
        "/metadata/resourceVersion",
        "/metadata/managedFields",
        "/metadata/creationTimestamp",
        "/metadata/ownerReferences",
        "/status",
    ] {
        assert!(
            manifest.pointer(pointer).is_none(),
            "retained execution field {pointer}"
        );
    }
    assert!(
        manifest["metadata"]["labels"]
            .get("workflows.argoproj.io/completed")
            .is_none()
    );
    assert_eq!(serde_json::to_value(source.as_ref()).unwrap(), template());

    inputs.values.insert("mode".into(), "invalid".into());
    assert!(
        new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit)
            .unwrap_err()
            .user_message()
            .contains("mode must be one of")
    );
    inputs.values.remove("mode");
    inputs.values.insert("unknown".into(), "value".into());
    assert!(
        new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit)
            .unwrap_err()
            .user_message()
            .contains("unknown Workflow parameter")
    );
}

#[test]
fn submission_requires_an_entrypoint_and_cron_runs_only_the_workflow_spec() {
    let mut missing = template();
    missing["spec"]
        .as_object_mut()
        .unwrap()
        .remove("entrypoint");
    let source = dynamic(missing);
    assert!(
        new_workflow("WorkflowTemplate", &source, &supplied(), Action::Submit)
            .unwrap_err()
            .user_message()
            .contains("choose an entrypoint")
    );
    let inputs = Inputs {
        entrypoint: "absent".into(),
        ..supplied()
    };
    assert!(
        new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit)
            .unwrap_err()
            .user_message()
            .contains("chosen entrypoint")
    );
    let cron = dynamic(
        json!({"apiVersion":"argoproj.io/v1alpha1","kind":"CronWorkflow","metadata":{"name":"schedule","namespace":"team-a","uid":"cron-uid","resourceVersion":"8"},"spec":{"suspend":true,"schedules":["* * * * *"],"concurrencyPolicy":"Forbid","workflowSpec":template()["spec"]}}),
    );
    let manifest = new_workflow("CronWorkflow", &cron, &supplied(), Action::Submit).unwrap();
    assert_eq!(
        manifest["spec"]["templates"],
        template()["spec"]["templates"]
    );
    for field in ["suspend", "schedules", "concurrencyPolicy", "workflowSpec"] {
        assert!(manifest["spec"].get(field).is_none());
    }
}

#[tokio::test]
async fn reviewed_submission_dry_runs_without_saving_and_creates_a_new_execution() {
    let fixture = Fixture::start(vec![template()]).await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("WorkflowTemplate", "workflowtemplates"),
        dynamic(template()),
        Action::Submit,
        supplied(),
    )
    .await
    .unwrap();
    assert!(
        writes(&fixture).is_empty(),
        "review/cancellation must not write"
    );
    assert_eq!(
        fixture.object("WorkflowTemplate", "run-template"),
        template()
    );
    let requests = fixture.requests.lock().unwrap().clone();
    assert!(requests.iter().any(|(line, _)| {
        line.starts_with("POST /apis/argoproj.io/v1alpha1/namespaces/team-a/workflows?")
            && line.contains("dryRun=All")
    }));
    assert!(
        requests
            .iter()
            .any(|(line, body)| line.contains("selfsubjectrulesreviews")
                && body.pointer("/spec/namespace") == Some(&json!("team-a")))
    );
    let created = execute(session, &plan).await.unwrap();
    assert_eq!(created.metadata.namespace.as_deref(), Some("team-a"));
    assert!(
        created
            .metadata
            .name
            .as_deref()
            .unwrap()
            .starts_with("run-template-")
    );
    assert_eq!(created.data["status"]["phase"], "Pending");
    assert_eq!(
        fixture.object("WorkflowTemplate", "run-template"),
        template()
    );
    assert_eq!(writes(&fixture).len(), 1);
}

#[tokio::test]
async fn cluster_template_uses_the_chosen_namespace_for_creation_and_permissions() {
    let mut source = template();
    source["kind"] = json!("ClusterWorkflowTemplate");
    source["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("namespace");
    let fixture = Fixture::start(vec![source.clone()]).await;
    fixture
        .rules(json!([{"verbs":["create"],"apiGroups":["argoproj.io"],"resources":["workflows"]}]));
    let session = connect(&fixture);
    let no_namespace = prepare(
        session.clone(),
        kind("ClusterWorkflowTemplate", "clusterworkflowtemplates"),
        dynamic(source.clone()),
        Action::Submit,
        supplied(),
    )
    .await
    .err()
    .unwrap();
    assert!(no_namespace.user_message().contains("choose the namespace"));
    let inputs = Inputs {
        namespace: "target-team".into(),
        ..supplied()
    };
    let plan = prepare(
        session.clone(),
        kind("ClusterWorkflowTemplate", "clusterworkflowtemplates"),
        dynamic(source.clone()),
        Action::Submit,
        inputs,
    )
    .await
    .unwrap();
    assert_eq!(
        plan.proposed["spec"]["workflowTemplateRef"],
        json!({"name":"run-template","clusterScope":true})
    );
    let created = execute(session, &plan).await.unwrap();
    assert_eq!(created.metadata.namespace.as_deref(), Some("target-team"));
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(line, body)| line.contains("selfsubjectrulesreviews")
                && body.pointer("/spec/namespace") == Some(&json!("target-team")))
    );
    assert!(
        writes(&fixture)[0]
            .0
            .contains("/namespaces/target-team/workflows")
    );
    assert_eq!(
        fixture.object("ClusterWorkflowTemplate", "run-template"),
        source
    );
}

#[tokio::test]
async fn resubmission_preserves_the_original_record_and_drops_completion_controls() {
    let original = failed_workflow();
    let fixture = Fixture::start(vec![original.clone()]).await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(original.clone()),
        Action::Resubmit,
        Inputs::default(),
    )
    .await
    .unwrap();
    assert!(plan.proposed.get("status").is_none());
    assert!(plan.proposed["spec"].get("shutdown").is_none());
    assert!(plan.proposed["spec"].get("activeDeadlineSeconds").is_none());
    assert_eq!(
        plan.proposed["spec"]["templates"],
        original["spec"]["templates"]
    );
    let created = execute(session, &plan).await.unwrap();
    assert_ne!(created.metadata.name.as_deref(), Some("run"));
    assert_eq!(created.metadata.generate_name.as_deref(), Some("run-"));
    assert_eq!(fixture.object("Workflow", "run"), original);
    assert_eq!(writes(&fixture).len(), 1);
}

#[tokio::test]
async fn template_uid_or_spec_changes_after_review_require_a_new_review() {
    for replacement in [false, true] {
        let fixture = Fixture::start(vec![template()]).await;
        let session = connect(&fixture);
        let plan = prepare(
            session.clone(),
            kind("WorkflowTemplate", "workflowtemplates"),
            dynamic(template()),
            Action::Submit,
            supplied(),
        )
        .await
        .unwrap();
        let mut changed = template();
        changed["metadata"]["resourceVersion"] = json!("9");
        if replacement {
            changed["metadata"]["uid"] = json!("replacement-uid");
        } else {
            changed["spec"]["templates"][0]["container"]["image"] = json!("unexpected-image");
        }
        fixture.put(changed);
        let error = execute(session, &plan).await.unwrap_err().user_message();
        assert!(
            error.contains(if replacement {
                "was replaced"
            } else {
                "source changed after review"
            }),
            "{error}"
        );
        assert!(writes(&fixture).is_empty());
    }
}

#[tokio::test]
async fn permissions_are_checked_on_workflows_instead_of_template_write_permissions() {
    let fixture = Fixture::start(vec![template()]).await;
    fixture.rules(json!([{"verbs":["create","patch"],"apiGroups":["argoproj.io"],"resources":["workflowtemplates"]}]));
    let error = prepare(
        connect(&fixture),
        kind("WorkflowTemplate", "workflowtemplates"),
        dynamic(template()),
        Action::Submit,
        supplied(),
    )
    .await
    .err()
    .unwrap()
    .user_message();
    assert!(error.contains("create on argoproj.io/workflows"));
    assert!(writes(&fixture).is_empty());

    let fixture = Fixture::start(vec![workflow("Running")]).await;
    fixture.rules(json!([{"verbs":["patch"],"apiGroups":["argoproj.io"],"resources":["workflows"],"resourceNames":["other"]}]));
    let error = prepare(
        connect(&fixture),
        kind("Workflow", "workflows"),
        dynamic(workflow("Running")),
        Action::Stop,
        Inputs::default(),
    )
    .await
    .err()
    .unwrap()
    .user_message();
    assert!(error.contains("patch on argoproj.io/workflows"));
    assert!(writes(&fixture).is_empty());

    let fixture = Fixture::start(vec![workflow("Running")]).await;
    fixture.rules(json!([{"verbs":["patch"],"apiGroups":["argoproj.io"],"resources":["workflows"],"resourceNames":["run"]}]));
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(workflow("Running")),
        Action::Suspend,
        Inputs::default(),
    )
    .await
    .unwrap();
    execute(session, &plan).await.unwrap();
    assert_eq!(fixture.object("Workflow", "run")["spec"]["suspend"], true);
}

#[tokio::test]
async fn controls_preserve_status_and_rebase_ongoing_controller_updates() {
    let fixture = Fixture::start(vec![workflow("Running")]).await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(workflow("Running")),
        Action::Suspend,
        Inputs::default(),
    )
    .await
    .unwrap();
    assert!(writes(&fixture).is_empty());
    assert!(
        fixture.object("Workflow", "run")["spec"]
            .get("suspend")
            .is_none()
    );
    let mut updated = workflow("Running");
    updated["metadata"]["resourceVersion"] = json!("9");
    updated["status"]["message"] = json!("controller has progressed");
    fixture.put(updated.clone());
    let suspended = execute(session.clone(), &plan).await.unwrap();
    assert_eq!(suspended.data["spec"]["suspend"], true);
    assert_eq!(suspended.data["status"], updated["status"]);
    let last = writes(&fixture).pop().unwrap().1;
    assert!(
        last.as_array()
            .unwrap()
            .iter()
            .any(|operation| operation["op"] == "test"
                && operation["path"] == "/metadata/resourceVersion"
                && operation["value"] == "9")
    );
    let resume = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        Arc::new(suspended),
        Action::Resume,
        Inputs::default(),
    )
    .await
    .unwrap();
    let resumed = execute(session.clone(), &resume).await.unwrap();
    assert!(resumed.data["spec"].get("suspend").is_none());
    assert_eq!(resumed.data["status"], updated["status"]);
    let stop = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        Arc::new(resumed),
        Action::Stop,
        Inputs::default(),
    )
    .await
    .unwrap();
    let stopping = execute(session.clone(), &stop).await.unwrap();
    assert_eq!(stopping.data["spec"]["shutdown"], "Stop");
    let terminate = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        Arc::new(stopping),
        Action::Terminate,
        Inputs::default(),
    )
    .await
    .unwrap();
    let terminated = execute(session, &terminate).await.unwrap();
    assert_eq!(terminated.data["spec"]["shutdown"], "Terminate");
    assert_eq!(terminated.data["status"], updated["status"]);
}

#[test]
fn actions_follow_execution_state_and_only_apply_to_canonical_argo_kinds() {
    let running = dynamic(workflow("Running"));
    for action in [Action::Suspend, Action::Stop, Action::Terminate] {
        assert!(action.available("Workflow", &running));
    }
    for action in [Action::Retry, Action::Resubmit, Action::Resume] {
        assert!(!action.available("Workflow", &running));
    }
    let failed = dynamic(workflow("Failed"));
    assert!(Action::Retry.available("Workflow", &failed));
    assert!(Action::Resubmit.available("Workflow", &failed));
    for action in [
        Action::Suspend,
        Action::Resume,
        Action::Stop,
        Action::Terminate,
    ] {
        assert!(!action.available("Workflow", &failed));
    }
    let succeeded = dynamic(workflow("Succeeded"));
    assert!(Action::Resubmit.available("Workflow", &succeeded));
    assert!(!Action::Retry.available("Workflow", &succeeded));
    let mut suspended = workflow("Running");
    suspended["spec"]["suspend"] = json!(true);
    let suspended = dynamic(suspended);
    assert!(Action::Resume.available("Workflow", &suspended));
    assert!(!Action::Suspend.available("Workflow", &suspended));
    let mut stopping = workflow("Running");
    stopping["spec"]["shutdown"] = json!("Stop");
    let stopping = dynamic(stopping);
    assert!(!Action::Stop.available("Workflow", &stopping));
    assert!(!Action::Suspend.available("Workflow", &stopping));
    assert!(!Action::Resume.available("Workflow", &stopping));
    assert!(Action::Terminate.available("Workflow", &stopping));
    assert!(Action::Submit.applies(argo::GROUP, "WorkflowTemplate"));
    assert!(!Action::Submit.applies("unrelated.example", "WorkflowTemplate"));
    assert!(!Action::Retry.applies("unrelated.example", "Workflow"));
}

#[tokio::test]
async fn retry_deletes_only_reviewed_failed_and_exit_pods_and_keeps_successful_branches() {
    let original = failed_workflow();
    let mut unrelated = pod("unrelated");
    unrelated["metadata"]["ownerReferences"][0]["uid"] = json!("another-workflow");
    let fixture = Fixture::start(vec![
        original.clone(),
        pod("run-ok"),
        pod("run-fail"),
        pod("run-exit"),
        unrelated.clone(),
    ])
    .await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(original.clone()),
        Action::Retry,
        Inputs::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        plan.pods
            .iter()
            .map(|pod| pod.reference.name.as_str())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["run-exit", "run-fail"])
    );
    assert!(writes(&fixture).is_empty());
    assert_eq!(fixture.object("Workflow", "run"), original);
    let result = execute(session.clone(), &plan).await.unwrap();
    assert_eq!(result.metadata.name.as_deref(), Some("run"));
    assert_eq!(result.metadata.uid.as_deref(), Some("workflow-uid"));
    let nodes = argo::nodes(&result.data).unwrap();
    assert_eq!(nodes["run-ok"], original["status"]["nodes"]["run-ok"]);
    assert_eq!(nodes["run"]["phase"], "Running");
    assert!(!nodes.contains_key("run-fail"));
    assert!(!nodes.contains_key("run-on-exit"));
    assert!(!nodes.contains_key("run-exit"));
    assert_eq!(fixture.object("Pod", "run-ok"), pod("run-ok"));
    assert_eq!(fixture.object("Pod", "unrelated"), unrelated);
    for name in ["run-fail", "run-exit"] {
        assert!(
            matches!(session.clone().get_object(resources::pod(), Some("team-a".into()), name.into()).await, Err(Error::Api(kube::Error::Api(status))) if status.code == 404)
        );
    }
    let writes = writes(&fixture);
    let deletions: Vec<_> = writes
        .iter()
        .filter(|(line, _)| line.starts_with("DELETE "))
        .collect();
    assert_eq!(deletions.len(), 2);
    for (line, body) in deletions {
        let name = if line.contains("run-fail") {
            "run-fail"
        } else {
            "run-exit"
        };
        assert_eq!(
            body.pointer("/preconditions/uid"),
            Some(&json!(format!("uid-{name}")))
        );
    }
    assert!(
        writes.last().unwrap().0.starts_with("PATCH "),
        "Pods terminate before retry status reset"
    );
}

#[tokio::test]
async fn retry_rejects_stale_workflow_nodes_before_deleting_any_pod() {
    let fixture = Fixture::start(vec![failed_workflow(), pod("run-fail")]).await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(failed_workflow()),
        Action::Retry,
        Inputs::default(),
    )
    .await
    .unwrap();
    let mut changed = failed_workflow();
    changed["metadata"]["resourceVersion"] = json!("9");
    changed["status"]["nodes"]["run-fail"]["message"] = json!("changed controller result");
    fixture.put(changed.clone());
    let error = execute(session, &plan).await.unwrap_err().user_message();
    assert!(error.contains("nodes changed after review"), "{error}");
    assert!(writes(&fixture).is_empty());
    assert_eq!(fixture.object("Pod", "run-fail"), pod("run-fail"));
    assert_eq!(fixture.object("Workflow", "run"), changed);
}

#[tokio::test]
async fn retry_requires_pod_delete_permission_and_proves_workflow_ownership() {
    let fixture = Fixture::start(vec![failed_workflow(), pod("run-fail")]).await;
    fixture
        .rules(json!([{"verbs":["patch"],"apiGroups":["argoproj.io"],"resources":["workflows"]}]));
    let error = prepare(
        connect(&fixture),
        kind("Workflow", "workflows"),
        dynamic(failed_workflow()),
        Action::Retry,
        Inputs::default(),
    )
    .await
    .err()
    .unwrap()
    .user_message();
    assert!(error.contains("delete on pods"));
    assert!(writes(&fixture).is_empty());
    let mut impostor = pod("run-fail");
    impostor["metadata"]["ownerReferences"][0]["uid"] = json!("another-workflow");
    let fixture = Fixture::start(vec![failed_workflow(), impostor.clone()]).await;
    let error = prepare(
        connect(&fixture),
        kind("Workflow", "workflows"),
        dynamic(failed_workflow()),
        Action::Retry,
        Inputs::default(),
    )
    .await
    .err()
    .unwrap()
    .user_message();
    assert!(error.contains("not owned by this Workflow"));
    assert!(writes(&fixture).is_empty());
    assert_eq!(fixture.object("Pod", "run-fail"), impostor);
}

#[tokio::test]
async fn retry_pod_uid_preconditions_and_server_denials_preserve_the_workflow() {
    for replacement in [false, true] {
        let fixture = Fixture::start(vec![failed_workflow(), pod("run-fail")]).await;
        let session = connect(&fixture);
        let plan = prepare(
            session.clone(),
            kind("Workflow", "workflows"),
            dynamic(failed_workflow()),
            Action::Retry,
            Inputs::default(),
        )
        .await
        .unwrap();
        if replacement {
            let mut replaced = pod("run-fail");
            replaced["metadata"]["uid"] = json!("replaced-pod-uid");
            fixture.put(replaced);
        } else {
            fixture.deny("delete/pods/run-fail");
        }
        let error = execute(session, &plan).await.unwrap_err().user_message();
        assert!(
            error.contains("could not delete team-a/run-fail"),
            "{error}"
        );
        assert!(error.contains("Workflow was not changed"));
        assert!(
            error.contains(if replacement {
                "UID changed"
            } else {
                "Forbidden"
            }) || !replacement && error.contains("Fixture refuses")
        );
        assert_eq!(fixture.object("Workflow", "run"), failed_workflow());
        assert!(
            !writes(&fixture)
                .iter()
                .any(|(line, _)| line.starts_with("PATCH "))
        );
    }
}

fn suspended_workflow(compressed: bool) -> Value {
    let mut workflow = workflow("Running");
    workflow["spec"]["suspend"] = json!(true);
    let nodes = json!({
        "run":{"id":"run","name":"run","type":"Steps","phase":"Running","children":["run-wait","run-done"]},
        "run-wait":{"id":"run-wait","name":"run.wait","displayName":"approval","type":"Suspend","phase":"Running","outputs":{"parameters":[{"name":"approved","globalName":"approval","valueFrom":{"supplied":{}}},{"name":"mode","valueFrom":{"supplied":{},"default":"safe"}}]}},
        "run-done":{"id":"run-done","name":"run.done","type":"Pod","phase":"Succeeded","outputs":{"parameters":[{"name":"keep","value":"success"}]}}
    });
    if compressed {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(nodes.to_string().as_bytes()).unwrap();
        workflow["status"]["compressedNodes"] = json!(STANDARD.encode(encoder.finish().unwrap()));
        workflow["status"]["nodes"] = json!({});
    } else {
        workflow["status"]["nodes"] = nodes;
    }
    workflow
}

#[tokio::test]
async fn resume_supplies_outputs_completes_suspend_nodes_and_preserves_compression() {
    for compressed in [false, true] {
        let original = suspended_workflow(compressed);
        let fixture = Fixture::start(vec![original.clone()]).await;
        let session = connect(&fixture);
        let source = dynamic(original.clone());
        let definitions = parameters("Workflow", &source, Action::Resume).unwrap();
        assert_eq!(definitions[0].key, "run-wait/approved");
        assert!(definitions[0].required);
        assert_eq!(definitions[1].value.as_deref(), Some("safe"));
        let error = prepare(
            session.clone(),
            kind("Workflow", "workflows"),
            source.clone(),
            Action::Resume,
            Inputs::default(),
        )
        .await
        .err()
        .unwrap()
        .user_message();
        assert!(error.contains("provide the suspended output parameter approved"));
        assert!(writes(&fixture).is_empty());
        let inputs = Inputs {
            values: BTreeMap::from([("run-wait/approved".into(), "yes".into())]),
            ..Default::default()
        };
        let plan = prepare(
            session.clone(),
            kind("Workflow", "workflows"),
            source,
            Action::Resume,
            inputs,
        )
        .await
        .unwrap();
        assert_eq!(
            fixture.object("Workflow", "run"),
            original,
            "review leaves suspend status untouched"
        );
        let resumed = execute(session, &plan).await.unwrap();
        assert!(resumed.data["spec"].get("suspend").is_none());
        let nodes = argo::nodes(&resumed.data).unwrap();
        assert_eq!(nodes["run-wait"]["phase"], "Succeeded");
        assert_eq!(
            nodes["run-wait"]["outputs"]["parameters"][0]["value"],
            "yes"
        );
        assert_eq!(
            nodes["run-wait"]["outputs"]["parameters"][1]["value"],
            "safe"
        );
        assert!(
            nodes["run-wait"]["outputs"]["parameters"][0]
                .get("valueFrom")
                .is_none()
        );
        assert!(nodes["run-wait"]["finishedAt"].as_str().is_some());
        assert_eq!(
            nodes["run-done"],
            argo::nodes(&dynamic(original).data).unwrap()["run-done"]
        );
        let globals = resumed.data["status"]["outputs"]["parameters"]
            .as_array()
            .unwrap();
        assert!(
            globals
                .iter()
                .any(|parameter| parameter["name"] == "keep" && parameter["value"] == "retained")
        );
        assert!(
            globals
                .iter()
                .any(|parameter| parameter["name"] == "approval" && parameter["value"] == "yes"),
            "Argo globalName output receives the supplied value"
        );
        assert_eq!(
            resumed
                .data
                .pointer("/status/compressedNodes")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
            compressed
        );
        if compressed {
            assert_eq!(resumed.data["status"]["nodes"], json!({}));
        }
    }
}

#[tokio::test]
async fn resume_rejects_changed_suspend_nodes_and_offloaded_status_is_explicit() {
    let original = suspended_workflow(false);
    let fixture = Fixture::start(vec![original.clone()]).await;
    let session = connect(&fixture);
    let inputs = Inputs {
        values: BTreeMap::from([("run-wait/approved".into(), "yes".into())]),
        ..Default::default()
    };
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(original.clone()),
        Action::Resume,
        inputs,
    )
    .await
    .unwrap();
    let mut changed = original;
    changed["metadata"]["resourceVersion"] = json!("9");
    changed["status"]["nodes"]["run-wait"]["phase"] = json!("Succeeded");
    fixture.put(changed.clone());
    let error = execute(session, &plan).await.unwrap_err().user_message();
    assert!(error.contains("suspended steps changed after review"));
    assert!(writes(&fixture).is_empty());
    assert_eq!(fixture.object("Workflow", "run"), changed);

    for action in [Action::Retry, Action::Resume] {
        let mut offloaded = workflow(if action == Action::Retry {
            "Failed"
        } else {
            "Running"
        });
        offloaded["status"]["offloadNodeStatusVersion"] = json!("database-version");
        let fixture = Fixture::start(vec![offloaded.clone()]).await;
        let error = prepare(
            connect(&fixture),
            kind("Workflow", "workflows"),
            dynamic(offloaded),
            action,
            Inputs::default(),
        )
        .await
        .err()
        .unwrap()
        .user_message();
        assert!(
            error.contains("offloaded") || error.contains("Offloaded"),
            "{error}"
        );
        assert!(writes(&fixture).is_empty());
    }
}

#[tokio::test]
async fn authoritative_completed_state_and_patch_failures_are_actionable_without_writes() {
    let fixture = Fixture::start(vec![workflow("Succeeded")]).await;
    let error = prepare(
        connect(&fixture),
        kind("Workflow", "workflows"),
        dynamic(workflow("Running")),
        Action::Stop,
        Inputs::default(),
    )
    .await
    .err()
    .unwrap()
    .user_message();
    assert!(error.contains("state changed"));
    assert!(writes(&fixture).is_empty());

    let fixture = Fixture::start(vec![workflow("Running")]).await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(workflow("Running")),
        Action::Stop,
        Inputs::default(),
    )
    .await
    .unwrap();
    fixture.deny("patch/workflows");
    let error = execute(session, &plan).await.unwrap_err().user_message();
    assert!(error.contains("Fixture refuses PATCH workflows"), "{error}");
    assert_eq!(fixture.object("Workflow", "run"), workflow("Running"));
}

#[tokio::test]
async fn resume_requires_a_new_review_when_same_node_output_definitions_change() {
    for add_parameter in [false, true] {
        let original = suspended_workflow(false);
        let fixture = Fixture::start(vec![original.clone()]).await;
        let session = connect(&fixture);
        let inputs = Inputs {
            values: BTreeMap::from([("run-wait/approved".into(), "yes".into())]),
            ..Default::default()
        };
        let plan = prepare(
            session.clone(),
            kind("Workflow", "workflows"),
            dynamic(original.clone()),
            Action::Resume,
            inputs,
        )
        .await
        .unwrap();
        let mut changed = original;
        changed["metadata"]["resourceVersion"] = json!("9");
        let parameters = changed["status"]["nodes"]["run-wait"]["outputs"]["parameters"]
            .as_array_mut()
            .unwrap();
        if add_parameter {
            parameters.push(json!({
                "name":"new-output",
                "valueFrom":{"supplied":{},"default":"unreviewed-default"}
            }));
        } else {
            parameters[0]["globalName"] = json!("unreviewed-global-output");
        }
        fixture.put(changed.clone());
        let error = execute(session, &plan).await.unwrap_err().user_message();
        assert!(error.contains("outputs changed"), "{error}");
        assert!(
            writes(&fixture).is_empty(),
            "unreviewed output definitions must not be resumed"
        );
        assert_eq!(fixture.object("Workflow", "run"), changed);
    }
}

#[tokio::test]
async fn compressed_status_without_an_active_suspend_node_cannot_be_resumed() {
    let mut original = workflow("Running");
    original["spec"]["suspend"] = json!(false);
    let nodes = json!({
        "run":{"id":"run","name":"run","type":"Steps","phase":"Running","children":["run-done"]},
        "run-done":{"id":"run-done","name":"run.done","type":"Suspend","phase":"Succeeded"}
    });
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(nodes.to_string().as_bytes()).unwrap();
    original["status"]["compressedNodes"] = json!(STANDARD.encode(encoder.finish().unwrap()));
    original["status"]["nodes"] = json!({});
    let fixture = Fixture::start(vec![original.clone()]).await;
    let error = prepare(
        connect(&fixture),
        kind("Workflow", "workflows"),
        dynamic(original.clone()),
        Action::Resume,
        Inputs::default(),
    )
    .await
    .err()
    .expect("compressed status still needs an actual suspension to resume")
    .user_message();
    assert!(
        error.contains("not suspended") || error.contains("no active Suspend"),
        "{error}"
    );
    assert!(writes(&fixture).is_empty());
    assert!(
        !fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(line, _)| line.starts_with("PATCH ")),
        "do not dry-run an empty or no-op resume patch"
    );
    assert_eq!(fixture.object("Workflow", "run"), original);
}

#[tokio::test]
async fn retry_reviews_and_deletes_failed_pods_even_after_their_workflow_label_is_removed() {
    let mut failed = pod("run-fail");
    failed["metadata"]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("workflows.argoproj.io/workflow");
    let success = pod("run-ok");
    let mut unrelated = pod("unrelated");
    unrelated["metadata"]["ownerReferences"][0]["uid"] = json!("another-workflow");
    let fixture = Fixture::start(vec![
        failed_workflow(),
        failed.clone(),
        success.clone(),
        unrelated.clone(),
    ])
    .await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("Workflow", "workflows"),
        dynamic(failed_workflow()),
        Action::Retry,
        Inputs::default(),
    )
    .await
    .unwrap();
    assert_eq!(plan.pods.len(), 1);
    assert_eq!(plan.pods[0].reference.name, "run-fail");
    assert_eq!(plan.pods[0].uid, "uid-run-fail");
    assert!(writes(&fixture).is_empty());
    assert_eq!(fixture.object("Pod", "run-fail"), failed);
    execute(session.clone(), &plan).await.unwrap();
    assert!(
        matches!(session.get_object(resources::pod(), Some("team-a".into()), "run-fail".into()).await, Err(Error::Api(kube::Error::Api(status))) if status.code == 404)
    );
    assert_eq!(fixture.object("Pod", "run-ok"), success);
    assert_eq!(fixture.object("Pod", "unrelated"), unrelated);
    let deleted: Vec<_> = writes(&fixture)
        .into_iter()
        .filter(|(line, _)| line.starts_with("DELETE "))
        .collect();
    assert_eq!(deleted.len(), 1);
    assert!(
        deleted[0]
            .0
            .starts_with("DELETE /api/v1/namespaces/team-a/pods/run-fail")
    );
}

#[tokio::test]
async fn cron_submission_uses_workflow_metadata_and_keeps_the_cron_controller_identity() {
    let cron = json!({
        "apiVersion":"argoproj.io/v1alpha1", "kind":"CronWorkflow",
        "metadata":{"name":"nightly","namespace":"team-a","uid":"cron-uid","resourceVersion":"8"},
        "spec":{
            "suspend":true, "schedules":["0 0 * * *"], "workflowSpec":template()["spec"],
            "workflowMetadata":{
                "labels":{"scheduled-team":"platform"},
                "annotations":{"scheduled-note":"preserve"},
                "finalizers":["example.dev/retain-artifacts"]
            }
        }
    });
    let fixture = Fixture::start(vec![cron.clone()]).await;
    let session = connect(&fixture);
    let plan = prepare(
        session.clone(),
        kind("CronWorkflow", "cronworkflows"),
        dynamic(cron.clone()),
        Action::Submit,
        supplied(),
    )
    .await
    .unwrap();
    let metadata = &plan.proposed["metadata"];
    assert_eq!(metadata["labels"]["scheduled-team"], "platform");
    assert_eq!(metadata["annotations"]["scheduled-note"], "preserve");
    assert_eq!(
        metadata["finalizers"],
        json!(["example.dev/retain-artifacts"])
    );
    assert_eq!(
        metadata["labels"]["workflows.argoproj.io/cron-workflow"],
        "nightly"
    );
    let owners = metadata["ownerReferences"].as_array().unwrap();
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0]["apiVersion"], "argoproj.io/v1alpha1");
    assert_eq!(owners[0]["kind"], "CronWorkflow");
    assert_eq!(owners[0]["name"], "nightly");
    assert_eq!(owners[0]["uid"], "cron-uid");
    assert_eq!(owners[0]["controller"], true);
    assert!(writes(&fixture).is_empty());
    let created = execute(session, &plan).await.unwrap();
    assert_eq!(
        created.metadata.owner_references.as_ref().unwrap()[0].uid,
        "cron-uid"
    );
    assert_eq!(
        created.metadata.finalizers.as_ref().unwrap(),
        &["example.dev/retain-artifacts"]
    );
    assert_eq!(fixture.object("CronWorkflow", "nightly"), cron);
}

#[test]
fn submitted_template_workflows_carry_the_official_template_source_label() {
    let source = dynamic(template());
    let manifest = new_workflow("WorkflowTemplate", &source, &supplied(), Action::Submit).unwrap();
    assert_eq!(
        manifest["metadata"]["labels"]["workflows.argoproj.io/workflow-template"],
        "run-template"
    );
    assert!(
        manifest["metadata"]["labels"]
            .get("workflows.argoproj.io/cluster-workflow-template")
            .is_none()
    );
    let mut cluster = template();
    cluster["kind"] = json!("ClusterWorkflowTemplate");
    cluster["metadata"]
        .as_object_mut()
        .unwrap()
        .remove("namespace");
    let inputs = Inputs {
        namespace: "team-a".into(),
        ..supplied()
    };
    let manifest = new_workflow(
        "ClusterWorkflowTemplate",
        &dynamic(cluster),
        &inputs,
        Action::Submit,
    )
    .unwrap();
    assert_eq!(
        manifest["metadata"]["labels"]["workflows.argoproj.io/cluster-workflow-template"],
        "run-template"
    );
    assert!(
        manifest["metadata"]["labels"]
            .get("workflows.argoproj.io/workflow-template")
            .is_none()
    );
}

#[test]
fn template_entrypoint_inputs_are_reviewed_and_runtime_inputs_inherit_the_template_reference() {
    let mut template = template();
    template["spec"]
        .as_object_mut()
        .unwrap()
        .remove("arguments");
    template["spec"]["templates"] = json!([
        {"name":"main","inputs":{"parameters":[
            {"name":"message","description":"Required message"},
            {"name":"retries","default":3},
            {"name":"runtime","valueFrom":{"configMapKeyRef":{"name":"settings","key":"runtime"}}}
        ]},"container":{"image":"busybox"}},
        {"name":"alternative","inputs":{"parameters":[
            {"name":"environment","enum":["stage","production"]},
            {"name":"parallelism","default":2}
        ]},"container":{"image":"alpine"}}
    ]);
    let source = dynamic(template);
    let definitions =
        parameters_for_entrypoint("WorkflowTemplate", &source, Action::Submit, "main").unwrap();
    assert_eq!(
        definitions
            .iter()
            .map(|parameter| parameter.key.as_str())
            .collect::<Vec<_>>(),
        ["message", "retries", "runtime"]
    );
    assert!(definitions[0].required);
    assert_eq!(definitions[1].value.as_deref(), Some("3"));
    assert!(definitions[2].runtime);
    assert!(!definitions[2].required);
    let error = new_workflow(
        "WorkflowTemplate",
        &source,
        &Inputs::default(),
        Action::Submit,
    )
    .unwrap_err()
    .user_message();
    assert!(error.contains("provide a value for message"), "{error}");
    for message in ["", "user value"] {
        let inputs = Inputs {
            values: BTreeMap::from([("message".into(), message.into())]),
            ..Default::default()
        };
        let manifest = new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit).unwrap();
        assert_eq!(
            manifest["spec"]["workflowTemplateRef"]["name"],
            "run-template"
        );
        let parameters = manifest["spec"]["arguments"]["parameters"]
            .as_array()
            .unwrap();
        assert_eq!(
            parameters.len(),
            2,
            "no bare runtime input may shadow the referenced template input"
        );
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == "message" && parameter["value"] == message)
        );
        assert!(
            parameters
                .iter()
                .any(|parameter| parameter["name"] == "retries" && parameter["value"] == "3")
        );
        assert!(
            !parameters
                .iter()
                .any(|parameter| parameter["name"] == "runtime")
        );
    }
    let override_runtime = Inputs {
        values: BTreeMap::from([
            ("message".into(), "hello".into()),
            ("runtime".into(), "manual value".into()),
        ]),
        ..Default::default()
    };
    let manifest = new_workflow(
        "WorkflowTemplate",
        &source,
        &override_runtime,
        Action::Submit,
    )
    .unwrap();
    assert!(manifest["spec"]["arguments"]["parameters"].as_array().unwrap().iter().any(|parameter| parameter["name"] == "runtime" && parameter["value"] == "manual value"));

    let definitions =
        parameters_for_entrypoint("WorkflowTemplate", &source, Action::Submit, "alternative")
            .unwrap();
    assert_eq!(
        definitions
            .iter()
            .map(|parameter| parameter.key.as_str())
            .collect::<Vec<_>>(),
        ["environment", "parallelism"]
    );
    let mut inputs = Inputs {
        entrypoint: "alternative".into(),
        values: BTreeMap::from([("environment".into(), "stage".into())]),
        ..Default::default()
    };
    let manifest = new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit).unwrap();
    assert_eq!(manifest["spec"]["entrypoint"], "alternative");
    let parameters = manifest["spec"]["arguments"]["parameters"]
        .as_array()
        .unwrap();
    assert_eq!(parameters.len(), 2);
    assert!(
        parameters
            .iter()
            .any(|parameter| parameter["name"] == "environment" && parameter["value"] == "stage")
    );
    assert!(
        parameters
            .iter()
            .any(|parameter| parameter["name"] == "parallelism" && parameter["value"] == "2")
    );
    inputs
        .values
        .insert("message".into(), "unreviewed main parameter".into());
    let error = new_workflow("WorkflowTemplate", &source, &inputs, Action::Submit)
        .unwrap_err()
        .user_message();
    assert!(error.contains("unknown Workflow parameter"), "{error}");
}
