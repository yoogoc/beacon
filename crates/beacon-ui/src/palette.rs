//! The command palette.
//!
//! Lens is a mouse application. Beacon's bet is that a Kubernetes client is
//! something you keep open all day and navigate constantly, and that the
//! fastest way through it is a text field — so the palette is not a shortcut
//! for the menus, it is the primary way to move.
//!
//! What it lists is decided by a prefix, because the alternative is a mode
//! switch the user has to remember they are in:
//!
//! ```text
//! (nothing)   objects of the kind currently on screen
//! @           resource kinds, including CRDs
//! #           namespaces
//! ctx         clusters
//! >           commands
//! ```
//!
//! Prefixes are the whole routing rule. Matching inside a section is fuzzy, so
//! `@dep` finds Deployment and `#kube-sys` finds kube-system.

use std::sync::Arc;

use beacon_kube::{ClusterId, Kind, ObjectRef, Operation};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;
use nucleo_matcher::{
    Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};

/// How many matches the palette will show. Beyond this nobody is reading the
/// list, they are typing more.
const LIMIT: usize = 50;

/// The prefix that selects each section.
const KINDS: char = '@';
const NAMESPACES: char = '#';
const COMMANDS: char = '>';
const CLUSTERS: &str = "ctx ";

/// What the palette is listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Objects,
    Kinds,
    Namespaces,
    Clusters,
    Commands,
}

impl Section {
    /// Splits a query into the section it selects and the text to match.
    ///
    /// `ctx` is a word rather than a symbol because it is the rarest of the
    /// five and the symbols are better spent on the common ones.
    pub fn parse(query: &str) -> (Self, &str) {
        if let Some(rest) = query.strip_prefix(CLUSTERS) {
            return (Self::Clusters, rest);
        }
        // `ctx` alone, still being typed, should already show the clusters.
        if query == CLUSTERS.trim_end() {
            return (Self::Clusters, "");
        }

        match query.chars().next() {
            Some(KINDS) => (Self::Kinds, &query[KINDS.len_utf8()..]),
            Some(NAMESPACES) => (Self::Namespaces, &query[NAMESPACES.len_utf8()..]),
            Some(COMMANDS) => (Self::Commands, &query[COMMANDS.len_utf8()..]),
            _ => (Self::Objects, query),
        }
    }

    fn placeholder(&self, current_kind: Option<&str>) -> String {
        match self {
            Self::Objects => match current_kind {
                Some(kind) => format!("Search {kind}s…   @ kind   # namespace   > command"),
                None => "@ kind   # namespace   ctx cluster   > command".to_string(),
            },
            Self::Kinds => "Jump to a resource kind".to_string(),
            Self::Namespaces => "Scope to a namespace".to_string(),
            Self::Clusters => "Switch cluster".to_string(),
            Self::Commands => "Run a command".to_string(),
        }
    }

    fn heading(&self) -> &'static str {
        match self {
            Self::Objects => "Objects",
            Self::Kinds => "Kinds",
            Self::Namespaces => "Namespaces",
            Self::Clusters => "Clusters",
            Self::Commands => "Commands",
        }
    }
}

/// Something the palette can do that is not navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ToggleTheme,
    ToggleDetails,
    ClearFilter,
    CopyName,
}

impl Action {
    const ALL: [Self; 4] = [
        Self::ToggleTheme,
        Self::ToggleDetails,
        Self::ClearFilter,
        Self::CopyName,
    ];

    fn label(&self) -> &'static str {
        match self {
            Self::ToggleTheme => "Toggle light and dark",
            Self::ToggleDetails => "Show or hide the details panel",
            Self::ClearFilter => "Clear the search filter",
            Self::CopyName => "Copy the selected object's name",
        }
    }
}

/// What the user picked.
#[derive(Debug, Clone)]
pub enum Choice {
    Kind(Arc<Kind>),
    /// `None` is every namespace.
    Namespace(Option<String>),
    Cluster(ClusterId),
    Object(ObjectRef),
    Action(Action),
    /// Something that changes the cluster, aimed at the selected object.
    Operation(Operation),
    /// Forward a port the selected pod declares.
    Forward {
        remote_port: u16,
    },
}

pub enum PaletteEvent {
    Chose(Choice),
    Dismissed,
}

impl EventEmitter<PaletteEvent> for Palette {}

/// Everything the palette can offer, snapshotted when it opens.
///
/// A snapshot rather than a live borrow: the palette is open for a few seconds
/// and a list that changed underneath the highlighted row would confirm the
/// wrong thing.
#[derive(Default)]
pub struct Sources {
    pub kinds: Vec<Arc<Kind>>,
    pub namespaces: Vec<String>,
    pub clusters: Vec<ClusterId>,
    pub objects: Vec<ObjectRef>,
    /// What can be done to the selected object, already marked with whether
    /// this user may do it. Empty when nothing is selected.
    pub operations: Vec<crate::actions::Choice>,
    /// The ports the selected pod declares. Offered one per port rather than
    /// behind a prompt: the pod already said which ports it has, so asking
    /// again would be asking the user to read the manifest for us.
    pub ports: Vec<u16>,
    /// The kind on screen, for the placeholder.
    pub current_kind: Option<String>,
}

pub struct Palette {
    pub state: Entity<CommandState>,
    sources: Sources,
    section: Section,
    matches: Vec<Choice>,
    matcher: Matcher,
}

impl Palette {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| CommandState::new(window, cx));
        let mut this = Self {
            state,
            sources: Sources::default(),
            section: Section::Objects,
            matches: Vec::new(),
            matcher: crate::catalog::matcher(),
        };
        this.refresh("");
        this
    }

    /// Fills the palette and clears whatever was typed last time.
    pub fn open(&mut self, sources: Sources, window: &mut Window, cx: &mut Context<Self>) {
        self.sources = sources;
        self.state
            .update(cx, |state, cx| state.set_query("", window, cx));
        self.refresh("");
        self.state.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    /// Recomputes the list for a query.
    fn refresh(&mut self, query: &str) {
        let (section, needle) = Section::parse(query);
        self.section = section;

        let candidates: Vec<(String, Choice)> = match section {
            Section::Objects => self
                .sources
                .objects
                .iter()
                .map(|object| (object.to_string(), Choice::Object(object.clone())))
                .collect(),
            Section::Kinds => self
                .sources
                .kinds
                .iter()
                .map(|kind| (kind.display_name(), Choice::Kind(kind.clone())))
                .collect(),
            Section::Namespaces => {
                std::iter::once(("All namespaces".to_string(), Choice::Namespace(None)))
                    .chain(self.sources.namespaces.iter().map(|namespace| {
                        (
                            namespace.clone(),
                            Choice::Namespace(Some(namespace.clone())),
                        )
                    }))
                    .collect()
            }
            Section::Clusters => self
                .sources
                .clusters
                .iter()
                .map(|cluster| (cluster.to_string(), Choice::Cluster(cluster.clone())))
                .collect(),
            Section::Commands => self
                .sources
                .operations
                .iter()
                .map(|choice| {
                    (
                        choice.label.trim_end_matches('…').to_string(),
                        Choice::Operation(choice.operation.clone()),
                    )
                })
                .chain(self.sources.ports.iter().map(|port| {
                    (
                        format!("Forward port {port}"),
                        Choice::Forward { remote_port: *port },
                    )
                }))
                .chain(
                    Action::ALL
                        .iter()
                        .map(|action| (action.label().to_string(), Choice::Action(*action))),
                )
                .collect(),
        };

        self.matches = rank(&candidates, needle, &mut self.matcher, LIMIT);
    }

    /// Whether a choice is offered but refused, and why.
    ///
    /// The refusal comes from the permission preflight; see
    /// [`crate::actions`].
    fn blocked(&self, choice: &Choice) -> Option<&str> {
        let Choice::Operation(operation) = choice else {
            return None;
        };
        self.sources
            .operations
            .iter()
            .find(|candidate| &candidate.operation == operation)
            .filter(|candidate| !candidate.allowed)
            .and_then(|candidate| candidate.tooltip())
    }

    /// The label for a choice, rebuilt rather than stored: the list is at most
    /// [`LIMIT`] long and this keeps one copy of the truth.
    fn label(&self, choice: &Choice) -> SharedString {
        match choice {
            Choice::Object(object) => SharedString::from(object.to_string()),
            Choice::Kind(kind) => SharedString::from(kind.display_name()),
            Choice::Namespace(None) => SharedString::from("All namespaces"),
            Choice::Namespace(Some(namespace)) => SharedString::from(namespace.clone()),
            Choice::Cluster(cluster) => SharedString::from(cluster.to_string()),
            Choice::Action(action) => SharedString::from(action.label()),
            Choice::Operation(operation) => SharedString::from(operation.describe()),
            Choice::Forward { remote_port } => {
                SharedString::from(format!("Forward port {remote_port}"))
            }
        }
    }
}

/// Scores candidates against a needle, best first.
///
/// An empty needle keeps the given order, which is what makes `@` on its own
/// show the kinds in the order the sidebar does.
fn rank(
    candidates: &[(String, Choice)],
    needle: &str,
    matcher: &mut Matcher,
    limit: usize,
) -> Vec<Choice> {
    if needle.is_empty() {
        return candidates
            .iter()
            .take(limit)
            .map(|(_, choice)| choice.clone())
            .collect();
    }

    let pattern = Pattern::parse(needle, CaseMatching::Smart, Normalization::Smart);
    let mut buffer = Vec::new();

    let mut scored: Vec<(u32, &str, &Choice)> = candidates
        .iter()
        .filter_map(|(label, choice)| {
            let score = pattern.score(Utf32Str::new(label, &mut buffer), matcher)?;
            Some((score, label.as_str(), choice))
        })
        .collect();

    // Score, then name: equal scores must not reshuffle between keystrokes.
    scored.sort_by(|(left_score, left, _), (right_score, right, _)| {
        right_score.cmp(left_score).then_with(|| left.cmp(right))
    });

    scored
        .into_iter()
        .take(limit)
        .map(|(_, _, choice)| choice.clone())
        .collect()
}

impl Render for Palette {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let on_query = this.clone();
        let on_confirm = this.clone();
        let on_cancel = this;

        let items: Vec<CommandItem> = self
            .matches
            .iter()
            .map(|choice| {
                // A refused action is shown, disabled, with the reason -- the
                // alternative is a menu that quietly hides what you cannot do,
                // which tells nobody anything.
                match self.blocked(choice) {
                    Some(reason) => CommandItem::new()
                        .label(format!("{} — {reason}", self.label(choice)))
                        .disabled(true),
                    None => CommandItem::new().label(self.label(choice)),
                }
            })
            .collect();

        let heading = self.section.heading();
        let placeholder = self
            .section
            .placeholder(self.sources.current_kind.as_deref());

        v_flex()
            .w(px(640.))
            .max_h(px(420.))
            .overflow_hidden()
            .rounded_lg()
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .shadow_lg()
            .child(
                Command::new(&self.state)
                    // Matching is ours: the prefix decides which list is being
                    // searched, and the component cannot know that.
                    .filterable(false)
                    .placeholder(placeholder)
                    .group(
                        gpui_kit::component::command::CommandGroup::new()
                            .label(heading)
                            .items(items),
                    )
                    .on_query(move |query, _, cx| {
                        let _ = on_query.update(cx, |palette, cx| {
                            palette.refresh(query);
                            cx.notify();
                        });
                    })
                    .on_confirm(move |index, _, cx| {
                        let _ = on_confirm.update(cx, |palette, cx| {
                            let Some(choice) = palette.matches.get(index.row).cloned() else {
                                return;
                            };
                            if palette.blocked(&choice).is_some() {
                                return;
                            }
                            cx.emit(PaletteEvent::Chose(choice));
                        });
                    })
                    .on_cancel(move |_, cx| {
                        let _ = on_cancel.update(cx, |_, cx| cx.emit(PaletteEvent::Dismissed));
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Action, Choice, Section, rank};
    use beacon_kube::{ApiResource, GroupVersionKind, Kind};
    use std::sync::Arc;

    fn kind(group: &str, name: &str) -> Arc<Kind> {
        Arc::new(Kind {
            resource: ApiResource::from_gvk_with_plural(
                &GroupVersionKind::gvk(group, "v1", name),
                &format!("{}s", name.to_lowercase()),
            ),
            namespaced: true,
            verbs: vec!["list".into(), "watch".into()],
        })
    }

    #[test]
    fn a_prefix_chooses_the_section() {
        assert_eq!(Section::parse("api-7f9"), (Section::Objects, "api-7f9"));
        assert_eq!(Section::parse("@dep"), (Section::Kinds, "dep"));
        assert_eq!(Section::parse("#kube"), (Section::Namespaces, "kube"));
        assert_eq!(Section::parse(">theme"), (Section::Commands, "theme"));
        assert_eq!(Section::parse("ctx prod"), (Section::Clusters, "prod"));
    }

    /// A bare prefix is a section with no filter, which is how the palette
    /// doubles as a menu.
    #[test]
    fn a_bare_prefix_lists_the_whole_section() {
        assert_eq!(Section::parse("@"), (Section::Kinds, ""));
        assert_eq!(Section::parse("#"), (Section::Namespaces, ""));
        assert_eq!(Section::parse(">"), (Section::Commands, ""));
        assert_eq!(Section::parse(""), (Section::Objects, ""));
    }

    /// `ctx` is three characters typed one at a time; the clusters should
    /// appear on the third, not only after the space.
    #[test]
    fn the_cluster_prefix_works_before_its_space() {
        assert_eq!(Section::parse("ctx"), (Section::Clusters, ""));
        assert_eq!(Section::parse("ctx "), (Section::Clusters, ""));
        assert_eq!(Section::parse("ctx pro"), (Section::Clusters, "pro"));
    }

    /// A name that merely starts with those letters is not the prefix.
    #[test]
    fn a_word_starting_with_ctx_is_still_an_object() {
        assert_eq!(Section::parse("ctxd-api"), (Section::Objects, "ctxd-api"));
    }

    fn candidates() -> Vec<(String, Choice)> {
        vec![
            ("Pod".into(), Choice::Kind(kind("", "Pod"))),
            (
                "Deployment (apps)".into(),
                Choice::Kind(kind("apps", "Deployment")),
            ),
            (
                "DaemonSet (apps)".into(),
                Choice::Kind(kind("apps", "DaemonSet")),
            ),
        ]
    }

    fn labels(choices: &[Choice]) -> Vec<String> {
        choices
            .iter()
            .map(|choice| match choice {
                Choice::Kind(kind) => kind.display_name(),
                Choice::Action(action) => action.label().to_string(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn ranking_puts_the_best_match_first() {
        let found = rank(&candidates(), "dep", &mut crate::catalog::matcher(), 50);
        assert_eq!(
            labels(&found).first().map(String::as_str),
            Some("Deployment (apps)")
        );
    }

    /// With nothing typed the section keeps the order it was given, which is
    /// the sidebar's order for kinds.
    #[test]
    fn an_empty_query_preserves_the_given_order() {
        let found = rank(&candidates(), "", &mut crate::catalog::matcher(), 50);
        assert_eq!(
            labels(&found),
            ["Pod", "Deployment (apps)", "DaemonSet (apps)"]
        );
    }

    /// A cluster with five thousand pods must not put five thousand rows in
    /// front of somebody who has typed one character.
    #[test]
    fn the_list_is_capped() {
        let many: Vec<(String, Choice)> = (0..5_000)
            .map(|index| (format!("pod-{index:05}"), Choice::Kind(kind("", "Pod"))))
            .collect();

        assert_eq!(
            rank(&many, "", &mut crate::catalog::matcher(), 50).len(),
            50
        );
        assert_eq!(
            rank(&many, "pod", &mut crate::catalog::matcher(), 50).len(),
            50
        );
    }

    #[test]
    fn a_query_matching_nothing_returns_nothing() {
        assert!(rank(&candidates(), "zzzz", &mut crate::catalog::matcher(), 50).is_empty());
    }

    #[test]
    fn every_action_is_reachable_from_the_command_section() {
        let candidates: Vec<(String, Choice)> = Action::ALL
            .iter()
            .map(|action| (action.label().to_string(), Choice::Action(*action)))
            .collect();

        for action in Action::ALL {
            let found = rank(
                &candidates,
                action.label(),
                &mut crate::catalog::matcher(),
                50,
            );
            assert_eq!(
                labels(&found).first().map(String::as_str),
                Some(action.label()),
                "{action:?}"
            );
        }
    }
}
