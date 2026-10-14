use super::*;
use crate::feature_test_support as support;
use gpui_kit::test::TestWindowExt as _;
use serde_json::json;

fn fixture(cx: &mut TestAppContext) -> (beacon_kube::test_support::Fixture, Arc<ClusterSession>) {
    let objects = vec![
        json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"Role","metadata":{"name":"logs","namespace":"team"},"rules":[{"verbs":["get"],"apiGroups":[""],"resources":["pods/log"],"resourceNames":["app"]}]}),
        json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":"runner-logs","namespace":"team"},"subjects":[{"kind":"ServiceAccount","name":"runner","namespace":"team"}],"roleRef":{"kind":"Role","name":"logs","apiGroup":rbac::GROUP}}),
        json!({"apiVersion":"rbac.authorization.k8s.io/v1","kind":"RoleBinding","metadata":{"name":"team-logs","namespace":"team"},"subjects":[{"kind":"Group","name":"team"}],"roleRef":{"kind":"Role","name":"logs","apiGroup":rbac::GROUP}}),
    ];
    let runtime = cx.read(|cx| Bridge::global(cx).handle());
    runtime.block_on(async move {
        let fixture = beacon_kube::test_support::Fixture::start(objects).await;
        let mut kinds = vec![
            support::kind("", "Pod", "pods"),
            support::kind("", "ServiceAccount", "serviceaccounts"),
        ];
        for (kind, plural) in [
            ("Role", "roles"),
            ("RoleBinding", "rolebindings"),
            ("ClusterRole", "clusterroles"),
            ("ClusterRoleBinding", "clusterrolebindings"),
        ] {
            let mut known = support::kind(rbac::GROUP, kind, plural);
            known.namespaced = !kind.starts_with("Cluster");
            kinds.push(known);
        }
        let session = Arc::new(ClusterSession::for_testing(
            beacon_kube::ClusterId::new("permissions"),
            fixture.url.clone(),
            kinds,
        ));
        (fixture, session)
    })
}
fn open(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
    subject: Subject,
) -> (AnyWindowHandle, Entity<Permissions>) {
    let query = Query {
        verb: "get".into(),
        resource: "pods".into(),
        subresource: "log".into(),
        namespace: Some("team".into()),
        name: Some("app".into()),
        ..Default::default()
    };
    let (window, view) = cx.update(|cx| {
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            window.set_view_retention(false);
            cx.new(|cx| Permissions::new(session, subject, query, window, cx))
        })
        .unwrap()
    });
    (window, view)
}
#[::core::prelude::v1::test]
fn serviceaccount_queries_exact_attributes_and_navigates_to_rules_then_switches_to_current_user() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = fixture(cx);
    let (window, view) = open(
        cx,
        session,
        Subject::ServiceAccount {
            namespace: "team".into(),
            name: "runner".into(),
        },
    );
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.loading && view.grants.len() == 1)
    });
    view.read_with(cx, |view, _| {
        assert!(!view.grants[0].missing);
        assert_eq!(view.grants[0].rules[0]["resources"], json!(["pods/log"]));
    });
    let clicked = Arc::new(std::sync::Mutex::new(None));
    let observed = clicked.clone();
    let _subscription = cx.update(|cx| {
        cx.subscribe(&view, move |_, event: &OwnerRequested, _| {
            *observed.lock().unwrap() =
                Some((event.kind.resource.kind.clone(), event.target.clone()))
        })
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("permission-role", 0usize), cx);
        window.click("permission-can-i", cx);
    })
    .unwrap();
    assert_eq!(
        *clicked.lock().unwrap(),
        Some(("Role".into(), ObjectRef::new(Some("team".into()), "logs")))
    );
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.checking && view.answer.is_some())
    });
    assert!(view.read_with(cx, |view, _| view.answer.as_ref().unwrap().0));
    {
        let requests = fixture.requests.lock().unwrap();
        let (_, review) = requests
            .iter()
            .find(|(request, _)| request.contains("/subjectaccessreviews"))
            .unwrap();
        assert_eq!(review["spec"]["user"], "system:serviceaccount:team:runner");
        assert_eq!(
            review["spec"]["resourceAttributes"],
            json!({"verb":"get","group":"","resource":"pods","subresource":"log","namespace":"team","name":"app"})
        );
        assert!(
            review["spec"]["groups"]
                .as_array()
                .unwrap()
                .contains(&json!("system:serviceaccounts:team"))
        );
    }
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("permission-subject-mode", 0usize), cx);
        window.click("permission-inspect", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| {
            !view.loading
                && view
                    .identity
                    .as_ref()
                    .and_then(|identity| identity.user.as_deref())
                    == Some("alice")
        })
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.grants[0].binding.name.clone()),
        "team-logs"
    );
    cx.update_window(window, |_, window, cx| window.click("permission-can-i", cx))
        .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.checking && view.answer.is_some())
    });
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|(request, _)| request.contains("/selfsubjectaccessreviews"))
    );
    assert!(
        fixture
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(request, _)| request.starts_with("GET")
                || (request.starts_with("POST") && request.contains("review")))
    );
}
#[::core::prelude::v1::test]
fn forbidden_reviews_and_missing_sources_are_unknown_and_subject_changes_clear_results() {
    let cx = &mut support::context();
    let directory = tempfile::tempdir().unwrap();
    support::workspace(cx, directory.path());
    let (fixture, session) = fixture(cx);
    fixture.deny("roles");
    fixture.deny("subjectaccessreviews");
    let (window, view) = open(cx, session, Subject::Group("team".into()));
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.loading && view.grants.len() == 1)
    });
    view.read_with(cx, |view, _| {
        assert!(view.grants[0].missing);
        assert!(view.grants[0].rules.is_empty());
        assert!(view.errors[0].contains("403"));
    });
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click("permission-can-i", cx);
    })
    .unwrap();
    support::settle(cx, |cx| {
        view.read_with(cx, |view, _| !view.checking && view.answer.is_some())
    });
    assert!(view.read_with(cx, |view, _| {
        view.answer
            .as_ref()
            .unwrap()
            .1
            .contains("could not be evaluated")
    }));
    cx.update_window(window, |_, window, cx| {
        window.render_frame(cx);
        window.click(("permission-subject-mode", 2usize), cx);
    })
    .unwrap();
    view.read_with(cx, |view, _| {
        assert!(view.answer.is_none());
        assert!(view.grants.is_empty());
        assert!(view.dirty);
    });
}
