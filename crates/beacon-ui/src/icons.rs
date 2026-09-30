//! Sidebar icons bundled with the binary; no external files are needed.
use crate::catalog::{Category, Section};
use gpui_kit::component::{Icon, IconName};

pub(crate) fn kubernetes() -> Icon {
    Icon::default().data(include_bytes!("../assets/kubernetes.svg"))
}

pub(crate) fn section(grouping: &Section) -> Icon {
    match grouping {
        Section::Builtin(Category::Cluster) => {
            Icon::default().data(include_bytes!("../assets/server.svg"))
        }
        Section::Builtin(Category::Workloads) => {
            Icon::default().data(include_bytes!("../assets/boxes.svg"))
        }
        Section::Builtin(Category::Config) => Icon::new(IconName::Settings),
        Section::Builtin(Category::Network) => Icon::new(IconName::Network),
        Section::Builtin(Category::Storage) => {
            Icon::default().data(include_bytes!("../assets/database.svg"))
        }
        Section::Builtin(Category::AccessControl) => {
            Icon::default().data(include_bytes!("../assets/shield.svg"))
        }
        Section::Builtin(Category::Other) => {
            Icon::default().data(include_bytes!("../assets/layers.svg"))
        }
        Section::Group(group) => match group.as_ref() {
            "authentication.k8s.io" | "authorization.k8s.io" => {
                section(&Section::Builtin(Category::AccessControl))
            }
            "flowcontrol.apiserver.k8s.io" => section(&Section::Builtin(Category::Network)),
            "metrics.k8s.io" => Icon::new(IconName::ChartPie),
            _ if grouping.is_custom_group() => custom(),
            _ => section(&Section::Builtin(Category::Other)),
        },
    }
}

pub(crate) fn custom() -> Icon {
    Icon::default().data(include_bytes!("../assets/puzzle.svg"))
}
pub(crate) fn tools() -> Icon {
    Icon::default().data(include_bytes!("../assets/wrench.svg"))
}
