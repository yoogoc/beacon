use super::*;
use crate::service_account_secret::{self as account, Inspection, Key, Timing};

impl DetailView {
    pub(super) fn service_account_key(&self, name: &str) -> Option<Key> {
        account::supported(
            &self.kind.resource.group,
            &self.kind.resource.kind,
            &self.object,
        )
        .then(|| Key::of(name))
        .flatten()
    }

    pub(super) fn copy_data_value(&self, index: usize, cx: &mut Context<Self>) {
        if !self.revealed || self.data_browser.deleted.contains(&index) {
            return;
        }
        let Data::Ready(keys) = &self.data else {
            return;
        };
        if let Some(editor) = keys.get(index).and_then(|key| key.editor.as_ref()) {
            cx.write_to_clipboard(ClipboardItem::new_string(
                editor.read(cx).value().to_string(),
            ));
        }
    }

    pub(super) fn render_service_account_value(
        &self,
        key: &DataKey,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let current = key.editor.as_ref().map(|editor| editor.read(cx).value());
        let inspection = key
            .inspection
            .as_deref()
            .filter(|_| current.as_ref() == key.inspection_value.as_ref());
        let Some(inspection) = inspection else {
            return h_flex()
                .id("account-inspection-loading")
                .test_support()
                .p_4()
                .gap_2()
                .child(Spinner::new().small())
                .child(crate::i18n::text("Inspecting value…"))
                .into_any_element();
        };
        match inspection {
            Inspection::Token(Err(error)) | Inspection::Certificate(Err(error)) => v_flex()
                .id("account-inspection-error")
                .test_support()
                .w_full()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().warning)
                        .child(copyable_text(
                            "account-inspection-error-text",
                            error.clone(),
                        )),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(crate::i18n::text(
                            "Use Raw value to view or edit the original value.",
                        )),
                )
                .into_any_element(),
            Inspection::Certificate(certificates) => div()
                .id("account-ca-details")
                .test_support()
                .w_full()
                .child(self.render_certificate_bundle(
                    "CA certificate bundle",
                    certificates,
                    true,
                    cx,
                ))
                .into_any_element(),
            Inspection::Namespace(namespace) => {
                let cell = crate::overview::Cell::text(namespace.clone()).link("", "Namespace");
                v_flex()
                    .id("account-namespace-details")
                    .test_support()
                    .w_full()
                    .gap_3()
                    .p_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .child(self.field(0, "Token namespace", &cell, cx))
                    .when(
                        self.target.namespace.as_deref() != Some(namespace.as_str()),
                        |view| {
                            view.child(div().text_xs().text_color(cx.theme().warning).child(
                                crate::i18n::text(
                                    "This value differs from the Secret's namespace.",
                                ),
                            ))
                        },
                    )
                    .into_any_element()
            }
            Inspection::Token(Ok(token)) => {
                let timing = token.timing(self.now.as_second());
                let tone = match timing {
                    Timing::Expired | Timing::NotYetActive => Tone::Warning,
                    _ => Tone::Unknown,
                };
                v_flex().id("account-token-details").test_support().w_full().gap_4().p_3().rounded_lg()
                    .border_1().border_color(cx.theme().border)
                    .child(v_flex().gap_2()
                        .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(crate::i18n::text("JWT details")))
                        .child(div().id("account-token-timing").test_support().text_sm().text_color(cx.theme().tone(tone))
                            .child(SelectableText::new("timing", timing.label())))
                        .child(div().text_xs().text_color(cx.theme().muted_foreground)
                            .child(crate::i18n::text("Decoded locally · Signature and token validity are not verified."))))
                    .child(self.field_rows(&token.fields(), cx))
                    .child(self.disclosure("Decoded JWT header", "account-jwt-header",
                        self.jwt_json("account-jwt-header-json", &token.header, cx), false, cx))
                    .child(self.disclosure("Decoded JWT claims", "account-jwt-claims",
                        self.jwt_json("account-jwt-claims-json", &token.claims, cx), false, cx))
                    .into_any_element()
            }
        }
    }

    fn jwt_json(&self, id: &'static str, value: &Value, cx: &App) -> AnyElement {
        div()
            .w_full()
            .min_w_0()
            .p_2()
            .rounded_md()
            .bg(cx.theme().muted.opacity(0.5))
            .font_family(cx.theme().mono_font_family.clone())
            .text_xs()
            .child(copyable_text(
                id,
                serde_json::to_string_pretty(value).unwrap_or_default(),
            ))
            .into_any_element()
    }
}
