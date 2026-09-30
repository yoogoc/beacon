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
            bulk_targets: Some(targets),
        }
    }

    fn title(&self) -> String {
        if let Some(targets) = &self.bulk_targets {
            return format!("Delete {} {} resources?", targets.len(), self.kind);
        }
        match self.operation {
            Operation::Delete => format!("Delete this {}?", self.kind),
            Operation::Scale { .. } => format!("Scale this {}", self.kind),
            _ => self.operation.describe(),
        }
    }

    fn body(&self) -> String {
        if self.bulk_targets.is_some() {
            return "The resources listed below will be deleted. There is no undo. Controller-owned resources may be recreated.".into();
        }
        match self.operation {
            Operation::Delete => format!(
                "{} will be deleted. There is no undo — if a controller owns it, \
                 it will be recreated; if not, it is gone.",
                self.target
            ),
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
}

impl EventEmitter<PromptEvent> for Prompt {}

pub struct Prompt {
    ask: Ask,
    replicas: Entity<InputState>,
}

impl Prompt {
    pub fn new(ask: Ask, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let start = match ask.operation {
            Operation::Scale { replicas } => replicas.max(0).to_string(),
            _ => String::new(),
        };

        let replicas = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Replicas")
                .default_value(start)
        });
        if ask.needs_a_number() {
            replicas.read(cx).focus_handle(cx).focus(window, cx);
        }

        Self { ask, replicas }
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

    fn render_targets(&self, cx: &App) -> Option<AnyElement> {
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
            .child(div().w(px(150.)).child("Namespace"))
            .child(div().flex_1().child("Name"));

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
                        .overflow_y_scroll()
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
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.ask.body()),
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
                    .child(
                        Button::new("cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
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
