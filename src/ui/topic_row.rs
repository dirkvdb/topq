//! Cached row presentation; the base tree retains navigation and virtualization.

use std::time::Instant;

use gpui_kit::base::TreeEntryState;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt, list::ListItem, tag::Tag};
use gpui_kit::{
    App, Context, Entity, IntoElement, MouseButton, Render, SharedString, Subscription, TestSupportExt, WeakEntity, Window, div,
    prelude::*, rems,
};

use super::{Explorer, TreeEntry, topic_summary};
use crate::appearance::Appearance;

/// Only shallow entry metadata is retained, never a clone of the tree subtree.
#[derive(Clone, Copy, PartialEq, Eq)]
struct RowState {
    depth: usize,
    folder: bool,
    expanded: bool,
    disabled: bool,
    interaction: TreeEntryState,
    updated: Option<Instant>,
    topics: usize,
    messages: u64,
    has_value: bool,
    inline_payload: bool,
    max_chars: Option<usize>,
}

impl RowState {
    fn new(entry: &TreeEntry, interaction: TreeEntryState, explorer: &Explorer, cx: &App) -> Self {
        let node = explorer.topics.nodes.get(entry.item().id.as_str());
        Self {
            depth: entry.depth(),
            folder: entry.is_folder(),
            expanded: entry.is_expanded(),
            disabled: entry.is_disabled(),
            interaction,
            updated: node.and_then(|node| node.updated),
            topics: node.map_or(0, |node| node.topics),
            messages: node.map_or(0, |node| node.messages),
            has_value: node.is_some_and(|node| node.value.is_some()),
            inline_payload: Appearance::inline_payload(cx),
            max_chars: Appearance::inline_payload_limit_enabled(cx).then(|| Appearance::inline_payload_max_chars(cx)),
        }
    }
}

enum RowPreview {
    Hidden,
    Text(SharedString),
    Datatype(SharedString),
}

pub(super) struct TopicRow {
    explorer: WeakEntity<Explorer>,
    path: SharedString,
    id: SharedString,
    label: SharedString,
    preview: RowPreview,
    state: RowState,
    _subscriptions: [Subscription; 2],
}

impl TopicRow {
    pub(super) fn new(explorer: &Entity<Explorer>, entry: &TreeEntry, interaction: TreeEntryState, cx: &mut Context<Self>) -> Self {
        let path = entry.item().id.clone();
        let label = path
            .rsplit('/')
            .next()
            .filter(|level| !level.is_empty())
            .unwrap_or("(empty level)")
            .to_owned()
            .into();
        let state = RowState::new(entry, interaction, explorer.read(cx), cx);
        let preview = Self::preview(&path, explorer.read(cx), state.inline_payload, state.max_chars);
        let tree_state = explorer.read(cx).tree_state.clone();
        Self {
            explorer: explorer.downgrade(),
            id: format!("topic:{path}").into(),
            path,
            label,
            preview,
            state,
            _subscriptions: [
                cx.observe(explorer, |_, _, cx| cx.notify()),
                cx.observe(&tree_state, |_, _, cx| cx.notify()),
            ],
        }
    }

    fn preview(path: &str, explorer: &Explorer, inline_payload: bool, max_chars: Option<usize>) -> RowPreview {
        let Some(node) = explorer.topics.nodes.get(path) else {
            return RowPreview::Hidden;
        };
        let Some(value) = &node.value else {
            return RowPreview::Text(topic_summary(node.topics, node.messages).into());
        };
        if !inline_payload {
            return RowPreview::Hidden;
        }
        match value.preview(max_chars) {
            Some(preview) => RowPreview::Text(format!("= {preview}").into()),
            None => RowPreview::Datatype(value.datatype_label().to_owned().into()),
        }
    }

    pub(super) fn sync(row: &Entity<Self>, entry: &TreeEntry, interaction: TreeEntryState, explorer: &Entity<Explorer>, cx: &mut App) {
        let state = RowState::new(entry, interaction, explorer.read(cx), cx);
        // Fast tracks update leases as writes, even when the closure changes nothing.
        if row.read(cx).state == state {
            return;
        }
        let preview = Self::preview(&row.read(cx).path, explorer.read(cx), state.inline_payload, state.max_chars);
        row.update(cx, |row, _| {
            row.state = state;
            row.preview = preview;
        });
    }

    pub(super) fn item(&self, explorer: &Explorer, cx: &App) -> ListItem {
        let node = explorer.topics.nodes.get(self.path.as_str());
        let highlight = if Appearance::motion_reduced(cx) {
            0.
        } else {
            node.map_or(0., |node| node.flash_amount(Instant::now()))
        };
        let state = explorer.tree_state.clone();
        ListItem::new(self.id.clone())
            .accessibility_label(if self.path.is_empty() {
                SharedString::from("Empty topic level")
            } else {
                self.path.clone()
            })
            .disabled(self.state.disabled)
            .selected(self.state.interaction.is_selected())
            .secondary_selected(self.state.interaction.is_right_clicked())
            .h_6()
            .text_sm()
            .text_color(cx.theme().foreground.blend(cx.theme().warning.opacity(highlight)))
            .rounded(cx.theme().radius_tokens().sm)
            .pl(rems(0.625 + self.state.depth as f32 * 0.875))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                state.update(cx, |state, cx| state.focus(window, cx))
            })
            .child(
                div()
                    .h_flex()
                    .min_w_0()
                    .gap_1()
                    .child(div().w_4().flex_none().when(self.state.folder, |slot| {
                        slot.child(
                            Icon::new(if self.state.expanded {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .small()
                            .text_color(cx.theme().foreground),
                        )
                    }))
                    .child(div().min_w_0().truncate().font_medium().child(self.label.clone()))
                    .map(|this| match &self.preview {
                        RowPreview::Hidden => this,
                        RowPreview::Text(preview) => this.child(
                            div()
                                .id(SharedString::from(format!("{}:preview", self.id)))
                                .test_support()
                                .aria_label(preview.clone())
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(preview.clone()),
                        ),
                        RowPreview::Datatype(datatype) => this.child(
                            Tag::secondary().outline().small().min_w_0().child(
                                div()
                                    .id(SharedString::from(format!("{}:datatype", self.id)))
                                    .test_support()
                                    .aria_label(datatype.clone())
                                    .min_w_0()
                                    .truncate()
                                    .child(datatype.clone()),
                            ),
                        ),
                    }),
            )
    }
}

impl Render for TopicRow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(explorer) = self.explorer.upgrade() else {
            return div().into_any_element();
        };
        let explorer = explorer.read(cx);
        if !Appearance::motion_reduced(cx)
            && explorer
                .topics
                .nodes
                .get(self.path.as_str())
                .is_some_and(|node| node.flashing(Instant::now()))
        {
            // Called inside the row's view scope, so only this row is dirtied.
            // Offscreen rows do not render and therefore do not renew the request.
            window.request_animation_frame();
        }
        self.item(explorer, cx).into_any_element()
    }
}
