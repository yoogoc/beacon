use super::*;

#[::core::prelude::v1::test]
fn localized_choices_keep_canonical_values_and_raw_names_when_switching_language() {
    use gpui_kit::component::select::SelectItem as _;
    let original = LOCALE.get();
    let choice = Choice::label("Last hour");
    let raw = Choice::raw("Save");
    LOCALE.set("zh-CN");
    assert_eq!(choice.title().as_ref(), "最近一小时");
    assert_eq!(choice.value(), "Last hour");
    assert!(choice.matches("一小时"));
    assert!(choice.matches("Last hour"));
    assert_eq!(raw.title().as_ref(), "Save");
    assert_eq!(
        Choice::label("Button › Primary").title().as_ref(),
        "按钮 › 主要"
    );
    assert_eq!(
        message(
            "messages.ready_ratio",
            &[("value0", "1".into()), ("value1", "2".into())]
        )
        .as_ref(),
        "1/2 就绪"
    );
    assert_eq!(text("Generation").as_ref(), "代次");
    assert_eq!(text("CA certificate bundle").as_ref(), "CA 证书链");
    LOCALE.set("en");
    assert_eq!(choice.title().as_ref(), "Last hour");
    assert_eq!(choice.value(), "Last hour");
    LOCALE.set(original);
}

#[::core::prelude::v1::test]
fn system_language_normalizes_supported_locales_and_falls_back_to_english() {
    for (system, expected) in [
        (None, "en"),
        (Some("en-GB"), "en"),
        (Some("de-DE"), "en"),
        (Some("zh"), "zh-CN"),
        (Some("zh_CN.UTF-8"), "zh-CN"),
        (Some("zh-Hans"), "zh-CN"),
        (Some("zh-Hans-SG"), "zh-CN"),
        (Some("zh-Hans-HK"), "zh-CN"),
        (Some("zh-Hant"), "en"),
        (Some("zh_TW"), "en"),
        (Some("zh-HK"), "en"),
    ] {
        assert_eq!(Language::System.resolve(system), expected, "{system:?}");
        assert_eq!(Language::English.resolve(system), "en");
        assert_eq!(Language::SimplifiedChinese.resolve(system), "zh-CN");
    }
}

#[::core::prelude::v1::test]
fn old_preferences_follow_system_and_language_roundtrips_stable_identifiers() {
    let preferences: crate::settings::Preferences = serde_json::from_str("{}").unwrap();
    assert_eq!(preferences.language, Language::System);
    let unsupported: crate::settings::Preferences =
        serde_json::from_str(r#"{"language":"fr","themes":{}}"#).unwrap();
    assert_eq!(unsupported.language, Language::English);
    for (language, value) in [
        (Language::System, "system"),
        (Language::English, "en"),
        (Language::SimplifiedChinese, "zh-CN"),
    ] {
        let encoded = serde_json::to_string(&language).unwrap();
        assert_eq!(encoded, format!("\"{value}\""));
        assert_eq!(
            serde_json::from_str::<Language>(&encoded).unwrap(),
            language
        );
    }
}

#[::core::prelude::v1::test]
fn translations_keep_raw_values_and_format_parameters_without_word_concatenation() {
    struct Restore(&'static str);
    impl Drop for Restore {
        fn drop(&mut self) {
            LOCALE.set(self.0);
        }
    }
    let _restore = Restore(LOCALE.get());
    LOCALE.set("zh-CN");
    assert_eq!(text("Save").as_ref(), "保存");
    assert_eq!(text("Pod").as_ref(), "Pod");
    assert_eq!(
        text("metadata.managedFields").as_ref(),
        "metadata.managedFields"
    );
    assert_eq!(
        text("app=web,env in (dev,prod)").as_ref(),
        "app=web,env in (dev,prod)"
    );
    assert_eq!(
        text("untranslated diagnostic").as_ref(),
        "untranslated diagnostic"
    );
    assert_eq!(
        message("resources.selected", &[("count", "100".into())]).as_ref(),
        "已选择 100 项"
    );
    assert_eq!(
        title_text("Beacon — Cluster settings · Save").as_ref(),
        "Beacon — 集群设置 · Save"
    );
    LOCALE.set("en");
    assert_eq!(text("Save").as_ref(), "Save");
    assert_eq!(
        message("resources.selected", &[("count", "1".into())]).as_ref(),
        "1 selected"
    );
}

#[cfg(feature = "ui-tests")]
#[::core::prelude::v1::test]
fn localized_column_headers_keep_persisted_ids_layout_and_canonical_filter_values() {
    use gpui_kit::component::table::TableDelegate as _;
    let cx = crate::feature_test_support::context();
    cx.update(|cx| {
        let mut table =
            crate::table::ResourceTable::new(beacon_columns::ColumnSet::for_kind("", "Pod", true));
        table.set_field_filter(crate::filters::Field::PodStatus, Some("Running".into()));
        let choices = table.column_choices();
        let mut layout = table.layout_snapshot();
        layout.order.reverse();
        layout.widths.insert(choices[0].0.clone(), 240.);
        layout.hidden.insert(choices[1].0.clone());
        table.apply_layout(layout);
        let before = table.layout_snapshot();
        let filters = table.filter_preset(Default::default());
        LOCALE.set("en");
        let heading = table.column(1, cx).name;
        LOCALE.set("zh-CN");
        let localized = table.column(1, cx).name;
        LOCALE.set("en");
        assert_ne!(heading, localized);
        assert_eq!(table.layout_snapshot(), before);
        assert_eq!(table.filter_preset(Default::default()), filters);
        assert_eq!(filters.fields[&crate::filters::Field::PodStatus], "Running");
    });
}
