//! The question asked before something is changed.
//!
//! Two shapes, because two things need asking. A delete needs confirming —
//! it is the one action in Beacon with no undo, and the object's name is
//! repeated back so that the wrong row cannot be deleted by reflex. A scale
//! needs a number, opened on the count that is currently set.
//!
//! Restarting asks nothing. It is disruptive but not destructive, it is what
//! the button says, and a confirmation for everything is a confirmation for
//! nothing.

use crate::scroll::OverflowScrollbars as _;
use beacon_kube::{ObjectRef, Operation};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// What is being asked, and about what.
pub struct Ask {
    pub operation: Operation,
    pub target: ObjectRef,
    pub kind: SharedString,
    pub bulk_targets: Option<Vec<ObjectRef>>,
}

impl Ask {
    pub fn bulk_delete(kind: SharedString, targets: Vec<ObjectRef>) -> Self {
        assert!(!targets.is_empty());
        Self {
            operation: Operation::Delete,
            target: targets[0].clone(),
            kind,
            bulk_targets: (targets.len() > 1).then_some(targets),
        }
    }

    fn title(&self) -> String {
        if let Some(targets) = &self.bulk_targets {
            return crate::i18n::message(
                "resources.delete_many",
                &[
                    ("count", targets.len().to_string()),
                    ("kind", self.kind.to_string()),
                ],
            )
            .to_string();
        }
        match self.operation {
            Operation::Delete => {
                crate::i18n::message("resources.delete_one", &[("kind", self.kind.to_string())])
                    .to_string()
            }
            Operation::Scale { .. } => {
                crate::i18n::message("resources.scale", &[("kind", self.kind.to_string())])
                    .to_string()
            }
            _ => self.operation.describe(),
        }
    }

    fn body(&self) -> String {
        if self.bulk_targets.is_some() {
            return "The resources listed below will be deleted. There is no undo. Controller-owned resources may be recreated.".into();
        }
        match self.operation {
            Operation::Delete => "Cannot be undone.".into(),
            _ => self.target.to_string(),
        }
    }

    fn needs_a_number(&self) -> bool {
        matches!(self.operation, Operation::Scale { .. })
    }

    fn is_destructive(&self) -> bool {
        matches!(self.operation, Operation::Delete)
    }
}

pub enum PromptEvent {
    /// Go ahead, with the operation as the user finally set it.
    Confirmed(Operation),
    Cancelled,
    UsageRequested(ObjectRef),
}

impl EventEmitter<PromptEvent> for Prompt {}

pub struct Prompt {
    ask: Ask,
    replicas: Entity<InputState>,
    usage: bool,
}

impl Prompt {
    pub fn new(ask: Ask, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = match ask.operation {
            Operation::Scale { replicas } => replicas.max(0).to_string(),
            _ => String::new(),
        };

        let replicas = cx.new(|cx| crate::i18n::input("Replicas", window, cx).default_value(start));
        if ask.needs_a_number() {
            replicas.read(cx).focus_handle(cx).focus(window, cx);
        }

        Self {
            ask,
            replicas,
            usage: false,
        }
    }

    pub(crate) fn with_usage(mut self, enabled: bool) -> Self {
        self.usage = enabled;
        self
    }

    /// The operation with whatever the user typed folded in.
    ///
    /// A number that does not parse keeps the prompt open rather than
    /// scaling to something nobody asked for.
    fn resolve(&self, cx: &App) -> Option<Operation> {
        match &self.ask.operation {
            Operation::Scale { .. } => {
                let typed = self.replicas.read(cx).value();
                let replicas: i32 = typed.trim().parse().ok()?;
                (replicas >= 0).then_some(Operation::Scale { replicas })
            }
            other => Some(other.clone()),
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(operation) = self.resolve(cx) {
            cx.emit(PromptEvent::Confirmed(operation));
        }
    }

    fn render_single_target(&self, cx: &App) -> Option<AnyElement> {
        if !self.ask.is_destructive() || self.ask.bulk_targets.is_some() {
            return None;
        }
        let fields = [
            match self.ask.target.namespace.as_deref() {
                Some(namespace) => ("Namespace", namespace, "delete-namespace"),
                None => ("Scope", "Cluster-scoped", "delete-scope"),
            },
            ("Name", self.ask.target.name.as_str(), "delete-name"),
        ];
        Some(
            v_flex()
                .w_full()
                .p_3()
                .gap_2()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().danger.opacity(0.2))
                .bg(cx.theme().danger.opacity(0.06))
                .children(fields.into_iter().map(|(label, value, id)| {
                    h_flex()
                        .w_full()
                        .items_start()
                        .gap_3()
                        .child(
                            div()
                                .w(px(88.))
                                .flex_shrink_0()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(label),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().danger)
                                .child(crate::copyable_text::copyable_text(id, value.to_owned())),
                        )
                }))
                .into_any_element(),
        )
    }

    fn render_targets(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let targets = self.ask.bulk_targets.as_ref()?;
        let border = cx.theme().border;
        let muted = cx.theme().muted_foreground;
        let header = h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_2()
            .border_b_1()
            .border_color(border)
            .bg(cx.theme().muted.opacity(0.35))
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted)
            .child(div().w(px(32.)).child("#"))
            .child(div().w(px(150.)).child(crate::i18n::text("Namespace")))
            .child(div().flex_1().child(crate::i18n::text("Name")));

        let rows = targets.iter().enumerate().map(|(index, target)| {
            h_flex()
                .w_full()
                .px_3()
                .py_2()
                .gap_2()
                .items_start()
                .text_sm()
                .when(index + 1 < targets.len(), |row| {
                    row.border_b_1().border_color(border)
                })
                .child(
                    div()
                        .w(px(32.))
                        .text_color(muted)
                        .child((index + 1).to_string()),
                )
                .child(
                    div().w(px(150.)).child(
                        target
                            .namespace
                            .as_deref()
                            .unwrap_or("Cluster-scoped")
                            .to_string(),
                    ),
                )
                .child(div().flex_1().min_w_0().child(target.name.clone()))
                .when(self.usage, |row| {
                    let target = target.clone();
                    row.child(
                        Button::new(("delete-view-usage", index))
                            .small()
                            .ghost()
                            .label(crate::i18n::text("View usage"))
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PromptEvent::UsageRequested(target.clone()))
                            })),
                    )
                })
        });

        Some(
            v_flex()
                .w_full()
                .rounded_md()
                .border_1()
                .border_color(border)
                .overflow_hidden()
                .child(header)
                .child(
                    div()
                        .id("delete-target-table")
                        .w_full()
                        .max_h(px(280.))
                        .overflow_y_scrollbar()
                        .child(v_flex().children(rows)),
                )
                .into_any_element(),
        )
    }
}

impl Render for Prompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let confirm = match self.ask.operation {
            Operation::Delete => "Delete",
            Operation::Scale { .. } => "Scale",
            _ => "Confirm",
        };
        let ready = self.resolve(cx).is_some();

        v_flex()
            .w(px(if self.ask.bulk_targets.is_some() {
                600.
            } else {
                460.
            }))
            .p_4()
            .gap_3()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.ask.title()),
            )
            .children(self.render_single_target(cx))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(crate::i18n::text(self.ask.body())),
            )
            .children(self.render_targets(cx))
            .children(
                self.ask
                    .needs_a_number()
                    .then(|| Input::new(&self.replicas).small()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .when(self.usage && self.ask.bulk_targets.is_none(), |row| {
                        let target = self.ask.target.clone();
                        row.child(
                            Button::new("delete-view-usage")
                                .small()
                                .ghost()
                                .label(crate::i18n::text("View usage"))
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(PromptEvent::UsageRequested(target.clone()))
                                })),
                        )
                    })
                    .child(
                        Button::new("cancel")
                            .ghost()
                            .small()
                            .label(crate::i18n::text("Cancel"))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(PromptEvent::Cancelled);
                            })),
                    )
                    .child(
                        Button::new("confirm")
                            .small()
                            .when(self.ask.is_destructive(), |button| button.danger())
                            .when(!self.ask.is_destructive(), |button| button.primary())
                            .label(confirm)
                            .disabled(!ready)
                            .on_click(cx.listener(|view, _, _, cx| view.confirm(cx))),
                    ),
            )
    }
}
