//! Cached row presentation; the base tree retains navigation and virtualization.

use std::time::Instant;

use gpui_kit::base::TreeEntryState;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Selectable, Sizable, StyledExt, list::ListItem};
use gpui_kit::{
    App, Context, Entity, IntoElement, MouseButton, Render, SharedString, Subscription, WeakEntity, Window, div, prelude::*, rems,
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
}

impl RowState {
    fn new(entry: &TreeEntry, interaction: TreeEntryState, explorer: &Explorer) -> Self {
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
        }
    }
}

pub(super) struct TopicRow {
    explorer: WeakEntity<Explorer>,
    path: SharedString,
    id: SharedString,
    label: SharedString,
    preview: SharedString,
    state: RowState,
    _subscriptions: [Subscription; 2],
    #[cfg(test)]
    pub(super) render_count: usize,
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
        let state = RowState::new(entry, interaction, explorer.read(cx));
        let preview = Self::preview(&path, explorer.read(cx));
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
            #[cfg(test)]
            render_count: 0,
        }
    }

    fn preview(path: &str, explorer: &Explorer) -> SharedString {
        explorer.topics.nodes.get(path).map_or_else(SharedString::default, |node| {
            node.value
                .as_ref()
                .map_or_else(
                    || topic_summary(node.topics, node.messages),
                    |value| format!("= {}", value.preview()),
                )
                .into()
        })
    }

    pub(super) fn sync(&mut self, entry: &TreeEntry, interaction: TreeEntryState, explorer: &Explorer) {
        let state = RowState::new(entry, interaction, explorer);
        if self.state != state {
            self.state = state;
            self.preview = Self::preview(&self.path, explorer);
        }
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.preview.clone()),
                    ),
            )
    }
}

impl Render for TopicRow {
    #[hotpath::measure(impl_type = "TopicRow")]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        {
            self.render_count += 1;
        }
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
