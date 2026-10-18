use crate::argo::{CARD_HEIGHT, CARD_WIDTH, Graph};
use gpui_kit::*;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

const MIN_SCALE: f32 = 0.000001;

pub(crate) struct Prepared {
    pub(super) source: Arc<Graph>,
    pub(super) grouped: Arc<Scene>,
}
impl Prepared {
    pub fn new(graph: Graph) -> Self {
        let source = Arc::new(graph);
        let grouped = Arc::new(Scene::new(&source, None, true, None));
        Self { source, grouped }
    }
}

#[derive(Clone)]
pub(super) struct Scene {
    pub graph: Graph,
    /// Each displayed item owns these indices in the original graph.
    pub members: Vec<Vec<usize>>,
}

fn template(node: &crate::argo::GraphNode) -> String {
    node.data["templateName"]
        .as_str()
        .or_else(|| node.data["template"].as_str())
        .or_else(|| {
            node.data
                .pointer("/templateRef/template")
                .and_then(serde_json::Value::as_str)
        })
        .unwrap_or("")
        .to_owned()
}

impl Scene {
    pub fn new(
        source: &Graph,
        subset: Option<&[usize]>,
        grouped: bool,
        pinned: Option<usize>,
    ) -> Self {
        let selected: BTreeSet<usize> = subset
            .map(|v| v.iter().copied().collect())
            .unwrap_or_else(|| (0..source.nodes.len()).collect());
        let mut incoming = vec![Vec::new(); source.nodes.len()];
        let mut outgoing = incoming.clone();
        for &(from, to) in &source.edges {
            if selected.contains(&from) && selected.contains(&to) {
                incoming[to].push(from);
                outgoing[from].push(to);
            }
        }
        let mut buckets = BTreeMap::new();
        let mut members = Vec::new();
        for &index in &selected {
            if !grouped || Some(index) == pinned {
                members.push(vec![index]);
            } else {
                let node = &source.nodes[index];
                buckets
                    .entry((
                        incoming[index].clone(),
                        outgoing[index].clone(),
                        template(node),
                        node.data["type"].as_str().unwrap_or("").to_owned(),
                        node.data["boundaryID"].as_str().unwrap_or("").to_owned(),
                    ))
                    .or_insert_with(Vec::new)
                    .push(index);
            }
        }
        for bucket in buckets.into_values() {
            let chunk = if bucket.len() < 6 {
                1
            } else if subset.is_some() {
                bucket.len().div_ceil(6)
            } else {
                bucket.len()
            };
            members.extend(bucket.chunks(chunk).map(<[usize]>::to_vec));
        }
        // Contract only uninterrupted chains: a fork or join remains visible.
        // Cycles have no entry point and are deliberately left inspectable.
        if grouped {
            let singles: BTreeSet<usize> = members
                .iter()
                .filter(|m| m.len() == 1)
                .map(|m| m[0])
                .filter(|i| Some(*i) != pinned)
                .collect();
            let mut folded = BTreeSet::new();
            let mut chains = Vec::new();
            for &start in &singles {
                if incoming[start].len() > 1 || outgoing[start].len() > 1 {
                    continue;
                }
                if incoming[start].first().is_some_and(|p| {
                    singles.contains(p) && incoming[*p].len() <= 1 && outgoing[*p].len() == 1
                }) {
                    continue;
                }
                let mut current = start;
                let mut chain = Vec::new();
                let mut seen = BTreeSet::new();
                loop {
                    if !singles.contains(&current)
                        || incoming[current].len() > 1
                        || outgoing[current].len() > 1
                        || !seen.insert(current)
                    {
                        break;
                    }
                    chain.push(current);
                    let Some(&next) = outgoing[current].first() else {
                        break;
                    };
                    current = next;
                }
                if chain.len() >= 8 {
                    folded.extend(chain.iter().copied());
                    chains.extend(chain.chunks(chain.len().div_ceil(4)).map(<[usize]>::to_vec));
                }
            }
            members.retain(|m| m.len() != 1 || !folded.contains(&m[0]));
            members.extend(chains);
        }
        members.sort_by_key(|m| m[0]);
        let mut owner = vec![None; source.nodes.len()];
        let nodes = members
            .iter()
            .enumerate()
            .map(|(index, group)| {
                for &member in group {
                    owner[member] = Some(index);
                }
                let mut node = source.nodes[group[0]].clone();
                if group.len() > 1 {
                    let common = template(&node);
                    let sequential = group
                        .windows(2)
                        .all(|pair| outgoing[pair[0]].contains(&pair[1]));
                    node.label = if !common.is_empty()
                        && group.iter().all(|i| template(&source.nodes[*i]) == common)
                    {
                        common
                    } else if sequential {
                        "Sequential steps".into()
                    } else {
                        "Parallel tasks".into()
                    };
                    if subset.is_some() {
                        node.label = crate::i18n::message(
                            "messages.group",
                            &[("value0", (index + 1).to_string())],
                        )
                        .to_string();
                    }
                    node.data["phase"] = phase(source, group).into();
                }
                node
            })
            .collect();
        let edges = source
            .edges
            .iter()
            .filter_map(|(a, b)| Some((owner[*a]?, owner[*b]?)))
            .collect();
        Self {
            graph: Graph::layout(nodes, edges),
            members,
        }
    }

    pub fn node_bounds(&self, index: usize) -> Bounds<Pixels> {
        let node = &self.graph.nodes[index];
        Bounds::new(
            point(px(node.x), px(node.y)),
            size(px(CARD_WIDTH), px(CARD_HEIGHT)),
        )
    }
    pub fn size(&self) -> Size<Pixels> {
        size(px(self.graph.width.max(1.)), px(self.graph.height.max(1.)))
    }

    /// Independent items can wrap without changing a dependency's direction.
    /// Keep this bounded to small drill-downs, never rebuilding a large raw graph
    /// on the foreground thread when its viewport changes.
    pub fn arrange_independent(&mut self, width: Pixels) {
        let count = self.graph.nodes.len();
        if count == 0 || count > 12 || !self.graph.edges.is_empty() {
            return;
        }
        let columns =
            (((f32::from(width) - 48.) / (CARD_WIDTH + 20.)).floor() as usize).clamp(1, count);
        let rows = count.div_ceil(columns);
        self.graph.width = 48. + columns as f32 * (CARD_WIDTH + 20.) - 20.;
        self.graph.height = 24. + rows as f32 * (CARD_HEIGHT + 28.) - 28.;
        for (index, node) in self.graph.nodes.iter_mut().enumerate() {
            let row = index / columns;
            let in_row = (count - row * columns).min(columns);
            node.x = 24.
                + (columns - in_row) as f32 * (CARD_WIDTH + 20.) / 2.
                + (index % columns) as f32 * (CARD_WIDTH + 20.);
            node.y = 12. + row as f32 * (CARD_HEIGHT + 28.);
        }
    }
}

fn phase<'a>(source: &'a Graph, members: &[usize]) -> &'a str {
    let phases: BTreeSet<&str> = members
        .iter()
        .map(|i| {
            source.nodes[*i].data["phase"]
                .as_str()
                .unwrap_or(if source.nodes[*i].runtime {
                    "Pending"
                } else {
                    "Template"
                })
        })
        .collect();
    for phase in [
        "Error",
        "Failed",
        "Running",
        "Pending",
        "Suspended",
        "Unknown",
    ] {
        if phases.contains(phase) {
            return phase;
        }
    }
    if phases.len() == 1 {
        phases.into_iter().next().unwrap_or("Unknown")
    } else if phases
        .iter()
        .all(|phase| matches!(*phase, "Succeeded" | "Skipped" | "Omitted"))
    {
        "Succeeded"
    } else {
        "Unknown"
    }
}

#[derive(Clone, Copy)]
pub(super) struct Camera {
    pub automatic: bool,
    pub scale: f32,
    pub center: Point<Pixels>,
}
impl Default for Camera {
    fn default() -> Self {
        Self {
            automatic: true,
            scale: 1.,
            center: Point::default(),
        }
    }
}
impl Camera {
    pub fn resolved(mut self, scene: &Scene, viewport: Size<Pixels>) -> Self {
        if self.automatic {
            let content = scene.size();
            self.scale = ((viewport.width - px(40.)).max(px(1.)) / content.width)
                .min((viewport.height - px(80.)).max(px(1.)) / content.height)
                .clamp(MIN_SCALE, 1.);
            self.center = point(content.width / 2., content.height / 2.);
        }
        self
    }
    pub fn origin(self, viewport: Size<Pixels>) -> Point<Pixels> {
        point(viewport.width / 2., (viewport.height - px(48.)) / 2.) - self.center * self.scale
    }
    pub fn bounds(self, logical: Bounds<Pixels>, viewport: Size<Pixels>) -> Bounds<Pixels> {
        Bounds::new(
            self.origin(viewport) + logical.origin * self.scale,
            size(
                logical.size.width * self.scale,
                logical.size.height * self.scale,
            ),
        )
    }
    pub fn pan(&mut self, delta: Point<Pixels>, scene: &Scene) {
        self.automatic = false;
        self.center -= delta / self.scale;
        self.center.x = self.center.x.clamp(px(0.), scene.size().width);
        self.center.y = self.center.y.clamp(px(0.), scene.size().height);
    }
    pub fn zoom(&mut self, multiplier: f32, anchor: Point<Pixels>, viewport: Size<Pixels>) {
        let logical = (anchor - self.origin(viewport)) / self.scale;
        self.automatic = false;
        self.scale = (self.scale * multiplier).clamp(MIN_SCALE, 2.);
        let center = point(viewport.width / 2., (viewport.height - px(48.)) / 2.);
        self.center = logical - (anchor - center) / self.scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argo::GraphNode;
    use serde_json::json;
    fn graph(count: usize, edges: Vec<(usize, usize)>) -> Graph {
        Graph::layout(
            (0..count)
                .map(|i| GraphNode {
                    id: i.to_string(),
                    label: format!("task-{i}"),
                    data: json!({"phase":"Succeeded","type":"Pod","templateName":"worker"}),
                    runtime: true,
                    template_context: None,
                    x: 0.,
                    y: 0.,
                })
                .collect(),
            edges,
        )
    }
    #[::core::prelude::v1::test]
    fn parallel_groups_preserve_dependencies_and_failures_and_reveal_a_searched_node() {
        let mut raw = graph(1202, (1..1201).flat_map(|i| [(0, i), (i, 1201)]).collect());
        raw.nodes[500].data["phase"] = "Failed".into();
        let scene = Scene::new(&raw, None, true, None);
        assert_eq!(scene.graph.nodes.len(), 3);
        assert_eq!(scene.graph.edges, vec![(0, 1), (1, 2)]);
        assert_eq!(scene.members[1].len(), 1200);
        assert_eq!(scene.graph.nodes[1].data["phase"], "Failed");
        let reveal = Scene::new(&raw, None, true, Some(500));
        assert!(reveal.members.contains(&vec![500]));
        assert_eq!(reveal.members.iter().map(Vec::len).sum::<usize>(), 1202);
        let inner = Scene::new(&raw, Some(&scene.members[1]), true, None);
        assert_eq!(inner.members.len(), 6);
        assert!(inner.members.iter().all(|m| m.len() == 200));
        raw.nodes[500].data["phase"] = serde_json::Value::Null;
        raw.nodes[700].data["phase"] = "Skipped".into();
        assert_eq!(
            Scene::new(&raw, None, true, None).graph.nodes[1].data["phase"],
            "Pending"
        );
        raw.nodes[500].data["phase"] = "Succeeded".into();
        assert_eq!(
            Scene::new(&raw, None, true, None).graph.nodes[1].data["phase"],
            "Succeeded"
        );
    }
    #[::core::prelude::v1::test]
    fn long_chains_are_folded_without_hiding_forks_or_losing_members() {
        let raw = graph(100, (0..99).map(|i| (i, i + 1)).collect());
        let scene = Scene::new(&raw, None, true, None);
        assert_eq!(scene.members.len(), 4);
        assert_eq!(scene.graph.edges, vec![(0, 1), (1, 2), (2, 3)]);
        assert_eq!(
            scene
                .members
                .iter()
                .flatten()
                .copied()
                .collect::<BTreeSet<_>>()
                .len(),
            100
        );
        let cycle = graph(10, (0..10).map(|i| (i, (i + 1) % 10)).collect());
        assert_eq!(Scene::new(&cycle, None, true, None).graph.nodes.len(), 10);
        let branches = graph(12, (1..11).flat_map(|i| [(0, i), (i, 11)]).collect());
        let grouped = Scene::new(&branches, None, true, None);
        assert_eq!(grouped.members.first(), Some(&vec![0]));
        assert_eq!(grouped.members.last(), Some(&vec![11]));
    }
    #[::core::prelude::v1::test]
    fn fit_centers_wide_and_tall_graphs_and_zoom_keeps_its_anchor() {
        for raw in [
            graph(1200, vec![]),
            graph(200, (0..199).map(|i| (i, i + 1)).collect()),
        ] {
            let scene = Scene::new(&raw, None, false, None);
            let viewport = size(px(320.), px(420.));
            let mut camera = Camera::default().resolved(&scene, viewport);
            for i in 0..scene.graph.nodes.len() {
                let bounds = camera.bounds(scene.node_bounds(i), viewport);
                assert!(bounds.left() >= px(0.) && bounds.right() <= viewport.width);
                assert!(bounds.top() >= px(0.) && bounds.bottom() <= viewport.height - px(48.));
            }
            let anchor = point(px(80.), px(90.));
            let mut zoomed = camera;
            for _ in 0..8 {
                let before = zoomed.scale;
                zoomed.zoom(0.5, anchor, viewport);
                assert!(
                    zoomed.scale < before,
                    "zooming out must not enlarge a fitted graph"
                );
            }
            let before = (anchor - camera.origin(viewport)) / camera.scale;
            camera.zoom(1.5, anchor, viewport);
            let after = (anchor - camera.origin(viewport)) / camera.scale;
            assert!((before.x - after.x).abs() < px(0.1));
            assert!((before.y - after.y).abs() < px(0.1));
        }
    }
}
