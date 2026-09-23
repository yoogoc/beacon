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
}

impl Ask {
    fn title(&self) -> String {
        match self.operation {
            Operation::Delete => format!("Delete this {}?", self.kind),
            Operation::Scale { .. } => format!("Scale this {}", self.kind),
            _ => self.operation.describe(),
        }
    }

    fn body(&self) -> String {
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
            .w(px(460.))
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
