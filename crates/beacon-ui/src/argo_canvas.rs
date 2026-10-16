//! A bounded execution canvas. Graph preparation happens outside the UI thread;
//! pan/zoom redraws this view and only mounts cards intersecting its viewport.
mod model;
use crate::{
    argo::{Graph, GraphNode},
    copyable_text::copyable_text,
    theme::BeaconTheme as _,
};
use beacon_columns::Timestamp;
use beacon_kube::argo::Nodes;
use gpui_kit::{
    assets::IconName,
    base::{ElementExt as _, Link, TestSupportExt as _},
    component::{
        ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _,
        button::{Button, ButtonVariants as _},
        h_flex,
        input::{Input, InputEvent, InputState},
        spinner::Spinner,
        tooltip::Tooltip,
        v_flex,
    },
    prelude::FluentBuilder as _,
    *,
};
pub(crate) use model::Prepared;
use model::{Camera, Scene};
use std::{cell::Cell, collections::BTreeSet, rc::Rc, sync::Arc};

pub(crate) enum CanvasEvent {
    Inspect(GraphNode),
    Reveal(GraphNode),
}
struct Focus {
    label: String,
    ids: Vec<String>,
}
pub(crate) struct ArgoCanvas {
    prepared: Prepared,
    scene: Arc<Scene>,
    nodes: Arc<Nodes>,
    stack: Vec<Focus>,
    grouped: bool,
    pinned: Option<String>,
    camera: Camera,
    bounds: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Point<Pixels>>,
    search: Entity<InputState>,
    matches: Vec<GraphNode>,
    loading: bool,
    expected: Option<usize>,
    error: Option<String>,
    context: String,
    now: Timestamp,
    _search: Subscription,
}
impl EventEmitter<CanvasEvent> for ArgoCanvas {}
impl ArgoCanvas {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Find a node…"));
        let subscription = cx.subscribe_in(
            &search,
            window,
            |view: &mut Self, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    view.search_nodes(cx);
                    cx.notify();
                }
                if matches!(event, InputEvent::PressEnter { .. })
                    && let Some(node) = view.matches.first().cloned()
                {
                    view.choose_search(node, window, cx);
                }
            },
        );
        let prepared = Prepared::new(Graph::default());
        Self {
            scene: prepared.grouped.clone(),
            prepared,
            nodes: Arc::new(Nodes::new()),
            stack: vec![],
            grouped: true,
            pinned: None,
            camera: Camera::default(),
            bounds: Rc::new(Cell::new(Bounds::new(
                Point::default(),
                size(px(600.), px(420.)),
            ))),
            drag: None,
            search,
            matches: vec![],
            loading: false,
            expected: None,
            error: None,
            context: String::new(),
            now: Timestamp::now(),
            _search: subscription,
        }
    }
    pub fn loading(&mut self, count: Option<usize>, context: String, cx: &mut Context<Self>) {
        if self.context != context {
            self.context = context;
            self.stack.clear();
            self.pinned = None;
            self.camera = Camera::default();
            self.prepared = Prepared::new(Graph::default());
            self.scene = self.prepared.grouped.clone();
        }
        self.loading = true;
        self.expected = count;
        self.error = None;
        cx.notify();
    }
    pub fn fail(&mut self, error: String, cx: &mut Context<Self>) {
        self.loading = false;
        self.error = Some(error);
        self.drag = None;
        cx.notify();
    }
    pub fn replace(&mut self, prepared: Prepared, nodes: Arc<Nodes>, cx: &mut Context<Self>) {
        self.prepared = prepared;
        self.nodes = nodes;
        self.loading = false;
        self.error = None;
        self.drag = None;
        self.rebuild();
        self.search_nodes(cx);
        cx.notify();
    }
    pub fn tick(&mut self, now: Timestamp, cx: &mut Context<Self>) {
        self.now = now;
        if self
            .prepared
            .source
            .nodes
            .iter()
            .any(|n| n.data["phase"] == "Running")
        {
            cx.notify();
        }
    }
    fn rebuild(&mut self) {
        let source = &self.prepared.source;
        let subset = self.stack.last().map(|focus| {
            let ids: BTreeSet<_> = focus.ids.iter().collect();
            source
                .nodes
                .iter()
                .enumerate()
                .filter(|(_, node)| ids.contains(&node.id))
                .map(|(i, _)| i)
                .collect::<Vec<_>>()
        });
        if subset.as_ref().is_some_and(Vec::is_empty) {
            self.stack.clear();
            self.camera = Camera::default();
        }
        let subset = if self.stack.is_empty() { None } else { subset };
        let pinned = self
            .pinned
            .as_ref()
            .and_then(|id| source.nodes.iter().position(|n| &n.id == id));
        self.scene = if subset.is_none() && self.grouped && pinned.is_none() {
            self.prepared.grouped.clone()
        } else {
            Arc::new(Scene::new(source, subset.as_deref(), self.grouped, pinned))
        };
    }
    fn search_nodes(&mut self, cx: &App) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.matches.clear();
        if query.is_empty() {
            return;
        }
        if self.nodes.is_empty() {
            self.matches = self
                .prepared
                .source
                .nodes
                .iter()
                .filter(|node| {
                    node.label.to_lowercase().contains(&query)
                        || node.id.to_lowercase().contains(&query)
                })
                .take(8)
                .cloned()
                .collect();
        } else {
            for (id, data) in self.nodes.iter() {
                let label = data["displayName"]
                    .as_str()
                    .or_else(|| data["name"].as_str())
                    .unwrap_or(id);
                if id.to_lowercase().contains(&query) || label.to_lowercase().contains(&query) {
                    let mut data = data.clone();
                    data["id"] = id.clone().into();
                    self.matches.push(GraphNode {
                        id: id.clone(),
                        label: label.into(),
                        data,
                        runtime: true,
                        template_context: None,
                        x: 0.,
                        y: 0.,
                    });
                    if self.matches.len() == 8 {
                        break;
                    }
                }
            }
        }
    }
    pub fn reveal(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.prepared.source.nodes.iter().position(|n| n.id == id) else {
            return false;
        };
        self.stack.clear();
        self.pinned = Some(id.into());
        self.rebuild();
        let display = self
            .scene
            .members
            .iter()
            .position(|m| m.contains(&index))
            .unwrap();
        let bounds = self.scene.node_bounds(display);
        self.camera = Camera {
            automatic: false,
            scale: 1.,
            center: bounds.center(),
        };
        cx.notify();
        true
    }
    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let members = &self.scene.members[index];
        if members.len() == 1 {
            let node = self.prepared.source.nodes[members[0]].clone();
            self.pinned = Some(node.id.clone());
            cx.emit(CanvasEvent::Inspect(node));
        } else {
            self.stack.push(Focus {
                label: self.scene.graph.nodes[index].label.clone(),
                ids: members
                    .iter()
                    .map(|i| self.prepared.source.nodes[*i].id.clone())
                    .collect(),
            });
            self.pinned = None;
            self.camera = Camera::default();
            self.rebuild();
        }
        cx.notify();
    }
    fn choose_search(&mut self, node: GraphNode, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.matches.clear();
        if self.reveal(&node.id, cx) {
            cx.emit(CanvasEvent::Inspect(node));
        } else {
            cx.emit(CanvasEvent::Reveal(node));
        }
    }
    fn resolved(&self) -> Camera {
        self.camera.resolved(&self.scene, self.bounds.get().size)
    }
    fn zoom(&mut self, multiplier: f32, anchor: Option<Point<Pixels>>, cx: &mut Context<Self>) {
        let size = self.bounds.get().size;
        self.camera = self.resolved();
        self.camera.zoom(
            multiplier,
            anchor.unwrap_or(point(size.width / 2., (size.height - px(48.)) / 2.)),
            size,
        );
        cx.notify();
    }
    fn subtitle(&self, index: usize) -> String {
        let members = &self.scene.members[index];
        let node = &self.scene.graph.nodes[index];
        let phase = node.data["phase"].as_str().unwrap_or(if node.runtime {
            "Pending"
        } else {
            "Template"
        });
        if members.len() > 1 {
            let failed = members
                .iter()
                .filter(|i| {
                    matches!(
                        self.prepared.source.nodes[**i].data["phase"].as_str(),
                        Some("Failed" | "Error")
                    )
                })
                .count();
            let running = members
                .iter()
                .filter(|i| self.prepared.source.nodes[**i].data["phase"] == "Running")
                .count();
            if failed > 0 && running > 0 {
                format!("{failed} failed · {running} running")
            } else if failed > 0 {
                format!("{} nodes · {failed} failed", members.len())
            } else if running > 0 {
                format!("{} nodes · {running} running", members.len())
            } else if phase == "Succeeded" {
                format!("{} nodes", members.len())
            } else {
                format!("{} nodes · {phase}", members.len())
            }
        } else if node.runtime {
            let duration =
                crate::argo::duration(&node.data, self.now).unwrap_or_else(|| "—".into());
            if phase == "Succeeded" {
                duration
            } else {
                format!("{phase} · {duration}")
            }
        } else {
            node.data["template"].as_str().unwrap_or("inline").into()
        }
    }
    fn drag_listener(&self, cx: &Context<Self>) -> AnyElement {
        let owner = cx.weak_entity();
        canvas(
            |_, _, _| (),
            move |_, _, window, _| {
                let moving = owner.clone();
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                    if !phase.capture() {
                        return;
                    }
                    let _ = moving.update(cx, |view, cx| {
                        let Some(previous) = view.drag else {
                            return;
                        };
                        if event.pressed_button != Some(MouseButton::Left) {
                            view.drag = None;
                            cx.notify();
                            return;
                        }
                        view.camera = view.resolved();
                        view.camera.pan(event.position - previous, &view.scene);
                        view.drag = Some(event.position);
                        cx.notify();
                    });
                });
                let owner = owner.clone();
                window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                    if phase.capture() && event.button == MouseButton::Left {
                        let _ = owner.update(cx, |view, cx| {
                            if view.drag.take().is_some() {
                                cx.notify();
                            }
                        });
                    }
                });
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
    fn background(&self, cx: &App) -> AnyElement {
        let scene = self.scene.clone();
        let camera = self.resolved();
        let color = cx.theme().border;
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let mut y = px(14.);
                while y < bounds.size.height {
                    let mut x = px(14.);
                    while x < bounds.size.width {
                        window.paint_quad(fill(
                            Bounds::new(bounds.origin + point(x, y), size(px(1.), px(1.))),
                            color.opacity(0.65),
                        ));
                        x += px(18.);
                    }
                    y += px(18.);
                }
                let mut path = PathBuilder::stroke(px(1.25));
                for &(a, b) in &scene.graph.edges {
                    let a = camera.bounds(scene.node_bounds(a), bounds.size);
                    let b = camera.bounds(scene.node_bounds(b), bounds.size);
                    if a.right() < px(0.) && b.right() < px(0.)
                        || a.left() > bounds.size.width && b.left() > bounds.size.width
                        || a.bottom() < px(0.) && b.bottom() < px(0.)
                        || a.top() > bounds.size.height && b.top() > bounds.size.height
                    {
                        continue;
                    }
                    let start = bounds.origin + point(a.center().x, a.bottom());
                    let end = bounds.origin + point(b.center().x, b.top());
                    let mid = (start.y + end.y) / 2.;
                    path.move_to(start);
                    path.line_to(point(start.x, mid));
                    path.line_to(point(end.x, mid));
                    path.line_to(end);
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, color);
                }
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
    fn minimap(&self, cx: &mut Context<Self>) -> AnyElement {
        let scene = self.scene.clone();
        let camera = self.resolved();
        let viewport = self.bounds.get().size;
        let border = cx.theme().border;
        let active = cx.theme().ring;
        let map_bounds = Rc::new(Cell::new(Bounds::default()));
        let measured = map_bounds.clone();
        div()
            .id("argo-minimap")
            .test_support()
            .absolute()
            .right(px(12.))
            .bottom(px(12.))
            .w(px(96.))
            .h(px(64.))
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(cx.theme().background)
            .overflow_hidden()
            .occlude()
            .cursor_pointer()
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds),
                    move |bounds, _, window, _| {
                        let content = scene.size();
                        let ratio = ((bounds.size.width - px(8.)) / content.width)
                            .min((bounds.size.height - px(8.)) / content.height);
                        let origin =
                            bounds.center() - point(content.width, content.height) * ratio / 2.;
                        for i in 0..scene.graph.nodes.len() {
                            let logical = scene.node_bounds(i);
                            window.paint_quad(
                                fill(
                                    Bounds::new(
                                        origin + logical.origin * ratio,
                                        size(
                                            logical.size.width * ratio,
                                            logical.size.height * ratio,
                                        ),
                                    ),
                                    border,
                                )
                                .corner_radii(px(1.)),
                            );
                        }
                        let logical = Bounds::new(
                            (Point::default() - camera.origin(viewport)) / camera.scale,
                            size(viewport.width, viewport.height - px(48.)) / camera.scale,
                        )
                        .intersect(&Bounds::new(Point::default(), content));
                        if logical.size.width > px(0.) && logical.size.height > px(0.) {
                            window.paint_quad(
                                outline(
                                    Bounds::new(
                                        origin + logical.origin * ratio,
                                        size(
                                            logical.size.width * ratio,
                                            logical.size.height * ratio,
                                        ),
                                    ),
                                    active,
                                    BorderStyle::default(),
                                )
                                .corner_radii(px(2.)),
                            );
                        }
                    },
                )
                .size_full(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |view, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    let bounds = map_bounds.get();
                    let content = view.scene.size();
                    let ratio = ((bounds.size.width - px(8.)) / content.width)
                        .min((bounds.size.height - px(8.)) / content.height);
                    let origin =
                        bounds.center() - point(content.width, content.height) * ratio / 2.;
                    view.camera = view.resolved();
                    view.camera.automatic = false;
                    view.camera.center = (event.position - origin) / ratio;
                    view.camera.pan(Point::default(), &view.scene);
                    cx.notify();
                }),
            )
            .into_any_element()
    }
    fn loading_view(&self, cx: &App) -> AnyElement {
        div()
            .id("argo-graph-loading")
            .test_support()
            .size_full()
            .relative()
            .child(
                h_flex()
                    .size_full()
                    .flex_wrap()
                    .gap_6()
                    .p_6()
                    .items_center()
                    .justify_center()
                    .children((0..6).map(|_| {
                        v_flex()
                            .w(px(140.))
                            .h(px(52.))
                            .p_3()
                            .gap_2()
                            .rounded_md()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .child(div().w(px(85.)).h(px(6.)).rounded_sm().bg(cx.theme().muted))
                            .child(div().w(px(50.)).h(px(5.)).rounded_sm().bg(cx.theme().muted))
                    })),
            )
            .child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_center()
                            .p_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .shadow_md()
                            .child(Spinner::new().small())
                            .child(
                                v_flex().gap_1().child("Preparing execution graph").child(
                                    div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(
                                            self.expected
                                                .map(|n| format!("Reading {n} nodes…"))
                                                .unwrap_or_else(|| "Reading node status…".into()),
                                        ),
                                ),
                            ),
                    ),
            )
            .into_any_element()
    }
}

impl Render for ArgoCanvas {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let size = self.bounds.get().size;
        if self.scene.graph.edges.is_empty() && self.scene.graph.nodes.len() <= 12 {
            Arc::make_mut(&mut self.scene).arrange_independent(size.width);
        }
        let camera = self.resolved();
        let empty = self.scene.graph.nodes.is_empty();
        let ready = !empty && self.error.is_none();
        let mut graph = div()
            .id("argo-execution-graph")
            .test_support()
            .relative()
            .w_full()
            .h(px(420.))
            .overflow_hidden()
            .bg(cx.theme().muted.opacity(0.18))
            .cursor(if !ready {
                CursorStyle::Arrow
            } else if self.drag.is_some() {
                CursorStyle::ClosedHand
            } else {
                CursorStyle::OpenHand
            })
            .on_scroll_wheel(cx.listener(|view, event: &ScrollWheelEvent, window, cx| {
                cx.stop_propagation();
                if view.loading && view.scene.graph.nodes.is_empty() || view.error.is_some() {
                    return;
                }
                if event.modifiers.control || event.modifiers.platform {
                    view.zoom(
                        (-f32::from(event.delta.pixel_delta(window.line_height()).y) * 0.008).exp(),
                        Some(event.position - view.bounds.get().origin),
                        cx,
                    );
                } else {
                    view.camera = view.resolved();
                    view.camera
                        .pan(event.delta.pixel_delta(window.line_height()), &view.scene);
                    cx.notify();
                }
            }))
            .on_pinch(cx.listener(|view, event: &PinchEvent, _, cx| {
                cx.stop_propagation();
                view.zoom(
                    (1. + event.delta).max(0.1),
                    Some(event.position - view.bounds.get().origin),
                    cx,
                );
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|view, event: &MouseDownEvent, _, cx| {
                    let position = event.position - view.bounds.get().origin;
                    if position.y > view.bounds.get().size.height - px(60.)
                        || view.error.is_some()
                        || view.loading && view.scene.graph.nodes.is_empty()
                    {
                        return;
                    }
                    let camera = view.resolved();
                    if view.scene.graph.nodes.iter().enumerate().any(|(i, _)| {
                        camera
                            .bounds(view.scene.node_bounds(i), view.bounds.get().size)
                            .contains(&position)
                    }) {
                        return;
                    }
                    view.drag = Some(event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            );
        let measured = self.bounds.clone();
        let entity = cx.entity().entity_id();
        graph = graph.on_prepaint(move |bounds, _, cx| {
            let old = measured.replace(bounds);
            if old.size != bounds.size {
                cx.notify(entity);
            }
        });
        if let Some(error) = &self.error {
            graph = graph.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .p_5()
                    .child(copyable_text("argo-graph-error", error.clone())),
            );
        } else if empty && self.loading {
            graph = graph.child(self.loading_view(cx));
        } else if empty {
            graph = graph.child(
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .text_color(cx.theme().muted_foreground)
                    .gap_2()
                    .child(Icon::new(IconName::Workflow).size_5())
                    .child("No execution nodes yet"),
            );
        } else {
            graph = graph.child(self.background(cx));
            let origin = camera.origin(size);
            let mut surface = div()
                .id("argo-graph-surface")
                .test_support()
                .absolute()
                .left(origin.x)
                .top(origin.y)
                .w(self.scene.size().width * camera.scale)
                .h(self.scene.size().height * camera.scale);
            let clip = Bounds::new(Point::default(), size);
            for (index, node) in self.scene.graph.nodes.iter().enumerate() {
                let bounds = camera.bounds(self.scene.node_bounds(index), size);
                if !bounds.intersects(&clip) {
                    continue;
                }
                let phase = node.data["phase"].as_str().unwrap_or(if node.runtime {
                    "Pending"
                } else {
                    "Template"
                });
                let tone = crate::argo::phase_tone(phase);
                let subtitle = self.subtitle(index);
                let description = if phase == "Succeeded" {
                    format!("{phase} · {subtitle}")
                } else {
                    subtitle.clone()
                };
                let group = self.scene.members[index].len() > 1;
                let selected = self.pinned.as_ref().is_some_and(|id| {
                    self.scene.members[index]
                        .iter()
                        .any(|i| &self.prepared.source.nodes[*i].id == id)
                });
                let tooltip = if group {
                    format!(
                        "{} · {} nodes\n{description}",
                        node.label,
                        self.scene.members[index].len()
                    )
                } else {
                    format!("{}\n{description}", node.label)
                };
                let mut card = Link::new(SharedString::from(format!("argo-node-{}", node.id)))
                    .accessibility_label(format!("{}: {description}", node.label))
                    .absolute()
                    .left(px(node.x) * camera.scale)
                    .top(px(node.y) * camera.scale)
                    .w(bounds.size.width)
                    .h(bounds.size.height)
                    .rounded_md()
                    .border_1()
                    .border_color(if selected {
                        cx.theme().ring
                    } else {
                        cx.theme().border
                    })
                    .bg(cx.theme().background)
                    .hover(|style| style.bg(cx.theme().muted))
                    .cursor_pointer()
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
                    .on_activate(cx.listener(move |view, _, _, cx| view.choose(index, cx)));
                if camera.scale >= 0.5 {
                    let icon = if group {
                        IconName::Layers
                    } else {
                        match phase {
                            "Succeeded" => IconName::CircleCheck,
                            "Failed" | "Error" => IconName::CircleX,
                            "Running" => IconName::LoaderCircle,
                            _ => IconName::Circle,
                        }
                    };
                    card = card.child(
                        h_flex()
                            .size_full()
                            .px(px(10.) * camera.scale)
                            .gap(px(8.) * camera.scale)
                            .items_center()
                            .child(
                                Icon::new(icon)
                                    .size((px(16.) * camera.scale).max(px(11.)))
                                    .text_color(cx.theme().tone(tone)),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_size((px(13.) * camera.scale).max(px(11.)))
                                            .truncate()
                                            .child(node.label.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size((px(11.) * camera.scale).max(px(11.)))
                                            .truncate()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(subtitle),
                                    ),
                            )
                            .when(group, |row| {
                                row.child(
                                    Icon::new(IconName::ChevronRight)
                                        .size(px(12.))
                                        .text_color(cx.theme().muted_foreground),
                                )
                            }),
                    );
                } else {
                    card = card.bg(cx.theme().tone(tone).opacity(0.18));
                }
                surface = surface.child(card);
            }
            graph = graph.child(surface).child(self.drag_listener(cx));
            graph = graph.child(
                h_flex()
                    .id("argo-zoom-controls")
                    .absolute()
                    .left(px(12.))
                    .bottom(px(12.))
                    .gap_1()
                    .p_1()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .occlude()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("argo-zoom-out")
                            .small()
                            .ghost()
                            .icon(IconName::Minus)
                            .accessibility_label("Zoom out")
                            .on_click(cx.listener(|view, _, _, cx| view.zoom(1. / 1.2, None, cx))),
                    )
                    .child(div().id("argo-zoom-value").test_support().text_xs().child(
                        if camera.scale < 0.001 {
                            "<0.1%".into()
                        } else if camera.scale < 0.01 {
                            format!("{:.1}%", camera.scale * 100.)
                        } else {
                            format!("{:.0}%", camera.scale * 100.)
                        },
                    ))
                    .child(
                        Button::new("argo-zoom-in")
                            .small()
                            .ghost()
                            .icon(IconName::Plus)
                            .accessibility_label("Zoom in")
                            .on_click(cx.listener(|view, _, _, cx| view.zoom(1.2, None, cx))),
                    )
                    .child(
                        Button::new("argo-fit")
                            .small()
                            .ghost()
                            .icon(IconName::Maximize)
                            .label("Fit")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.camera = Camera::default();
                                cx.notify();
                            })),
                    ),
            );
            if size.width >= px(360.) {
                graph = graph.child(self.minimap(cx));
            }
        }
        let mut toolbar = h_flex()
            .p_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .disabled(!ready)
                        .prefix(Icon::new(IconName::Search).size(px(14.))),
                ),
            )
            .child(
                Button::new("argo-group-nodes")
                    .small()
                    .ghost()
                    .icon(IconName::Layers)
                    .selected(self.grouped)
                    .disabled(!ready)
                    .accessibility_label("Group nodes")
                    .tooltip("Group parallel tasks and long sequences")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.grouped = !view.grouped;
                        view.camera = Camera::default();
                        view.rebuild();
                        cx.notify();
                    })),
            )
            .child(
                Button::new("argo-fit-toolbar")
                    .small()
                    .ghost()
                    .icon(IconName::Maximize)
                    .disabled(!ready)
                    .accessibility_label("Fit graph")
                    .tooltip("Fit graph to viewport")
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.camera = Camera::default();
                        cx.notify();
                    })),
            );
        if self.loading && !empty {
            toolbar = toolbar.child(Spinner::new().small());
        }
        let mut view = v_flex()
            .id("argo-workbench")
            .test_support()
            .relative()
            .w_full()
            .min_w_0()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .child(toolbar);
        if !self.stack.is_empty() {
            view = view.child(
                h_flex()
                    .px_2()
                    .py_1()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("argo-group-back")
                            .small()
                            .ghost()
                            .icon(IconName::ChevronLeft)
                            .label("Back")
                            .on_click(cx.listener(|view, _, _, cx| {
                                view.stack.pop();
                                view.pinned = None;
                                view.camera = Camera::default();
                                view.rebuild();
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(
                                self.stack
                                    .iter()
                                    .map(|f| f.label.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" / "),
                            ),
                    ),
            );
        }
        view = view.child(graph).child(
            h_flex()
                .px_3()
                .py_2()
                .gap_2()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if self.error.is_some() {
                    "Graph unavailable".into()
                } else if self.loading && empty {
                    "Preparing graph…".into()
                } else {
                    format!(
                        "{} {} · {} nodes{}",
                        self.scene.graph.nodes.len(),
                        if self.scene.graph.nodes.len() == 1 {
                            "item"
                        } else {
                            "items"
                        },
                        if self.nodes.is_empty() {
                            self.prepared.source.nodes.len()
                        } else {
                            self.nodes.len()
                        },
                        if self.grouped { " · grouped" } else { "" }
                    )
                })
                .when(ready && size.width >= px(520.), |row| {
                    row.child(div().flex_1())
                        .child("Drag to pan · Click a node for details")
                }),
        );
        if !self.search.read(cx).value().trim().is_empty() && ready {
            let mut results = v_flex()
                .id("argo-search-results")
                .test_support()
                .absolute()
                .top(px(44.))
                .left(px(8.))
                .right(px(8.))
                .p_1()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().background)
                .shadow_md()
                .occlude()
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
            for (index, node) in self.matches.iter().enumerate() {
                let node = node.clone();
                results = results.child(
                    Button::new(("argo-search-result", index))
                        .small()
                        .ghost()
                        .w_full()
                        .justify_start()
                        .label(node.label.clone())
                        .tooltip(node.id.clone())
                        .on_click(cx.listener(move |view, _, window, cx| {
                            view.choose_search(node.clone(), window, cx)
                        })),
                );
            }
            if self.matches.is_empty() {
                results = results.child(div().p_3().text_xs().child("No matching nodes"));
            }
            view = view.child(deferred(results).with_priority(1));
        }
        view
    }
}

#[cfg(all(test, feature = "ui-tests"))]
mod tests;
