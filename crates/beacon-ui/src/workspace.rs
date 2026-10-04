//! Window-local tab groups. Tab contents live outside this tree so a move never
//! rebuilds a resource view or drops its watches, editor, or terminal.

pub(crate) type PaneId = u64;
pub(crate) type TabId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    pub fn horizontal(self) -> bool {
        matches!(self, Self::Left | Self::Right)
    }

    fn before(self) -> bool {
        matches!(self, Self::Left | Self::Up)
    }
}

/// The middle merges into a group; the nearest outer edge creates a split.
/// Coordinates are normalized so the same hit areas work at every pane size.
pub(crate) fn split_at(x: f32, y: f32) -> Option<Direction> {
    [
        (x, Direction::Left),
        (1. - x, Direction::Right),
        (y, Direction::Up),
        (1. - y, Direction::Down),
    ]
    .into_iter()
    .filter(|(distance, _)| *distance < 0.22)
    .min_by(|a, b| a.0.total_cmp(&b.0))
    .map(|(_, direction)| direction)
}

#[derive(Clone, Debug)]
pub(crate) struct Pane {
    pub id: PaneId,
    pub tabs: Vec<TabId>,
    pub active: Option<TabId>,
}

#[derive(Clone, Debug)]
pub(crate) enum Node {
    Pane(Pane),
    Split {
        id: u64,
        horizontal: bool,
        first: Box<Node>,
        second: Box<Node>,
    },
}

impl Node {
    pub fn split_ids(&self, ids: &mut Vec<u64>) {
        if let Self::Split {
            id, first, second, ..
        } = self
        {
            ids.push(*id);
            first.split_ids(ids);
            second.split_ids(ids);
        }
    }
    pub fn panes(&self, panes: &mut Vec<Pane>) {
        match self {
            Self::Pane(pane) => panes.push(pane.clone()),
            Self::Split { first, second, .. } => {
                first.panes(panes);
                second.panes(panes);
            }
        }
    }

    fn pane(&self, id: PaneId) -> Option<&Pane> {
        match self {
            Self::Pane(pane) => (pane.id == id).then_some(pane),
            Self::Split { first, second, .. } => first.pane(id).or_else(|| second.pane(id)),
        }
    }

    fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        match self {
            Self::Pane(pane) => (pane.id == id).then_some(pane),
            Self::Split { first, second, .. } => first.pane_mut(id).or_else(|| second.pane_mut(id)),
        }
    }

    fn collapse(self) -> Option<Self> {
        match self {
            Self::Pane(pane) => (!pane.tabs.is_empty()).then_some(Self::Pane(pane)),
            Self::Split {
                id,
                horizontal,
                first,
                second,
            } => match (first.collapse(), second.collapse()) {
                (Some(first), Some(second)) => Some(Self::Split {
                    id,
                    horizontal,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(node), None) | (None, Some(node)) => Some(node),
                (None, None) => None,
            },
        }
    }

    fn split(&mut self, pane: PaneId, new: Pane, id: u64, direction: Direction) -> bool {
        match self {
            Self::Pane(old) if old.id == pane => {
                let old = Box::new(Self::Pane(old.clone()));
                let new = Box::new(Self::Pane(new));
                let (first, second) = if direction.before() {
                    (new, old)
                } else {
                    (old, new)
                };
                *self = Self::Split {
                    id,
                    horizontal: direction.horizontal(),
                    first,
                    second,
                };
                true
            }
            Self::Pane(_) => false,
            Self::Split { first, second, .. } => {
                first.split(pane, new.clone(), id, direction)
                    || second.split(pane, new, id, direction)
            }
        }
    }
}

pub(crate) struct Workspace {
    pub root: Node,
    pub focused: PaneId,
    next: u64,
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            root: Node::Pane(Pane {
                id: 0,
                tabs: vec![],
                active: None,
            }),
            focused: 0,
            next: 1,
        }
    }
}

impl Workspace {
    pub fn panes(&self) -> Vec<Pane> {
        let mut panes = Vec::new();
        self.root.panes(&mut panes);
        panes
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.root.pane(id)
    }

    pub fn owner(&self, tab: TabId) -> Option<PaneId> {
        self.panes()
            .iter()
            .find(|pane| pane.tabs.contains(&tab))
            .map(|pane| pane.id)
    }

    pub fn active(&self) -> Option<TabId> {
        self.pane(self.focused).and_then(|pane| pane.active)
    }

    pub fn visible(&self, tab: TabId) -> bool {
        self.panes().iter().any(|pane| pane.active == Some(tab))
    }

    pub fn focus(&mut self, id: PaneId) {
        if self.pane(id).is_some() {
            self.focused = id;
        }
    }

    pub fn select(&mut self, tab: TabId) -> bool {
        let Some(id) = self.owner(tab) else {
            return false;
        };
        self.root.pane_mut(id).unwrap().active = Some(tab);
        self.focused = id;
        true
    }

    pub fn insert(&mut self, tab: TabId, pane: PaneId, before: Option<TabId>) -> bool {
        if self.owner(tab).is_some() || self.pane(pane).is_none() {
            return false;
        }
        let target = self.root.pane_mut(pane).unwrap();
        let ix = before
            .and_then(|id| target.tabs.iter().position(|tab| *tab == id))
            .unwrap_or(target.tabs.len());
        target.tabs.insert(ix, tab);
        target.active = Some(tab);
        self.focused = pane;
        true
    }

    pub fn remove(&mut self, tab: TabId) -> bool {
        let Some(owner) = self.owner(tab) else {
            return false;
        };
        self.remove_from_pane(tab, owner);
        self.collapse();
        true
    }

    fn remove_from_pane(&mut self, tab: TabId, owner: PaneId) {
        let pane = self.root.pane_mut(owner).unwrap();
        let ix = pane.tabs.iter().position(|id| *id == tab).unwrap();
        pane.tabs.remove(ix);
        if pane.active == Some(tab) {
            pane.active = pane
                .tabs
                .get(ix.min(pane.tabs.len().saturating_sub(1)))
                .copied();
        }
    }

    fn collapse(&mut self) {
        self.root = self.root.clone().collapse().unwrap_or_else(|| {
            Node::Pane(Pane {
                id: self.focused,
                tabs: vec![],
                active: None,
            })
        });
        if self.pane(self.focused).is_none() {
            self.focused = self.panes()[0].id;
        }
    }

    pub fn split(&mut self, pane: PaneId, tab: TabId, direction: Direction) -> bool {
        if self.owner(tab).is_some() || self.pane(pane).is_none() {
            return false;
        }
        let id = self.next;
        self.next += 2;
        let new = Pane {
            id,
            tabs: vec![tab],
            active: Some(tab),
        };
        self.root.split(pane, new, id + 1, direction);
        self.focused = id;
        self.collapse();
        true
    }

    /// Validate the destination before removing anything. Keep an empty source
    /// alive until insertion so moving its only tab back to itself is harmless.
    pub fn move_tab(
        &mut self,
        tab: TabId,
        pane: PaneId,
        before: Option<TabId>,
        split: Option<Direction>,
    ) -> bool {
        let Some(owner) = self.owner(tab) else {
            return false;
        };
        if self.pane(pane).is_none() {
            return false;
        }
        if owner == pane
            && (before == Some(tab) || split.is_some() && self.pane(pane).unwrap().tabs.len() == 1)
        {
            return self.select(tab);
        }
        self.remove_from_pane(tab, owner);
        if let Some(direction) = split {
            self.split(pane, tab, direction);
        } else {
            self.insert(tab, pane, before);
            self.collapse();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_edges_split_in_each_direction_and_center_merges() {
        assert_eq!(split_at(0.1, 0.5), Some(Direction::Left));
        assert_eq!(split_at(0.9, 0.5), Some(Direction::Right));
        assert_eq!(split_at(0.5, 0.1), Some(Direction::Up));
        assert_eq!(split_at(0.5, 0.9), Some(Direction::Down));
        assert_eq!(split_at(0.05, 0.1), Some(Direction::Left));
        assert_eq!(split_at(0.5, 0.5), None);
        assert_eq!(split_at(0.23, 0.77), None);
    }

    fn assert_tabs(layout: &Workspace, expected: &[TabId]) {
        let mut actual: Vec<_> = layout
            .panes()
            .into_iter()
            .flat_map(|pane| {
                assert!(pane.active.is_none_or(|id| pane.tabs.contains(&id)));
                pane.tabs
            })
            .collect();
        actual.sort();
        let mut expected = expected.to_vec();
        expected.sort();
        assert_eq!(actual, expected);
        assert!(layout.pane(layout.focused).is_some());
    }

    #[test]
    fn nested_splits_keep_sibling_tabs_visible_and_collapse_when_closed() {
        let mut w = Workspace::default();
        w.insert(10, 0, None);
        w.insert(11, 0, None);
        w.split(0, 12, Direction::Right);
        let right = w.focused;
        w.split(right, 13, Direction::Down);
        assert_eq!(w.panes().len(), 3);
        assert!(w.visible(11) && w.visible(12) && w.visible(13));
        assert!(!w.visible(10));
        w.remove(12);
        assert_eq!(w.panes().len(), 2);
        w.remove(13);
        assert_eq!(w.panes().len(), 1);
        assert_tabs(&w, &[10, 11]);
        w.remove(11);
        assert_eq!(w.active(), Some(10));
        w.remove(10);
        assert_tabs(&w, &[]);
        assert_eq!(w.panes().len(), 1);
    }

    #[test]
    fn moves_reorder_and_empty_source_collapses_without_losing_tabs() {
        let mut w = Workspace::default();
        for tab in [10, 11, 12] {
            w.insert(tab, 0, None);
        }
        w.move_tab(12, 0, Some(10), None);
        assert_eq!(w.pane(0).unwrap().tabs, [12, 10, 11]);
        w.move_tab(10, 0, None, Some(Direction::Left));
        let left = w.focused;
        assert_eq!(w.panes()[0].id, left);
        w.move_tab(10, 0, Some(11), None);
        assert_eq!(w.panes().len(), 1);
        assert_eq!(w.pane(0).unwrap().tabs, [12, 10, 11]);
        assert_tabs(&w, &[10, 11, 12]);
        assert!(!w.move_tab(10, 999, None, None));
        assert!(!w.insert(10, 0, None));
        assert_tabs(&w, &[10, 11, 12]);
    }

    #[test]
    fn last_tab_self_drop_and_cross_window_transfer_are_lossless() {
        let mut source = Workspace::default();
        let mut target = Workspace::default();
        source.insert(42, 0, None);
        source.move_tab(42, 0, None, Some(Direction::Down));
        source.move_tab(42, 0, Some(42), None);
        assert_eq!(source.panes().len(), 1);
        source.remove(42);
        target.insert(42, 0, None);
        assert_tabs(&source, &[]);
        assert_tabs(&target, &[42]);
    }
}
