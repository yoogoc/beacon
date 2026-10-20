//! Shared resource actions for the row menu and detail-header toolbar.
use std::rc::Rc;

use beacon_kube::Operation;
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    Icon,
    menu::{PopupMenu, PopupMenuItem},
};
use gpui_kit::*;

type Handler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

#[derive(Default)]
pub(crate) struct ResourceMenu {
    items: Vec<MenuEntry>,
}

struct MenuEntry {
    item: PopupMenuItem,
    destructive: bool,
    show_in_header: bool,
    group: Option<HeaderGroup>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderGroup {
    Copy,
    PortForward,
}

impl HeaderGroup {
    fn label(self) -> SharedString {
        crate::i18n::text(match self {
            Self::Copy => "Copy",
            Self::PortForward => "Port forward…",
        })
    }

    fn icon(self) -> Icon {
        Icon::new(match self {
            Self::Copy => IconName::Copy,
            Self::PortForward => IconName::RadioTower,
        })
    }
}

#[derive(Clone)]
pub(crate) enum HeaderItem {
    Separator,
    Action {
        label: SharedString,
        icon: Box<Icon>,
        disabled: bool,
        destructive: bool,
        handler: Handler,
    },
    Menu {
        group: HeaderGroup,
        label: SharedString,
        icon: Box<Icon>,
        items: Vec<HeaderItem>,
    },
}

impl ResourceMenu {
    pub fn label(self, label: impl Into<SharedString>) -> Self {
        self.push(PopupMenuItem::label(label), false, false)
    }

    pub fn item(self, item: PopupMenuItem) -> Self {
        self.push(item, false, true)
    }

    /// Keep the row menu explicit while grouping related toolbar actions.
    pub fn grouped_item(mut self, item: PopupMenuItem, group: HeaderGroup) -> Self {
        self.items.push(MenuEntry {
            item,
            destructive: false,
            show_in_header: true,
            group: Some(group),
        });
        self
    }

    pub fn popup_only_item(self, item: PopupMenuItem) -> Self {
        self.push(item, false, false)
    }

    fn push(mut self, item: PopupMenuItem, destructive: bool, show_in_header: bool) -> Self {
        self.items.push(MenuEntry {
            item,
            destructive,
            show_in_header,
            group: None,
        });
        self
    }

    pub fn operation(self, item: PopupMenuItem, operation: &Operation) -> Self {
        use beacon_kube::workflow_ops::Action;
        let (icon, destructive) = match operation {
            Operation::Delete => (IconName::Trash, true),
            Operation::Restart => (IconName::RefreshCw, false),
            Operation::Scale { .. } => (IconName::SlidersHorizontal, false),
            Operation::Apply => (IconName::FileCode, false),
            Operation::RunJob => (IconName::Play, false),
            Operation::CronJobSuspend { suspend } => (
                if *suspend {
                    IconName::Pause
                } else {
                    IconName::CalendarCheck
                },
                false,
            ),
            Operation::NodeCordon { unschedulable } => (
                if *unschedulable {
                    IconName::Lock
                } else {
                    IconName::LockOpen
                },
                false,
            ),
            Operation::NodeDrain => (IconName::Unplug, false),
            Operation::Workflow { action } => match action {
                Action::Suspend => (IconName::Pause, false),
                Action::Stop => (IconName::CircleX, true),
                Action::Terminate => (IconName::X, true),
                Action::Retry => (IconName::RotateCcw, false),
                Action::Resubmit => (IconName::Redo2, false),
                Action::Submit => (IconName::Send, false),
                Action::Resume => (IconName::Play, false),
            },
        };
        self.push(item.icon(icon), destructive, true)
    }

    pub fn separator(self) -> Self {
        self.push(PopupMenuItem::Separator, false, true)
    }

    pub fn popup(self, menu: PopupMenu) -> PopupMenu {
        self.items
            .into_iter()
            .fold(menu, |menu, entry| menu.item(entry.item))
    }

    pub fn header(self) -> Vec<HeaderItem> {
        let mut header = Vec::new();
        for entry in self.items.into_iter().filter(|entry| entry.show_in_header) {
            let item = match entry.item {
                PopupMenuItem::Separator => Some(HeaderItem::Separator),
                PopupMenuItem::Item {
                    label,
                    icon,
                    disabled,
                    handler,
                    action,
                    ..
                } => {
                    let handler = handler.or_else(|| {
                        action.map(|action| {
                            Rc::new(move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                window.dispatch_action(action.boxed_clone(), cx)
                            }) as Handler
                        })
                    });
                    handler.map(|handler| HeaderItem::Action {
                        label,
                        icon: Box::new(icon.unwrap_or_else(|| Icon::new(IconName::Info))),
                        disabled,
                        destructive: entry.destructive,
                        handler,
                    })
                }
                // The identity is already displayed by the detail header.
                _ => None,
            };
            let Some(item) = item else { continue };
            if let Some(group) = entry.group {
                if let Some(HeaderItem::Menu { items, .. }) = header.iter_mut().find(|item| {
                    matches!(item, HeaderItem::Menu { group: existing, .. } if *existing == group)
                }) {
                    items.push(item);
                } else {
                    header.push(HeaderItem::Menu {
                        group,
                        label: group.label(),
                        icon: Box::new(group.icon()),
                        items: vec![item],
                    });
                }
            } else {
                header.push(item);
            }
        }
        header
    }
}
