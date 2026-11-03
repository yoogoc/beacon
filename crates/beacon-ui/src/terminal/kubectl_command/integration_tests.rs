use super::*;
use crate::feature_test_support as support;
use beacon_kube::test_support::Fixture;
use gpui_kit::test::TestWindowExt as _;

fn open(
    cx: &mut TestAppContext,
    session: Arc<ClusterSession>,
    width: f32,
) -> (AnyWindowHandle, Entity<TerminalView>) {
    cx.update(|cx| {
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                    gpui_kit::Point::default(),
                    size(px(width), px(360.)),
                ))),
                ..Default::default()
            },
            cx,
            |window, cx| {
                window.set_view_retention(false);
                cx.new(|cx| {
                    TerminalView::new(
                        session,
                        "default".into(),
                        "web".into(),
                        Some("main".into()),
                        window,
                        cx,
                    )
                })
            },
        )
        .unwrap()
    })
}
fn draw(cx: &mut TestAppContext, window: AnyWindowHandle) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update_window(window, |_, window, cx| window.render_frame(cx))
            .unwrap();
    }
}
fn copy(cx: &mut TestAppContext, window: AnyWindowHandle) -> String {
    cx.update_window(window, |_, window, cx| {
        window.click("copy-shell-command", cx)
    })
    .unwrap();
    cx.update(|cx| cx.read_from_clipboard().unwrap().text().unwrap())
}
#[::core::prelude::v1::test]
fn copying_reads_current_command_and_never_starts_a_remote_session() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (fixture, session) = support::fixture(cx, "terminal-copy", vec![]);
    let (window, terminal) = open(cx, session, 800.);
    draw(cx, window);
    let text = copy(cx, window);
    assert!(text.starts_with(
        "kubectl exec -it web -n default --context terminal-copy -c main -- /bin/sh -c "
    ));
    assert!(text.contains("command -v bash") && text.contains("exec /bin/sh -i"));
    cx.update_window(window, |_, window, cx| {
        let command = terminal.read(cx).command.clone();
        command.update(cx, |input, cx| {
            input.set_value("sh -c 'printf \"%s\" \"a b\"'", window, cx)
        });
    })
    .unwrap();
    draw(cx, window);
    let text = copy(cx, window);
    assert!(text.ends_with("-- sh -c 'printf \"%s\" \"a b\"'"));
    assert!(
        terminal.read_with(cx, |view, _| matches!(view.state, State::Idle)
            && view.shell.is_none())
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
}
#[::core::prelude::v1::test]
fn copied_commands_use_the_real_context_and_durable_source_file() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let file = dir.path().join("team's config.yaml");
    std::fs::write(&file,"apiVersion: v1\nkind: Config\nclusters:\n- name: local\n  cluster:\n    server: https://example.invalid\ncontexts:\n- name: dev context\n  context:\n    cluster: local\n    user: auth\nusers:\n- name: auth\n  user:\n    token: fake-secret-not-for-clipboard\n").unwrap();
    let preferences = beacon_kube::sources::Preferences {
        system_enabled: false,
        sources: vec![beacon_kube::sources::Source {
            id: "custom".into(),
            name: "Custom config".into(),
            enabled: true,
            excluded: Default::default(),
            kind: beacon_kube::sources::SourceKind::File { path: file.clone() },
        }],
    };
    let catalog = beacon_kube::sources::load(&preferences, dir.path(), &[]);
    let id = catalog.contexts.entries()[0].id.clone();
    cx.update(|cx| {
        crate::sources::store(cx).update(cx, |state, _| state.catalog = Arc::new(catalog))
    });
    let runtime = cx.read(|cx| crate::Bridge::global(cx).handle());
    let fixture = runtime.block_on(Fixture::start(vec![]));
    let session = {
        let _entered = runtime.enter();
        Arc::new(ClusterSession::for_testing(id, fixture.url.clone(), vec![]))
    };
    let (window, terminal) = open(cx, session, 800.);
    cx.update_window(window, |_, window, cx| {
        let command = terminal.read(cx).command.clone();
        command.update(cx, |input, cx| input.set_value("bash", window, cx));
    })
    .unwrap();
    draw(cx, window);
    let text = copy(cx, window);
    assert!(text.contains("--context 'dev context'"));
    assert!(text.contains("--kubeconfig "));
    assert!(text.contains(&quote(
        &file.to_string_lossy(),
        if cfg!(windows) {
            Quoting::PowerShell
        } else {
            Quoting::Posix
        }
    )));
    assert!(text.ends_with("-c main -- bash"));
    assert!(!text.contains("beacon-source/") && !text.contains("fake-secret-not-for-clipboard"));
    assert!(fixture.requests.lock().unwrap().is_empty());
    // A disconnected source can disappear from the catalog while its session
    // stays open. Never silently target a same-named system context instead.
    cx.update(|cx| {
        crate::sources::store(cx).update(cx, |state, _| state.catalog = Arc::default());
        cx.write_to_clipboard(ClipboardItem::new_string("keep clipboard".into()));
    });
    assert_eq!(copy(cx, window), "keep clipboard");
}
#[::core::prelude::v1::test]
fn launcher_copy_action_fits_narrow_windows_in_both_languages() {
    let cx = &mut support::context();
    let dir = tempfile::tempdir().unwrap();
    support::workspace(cx, dir.path());
    let (_fixture, session) = support::fixture(cx, "copy-layout", vec![]);
    for language in [
        crate::i18n::Language::English,
        crate::i18n::Language::SimplifiedChinese,
    ] {
        cx.update(|cx| crate::i18n::apply(language, cx));
        for width in [320., 360., 800.] {
            let (window, _) = open(cx, session.clone(), width);
            draw(cx, window);
            cx.update_window(window, |_, window, _| {
                for id in ["start-shell", "copy-shell-command"] {
                    let bounds = window.find(id).bounds();
                    assert!(
                        bounds.left() >= px(0.) && bounds.right() <= px(width),
                        "{id} overflowed at {width}"
                    );
                }
            })
            .unwrap();
        }
    }
}
