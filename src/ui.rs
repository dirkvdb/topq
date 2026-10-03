//! Connection configuration, live topic navigation, and selected-topic details.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, IndexPath, Sizable, StyledExt, TitleBar,
    button::{Button, ButtonVariants},
    input::{Editor, EditorState, InputEvent, InputState},
    list::ListItem,
    menu::{DropdownMenu, PopupMenuItem},
    resizable::{ResizablePanelEvent, ResizableState, h_resizable, resizable_panel},
    select::SelectState,
    spinner::Spinner,
    tag::Tag,
    tree::{Tree, TreeEntry, TreeEvent, TreeItem, TreeState},
};
use gpui_kit::{
    App, ClipboardItem, Context, Entity, FocusHandle, IntoElement, KeyBinding, MouseButton, PromptButton, PromptLevel, Render, Role,
    ScrollStrategy, SharedString, Subscription, Task, TestSupportExt, Window, div, prelude::*, relative, rems,
};

use crate::{
    appearance::{self, Appearance},
    config::{self, ConnectionConfig, ConnectionField, SavedConnections, TopicSubscription},
    mqtt::{self, BrokerEvent, Connection},
    topics::{FLASH_DURATION, TopicStore},
};

mod settings;

gpui_kit::actions!(
    mqtt_ui,
    [
        OpenConnection,
        CancelConnection,
        FocusTopics,
        FirstTopic,
        LastTopic,
        NarrowTopics,
        WidenTopics
    ]
);

pub(crate) fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-,", OpenConnection, Some("Explorer")),
        KeyBinding::new("escape", CancelConnection, Some("Explorer")),
        KeyBinding::new("secondary-1", FocusTopics, Some("Explorer")),
        KeyBinding::new("home", FirstTopic, Some("Tree")),
        KeyBinding::new("end", LastTopic, Some("Tree")),
        KeyBinding::new("enter", gpui_kit::base::actions::Confirm { secondary: false }, Some("Tree")),
        KeyBinding::new("ctrl-alt-left", NarrowTopics, Some("Explorer")),
        KeyBinding::new("ctrl-alt-right", WidenTopics, Some("Explorer")),
    ]);
}

enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Failed(String),
}

fn connection_label(config: &ConnectionConfig) -> String {
    if !config.name.trim().is_empty() {
        return config.name.clone();
    }
    let mut label = format!("{}:{}", config.host, config.port);
    if !config.username.is_empty() {
        label.push_str(&format!(" · {}", config.username));
    }
    if config.tls {
        label.push_str(" · TLS");
    }
    label
}

fn topic_summary(topics: usize, messages: u64) -> String {
    format!(
        "{topics} {} · {messages} {}",
        if topics == 1 { "topic" } else { "topics" },
        if messages == 1 { "message" } else { "messages" }
    )
}

impl ConnectionStatus {
    fn label(&self) -> &str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting to broker",
            Self::Connected => "Connected",
            Self::Failed(message) => message,
        }
    }

    fn is_connecting(&self) -> bool {
        matches!(self, Self::Connecting)
    }
    fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }
}

pub struct Explorer {
    name: Entity<InputState>,
    host: Entity<InputState>,
    port: Entity<InputState>,
    topic_input: Entity<InputState>,
    topic_qos: Entity<SelectState<Vec<&'static str>>>,
    subscription_topics: Vec<TopicSubscription>,
    topics_open: bool,
    topic_editor_open: bool,
    topic_editor_restore_focus: Option<FocusHandle>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    tls: bool,
    show_config: bool,
    active_config: Option<ConnectionConfig>,
    connection: Option<Connection>,
    status: ConnectionStatus,
    error: Option<String>,
    field_error: Option<(ConnectionField, String)>,
    saved_connections: SavedConnections,
    editing: Option<usize>,
    connection_form_open: bool,
    topics: TopicStore,
    tree_state: Entity<TreeState>,
    expanded: BTreeSet<String>,
    selected: Option<String>,
    payload: Entity<EditorState>,
    payload_format: &'static str,
    panes: Entity<ResizableState>,
    topics_width_rem: Option<f32>,
    topics_width_fraction: Option<f32>,
    restore_topics_width: bool,
    focus: FocusHandle,
    restore_focus: Option<FocusHandle>,
    settings_generation: u64,
    flash_until: Option<Instant>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Explorer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (saved_connections, error) = match config::load_connections() {
            Ok(connections) => (connections, None),
            Err(error) => (SavedConnections::default(), Some(format!("{error:#}"))),
        };
        let layout = config::load_topics_layout().unwrap_or_default();
        Self::with_connections(saved_connections, error, layout.width_rem, layout.width_fraction, window, cx)
    }

    #[cfg(test)]
    fn with_settings(
        saved_config: Option<ConnectionConfig>,
        error: Option<String>,
        width: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected = saved_config.as_ref().map(|_| 0);
        Self::with_connections(
            SavedConnections {
                connections: saved_config.into_iter().collect(),
                selected,
            },
            error,
            width,
            None,
            window,
            cx,
        )
    }

    fn with_connections(
        saved_connections: SavedConnections,
        error: Option<String>,
        width: Option<f32>,
        width_fraction: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if !cx.has_global::<Appearance>() {
            cx.set_global(Appearance::default());
        }
        let saved_config = saved_connections
            .selected
            .and_then(|index| saved_connections.connections.get(index))
            .cloned();
        let initial = saved_config.clone().unwrap_or_default();
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. Home").default_value(initial.name));
        let host = cx.new(|cx| InputState::new(window, cx).default_value(initial.host));
        let port = cx.new(|cx| InputState::new(window, cx).default_value(initial.port.to_string()));
        let topic_input = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. home/#"));
        let topic_qos = cx.new(|cx| SelectState::new(vec!["0", "1", "2"], Some(IndexPath::default()), window, cx));
        let username = cx.new(|cx| InputState::new(window, cx).placeholder("Optional").default_value(initial.username));
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Optional")
                .default_value(initial.password)
                .masked(true)
        });
        let payload = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("plaintext")
                .line_number(false)
                .indent_guides(false)
                .folding(false)
        });
        let tree_state = cx.new(|cx| TreeState::new(cx));
        // Kit 0.7 exposes focus through the tree state rather than a handle reader.
        // Capture its real handle once and enable the native Tab stop.
        tree_state.update(cx, |state, cx| state.focus(window, cx));
        let _tree_focus = window.focused(cx).map(|handle| handle.tab_stop(true));
        let panes = cx.new(|_| ResizableState::default());
        let mut subscriptions: Vec<_> = [&name, &host, &port, &username, &password]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |view, input, event, window, cx| match event {
                    InputEvent::Focus if view.show_config && view.connection_form_open && window.last_input_was_keyboard() => {
                        input.update(cx, |input, cx| input.select_all(window, cx));
                    }
                    InputEvent::PressEnter { .. } if view.show_config && view.connection_form_open => view.connect_from_form(window, cx),
                    InputEvent::Change if view.field_error.is_some() => {
                        view.field_error = None;
                        cx.notify();
                    }
                    _ => {}
                })
            })
            .collect();
        subscriptions.push(cx.subscribe_in(&topic_input, window, |view, input, event, window, cx| match event {
            InputEvent::Focus if view.topic_editor_open && window.last_input_was_keyboard() => {
                input.update(cx, |input, cx| input.select_all(window, cx));
            }
            InputEvent::PressEnter { .. } if view.topic_editor_open => view.add_topic_from_form(window, cx),
            InputEvent::Change if matches!(view.field_error, Some((ConnectionField::Topics, _))) => {
                view.field_error = None;
                cx.notify();
            }
            _ => {}
        }));
        subscriptions.push(cx.subscribe_in(&tree_state, window, |view, _, event, _, _| match event {
            TreeEvent::Expanded(path) => {
                view.expanded.insert(path.to_string());
            }
            TreeEvent::Collapsed(path) => {
                view.expanded.remove(path.as_str());
            }
        }));
        subscriptions.push(cx.observe_in(&tree_state, window, |view, state, window, cx| {
            let selected = state.read(cx).selected_item().map(|item| item.id.to_string());
            if view.selected != selected {
                view.selected = selected;
                view.refresh_details(window, cx);
                cx.notify();
            }
        }));
        subscriptions.push(cx.observe_window_appearance(window, |_, window, cx| {
            appearance::sync_system(window, cx);
        }));
        subscriptions.push(cx.subscribe_in(&panes, window, |view, state, _: &ResizablePanelEvent, window, cx| {
            view.persist_split(state, window, cx);
        }));
        let poll = cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(50)).await;
                if view.update_in(cx, |view, window, cx| view.poll(window, cx)).is_err() {
                    break;
                }
            }
        });
        let mut view = Self {
            name,
            host,
            port,
            topic_input,
            topic_qos,
            subscription_topics: initial.topics,
            topics_open: true,
            topic_editor_open: false,
            topic_editor_restore_focus: None,
            username,
            password,
            payload,
            payload_format: "Text",
            tree_state,
            panes,
            topics_width_fraction: width_fraction,
            restore_topics_width: width.is_some() || width_fraction.is_some(),
            tls: initial.tls,
            show_config: saved_config.is_none(),
            active_config: None,
            connection: None,
            status: ConnectionStatus::Disconnected,
            error,
            field_error: None,
            editing: saved_connections.selected,
            connection_form_open: saved_config.is_none(),
            saved_connections,
            topics: TopicStore::default(),
            expanded: BTreeSet::new(),
            selected: None,
            topics_width_rem: width,
            focus: cx.focus_handle(),
            restore_focus: None,
            settings_generation: 0,
            flash_until: None,
            _poll: poll,
            _subscriptions: subscriptions,
        };
        if let Some(config) = saved_config {
            view.start_connection(config, window, cx);
        } else {
            view.name.update(cx, |input, cx| input.focus(window, cx));
        }
        view
    }

    fn start_connection(&mut self, config: ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.connection = None;
        self.error = None;
        self.field_error = None;
        match mqtt::connect(config.clone()) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.active_config = Some(config);
                self.show_config = false;
                self.status = ConnectionStatus::Connecting;
                self.clear_topics(window, cx);
                self.focus_topics(window, cx);
            }
            Err(error) => {
                self.status = ConnectionStatus::Disconnected;
                self.error = Some(format!("Could not start the MQTT connection: {error:#}"));
            }
        }
        cx.notify();
    }

    fn clear_topics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.topics = TopicStore::default();
        self.expanded.clear();
        self.selected = None;
        self.flash_until = None;
        self.sync_tree(cx);
        self.refresh_details(window, cx);
    }

    fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut ended = false;
        let mut changed = false;
        let mut tree_changed = false;
        let mut payload_changed = false;
        for _ in 0..1000 {
            let Some(connection) = &mut self.connection else {
                break;
            };
            let event = match connection.events.try_recv() {
                Ok(event) => event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    ended = true;
                    break;
                }
            };
            changed = true;
            match event {
                BrokerEvent::Connecting => self.status = ConnectionStatus::Connecting,
                BrokerEvent::Connected => {
                    self.status = ConnectionStatus::Connected;
                }
                BrokerEvent::Status(status) => self.status = ConnectionStatus::Failed(status),
                BrokerEvent::OperationError(error) => self.error = Some(format!("Could not clear retained topics: {error}")),
                BrokerEvent::Message(message) => {
                    payload_changed |= self.selected.as_ref() == Some(&message.topic);
                    tree_changed |= self.topics.receive(message, now);
                    if !cx.reduce_motion() {
                        self.flash_until = Some(now + FLASH_DURATION);
                    }
                }
            }
        }
        if ended {
            self.connection = None;
            self.status = ConnectionStatus::Failed(format!("Connection stopped · {}", self.status.label()));
            changed = true;
        }
        if tree_changed {
            self.sync_tree(cx);
        }
        if payload_changed {
            self.refresh_details(window, cx);
        }
        if let Some(deadline) = self.flash_until {
            changed = true;
            if now >= deadline || cx.reduce_motion() {
                self.flash_until = None;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn sync_tree(&mut self, cx: &mut Context<Self>) {
        // Children sort after their prefix, so reverse order builds complete roots
        // without recursing through broker-controlled topic depth.
        let mut items = BTreeMap::new();
        for (path, node) in self.topics.nodes.iter().rev() {
            let children: Vec<_> = node.children.iter().filter_map(|child| items.remove(child)).collect();
            let label = if path.is_empty() { "(empty level)".into() } else { path.clone() };
            items.insert(
                path.clone(),
                TreeItem::new(path.clone(), label)
                    .children(children)
                    .expanded(self.expanded.contains(path)),
            );
        }
        let selected: Option<SharedString> = self.selected.clone().map(Into::into);
        self.tree_state.update(cx, |state, cx| {
            state.set_items(items.into_values().collect::<Vec<_>>(), cx);
            state.set_selected_index(selected.as_ref().and_then(|id| state.index_of(id)), cx);
        });
    }

    fn refresh_details(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let value = self
            .selected
            .as_ref()
            .and_then(|path| self.topics.nodes.get(path))
            .and_then(|node| node.value.as_ref());
        self.payload_format = value.map_or("Text", |value| value.format_label());
        let value = value.map(|value| value.display_payload()).unwrap_or_default();
        let language = if self.payload_format == "JSON" { "json" } else { "plaintext" };
        self.payload.update(cx, |input, cx| {
            if input.language_name() != language {
                input.set_highlighter(language, cx);
            }
            input.set_value(value, window, cx);
        });
    }

    fn select_topic(&mut self, path: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.topics.nodes.contains_key(path) {
            return;
        }
        for (index, _) in path.match_indices('/') {
            self.expanded.insert(path[..index].to_owned());
        }
        self.selected = Some(path.to_owned());
        self.sync_tree(cx);
        self.tree_state.update(cx, |state, cx| {
            if let Some(index) = state.index_of(&SharedString::from(path.to_owned())) {
                state.scroll_to_item(index, ScrollStrategy::Top);
            }
            state.focus(window, cx);
        });
        self.refresh_details(window, cx);
        cx.notify();
    }

    fn confirm_clear_topic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.selected.clone() else {
            return;
        };
        let Some(session) = self.connection.as_ref().and_then(Connection::retained_clear_session) else {
            self.error = Some("Connect to the broker before clearing retained topics.".into());
            cx.notify();
            return;
        };
        self.confirm_clear_topic_with_send(path, window, cx, move |view, topics| {
            let connection = view
                .connection
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("The broker connection has closed."))?;
            connection.clear_retained_in_session(topics, &session)
        });
    }

    fn confirm_clear_topic_with_send(
        &mut self,
        path: String,
        window: &mut Window,
        cx: &mut Context<Self>,
        send: impl FnOnce(&Self, Vec<String>) -> anyhow::Result<()> + 'static,
    ) {
        if !self.status.is_connected() {
            return;
        }
        // Freeze the exact scope shown in the warning; new topics need a new confirmation.
        let topics = self.topics.branch_paths(&path);
        if topics.is_empty() {
            return;
        }
        let children = topics.len() - usize::from(!path.is_empty());
        let scope = if path.is_empty() {
            format!("the {children} known topics under the empty topic level")
        } else {
            format!(
                "\"{path}\" and {children} known child {}",
                if children == 1 { "topic" } else { "topics" }
            )
        };
        let detail = format!(
            "Do you want to clear {scope}?\n\nThis will send an empty payload (QoS 0, retain) to each exact topic name in this branch, clearing retained messages on the broker. This cannot be undone and may affect other subscribers. Only use this function if you know what you are doing.\n\nOnly the topics known now are included. Live publishers may recreate them."
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            "Confirm delete",
            Some(&detail),
            &[PromptButton::cancel("Cancel"), PromptButton::new("Delete")],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            if answer.await == Ok(1) {
                _ = view.update_in(cx, |view, _, cx| {
                    let result = if view.status.is_connected() {
                        send(view, topics)
                    } else {
                        Err(anyhow::anyhow!("The broker disconnected. Confirm again while connected."))
                    };
                    view.error = result.err().map(|error| format!("Could not clear retained topics: {error:#}"));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn focus_topics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_config {
            return;
        }
        self.tree_state.update(cx, |state, cx| state.focus(window, cx));
    }

    fn select_boundary(&mut self, last: bool, cx: &mut Context<Self>) {
        let count = self.topics.visible_paths(&self.expanded).len();
        if count == 0 {
            return;
        }
        let ix = if last { count - 1 } else { 0 };
        self.tree_state.update(cx, |state, cx| {
            state.set_selected_index(Some(ix), cx);
            state.scroll_to_item(ix, if last { ScrollStrategy::Bottom } else { ScrollStrategy::Top });
        });
    }

    fn resize_topics(&mut self, wider: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_config {
            return;
        }
        let Some(current) = self.panes.read(cx).sizes().first().copied() else {
            return;
        };
        let delta = rems(if wider { 2. } else { -2. }).to_pixels(window.rem_size());
        self.panes
            .update(cx, |state, cx| state.resize_panel(0, current + delta, window, cx));
    }

    fn persist_split(&mut self, state: &Entity<ResizableState>, window: &Window, cx: &mut Context<Self>) {
        let state = state.read(cx);
        let Some(width) = state.sizes().first() else { return };
        let container_size = state.container_size();
        if container_size.as_f32() <= 0. {
            return;
        }
        let width_rem = *width / window.rem_size();
        let width_fraction = (*width / container_size).clamp(0., 1.);
        self.topics_width_rem = Some(width_rem);
        self.topics_width_fraction = Some(width_fraction);
        if let Err(error) = config::save_topics_width(width_rem, width_fraction) {
            self.error = Some(format!("Could not save the pane layout: {error:#}"));
        }
        cx.notify();
    }

    fn connection_menu(&self, broker: String, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        Button::new("connection-picker")
            .small()
            .ghost()
            .label(broker)
            .dropdown_caret(true)
            .dropdown_menu(move |menu, window, cx| {
                let mut menu = menu.min_w(rems(15.).to_pixels(window.rem_size()));
                let Some(view) = view.upgrade() else { return menu };
                let selected = view.read(cx).saved_connections.selected;
                let connections: Vec<_> = view
                    .read(cx)
                    .saved_connections
                    .connections
                    .iter()
                    .enumerate()
                    .map(|(index, config)| (index, connection_label(config)))
                    .collect();
                if connections.is_empty() {
                    menu = menu.label("No saved connections");
                }
                for (index, label) in connections {
                    let view = view.downgrade();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(selected == Some(index))
                            .on_click(move |_, window, cx| {
                                _ = view.update(cx, |view, cx| view.select_connection(index, window, cx));
                            }),
                    );
                }
                menu
            })
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let broker = self
            .saved_connections
            .selected
            .and_then(|index| self.saved_connections.connections.get(index))
            .map(connection_label)
            .unwrap_or_else(|| "Choose a connection".into());
        TitleBar::new().child(
            div()
                .id("toolbar")
                .test_support()
                .h_flex()
                .h_full()
                .w_full()
                .pr_2()
                .gap_2()
                .min_w_0()
                .child(div().flex_1())
                .child(self.connection_menu(broker, cx))
                .child(
                    Button::new("settings")
                        .small()
                        .ghost()
                        .icon(IconName::Settings)
                        .accessibility_label("Settings")
                        .tooltip(if cfg!(target_os = "macos") {
                            "Settings (Cmd+,)"
                        } else {
                            "Settings (Ctrl+,)"
                        })
                        .on_click(cx.listener(|view, _, window, cx| view.open_connection(window, cx))),
                ),
        )
    }

    fn topic_row(&self, entry: &TreeEntry, cx: &App) -> ListItem {
        let path = entry.item().id.as_str();
        let node = self.topics.nodes.get(path);
        let label = path
            .rsplit('/')
            .next()
            .filter(|level| !level.is_empty())
            .unwrap_or("(empty level)")
            .to_owned();
        let preview = node
            .map(|node| {
                node.value.as_ref().map_or_else(
                    || topic_summary(node.topics, node.messages),
                    |value| format!("= {}", value.preview()),
                )
            })
            .unwrap_or_default();
        let now = Instant::now();
        let highlight = if cx.reduce_motion() {
            0.
        } else {
            node.map_or(0., |node| node.flash_amount(now))
        };
        let state = self.tree_state.clone();
        ListItem::new(SharedString::from(format!("topic:{path}")))
            .accessibility_label(if path.is_empty() {
                "Empty topic level".to_owned()
            } else {
                path.to_owned()
            })
            .h_6()
            .text_sm()
            .text_color(cx.theme().foreground.blend(cx.theme().warning.opacity(highlight)))
            .rounded(cx.theme().radius_tokens().sm)
            .pl(rems(0.625 + entry.depth() as f32 * 0.875))
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                state.update(cx, |state, cx| state.focus(window, cx))
            })
            .child(
                div()
                    .h_flex()
                    .min_w_0()
                    .gap_1()
                    .child(div().w_4().flex_none().when(entry.is_folder(), |slot| {
                        slot.child(
                            Icon::new(if entry.is_expanded() {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .small()
                            .text_color(cx.theme().foreground),
                        )
                    }))
                    .child(div().min_w_0().truncate().font_medium().child(label))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(preview),
                    ),
            )
    }

    fn tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        div()
            .id("topics-pane")
            .test_support()
            .v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .id("topics-heading")
                    .test_support()
                    .h_flex()
                    .h_10()
                    .flex_none()
                    .px_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .font_medium()
                    .child("Topics"),
            )
            .when(self.topics.nodes.is_empty(), |tree| {
                tree.child(
                    div()
                        .v_flex()
                        .flex_1()
                        .p_4()
                        .gap_2()
                        .child(if self.status.is_connecting() {
                            "Connecting to broker"
                        } else if self.status.is_connected() {
                            "Waiting for messages"
                        } else {
                            "No topics received"
                        })
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(if self.status.is_connected() {
                                    "Topics appear as the broker sends messages."
                                } else {
                                    "Use Settings to check the broker settings."
                                }),
                        ),
                )
            })
            .when(!self.topics.nodes.is_empty(), |tree| {
                tree.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .border_1()
                        .border_color(cx.theme().sidebar)
                        .child(Tree::new(&self.tree_state, move |_, entry, _, window, cx| {
                            let Some(view) = view.upgrade() else {
                                return ListItem::new("closed-topic");
                            };
                            let view = view.read(cx);
                            if !cx.reduce_motion()
                                && view
                                    .topics
                                    .nodes
                                    .get(entry.item().id.as_str())
                                    .is_some_and(|node| node.flashing(Instant::now()))
                            {
                                window.request_animation_frame();
                            }
                            view.topic_row(entry, cx)
                        })),
                )
            })
    }

    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div().id("details-pane").test_support().v_flex().size_full().min_h_0().min_w_0();
        let Some(path) = &self.selected else {
            return panel.p_6().justify_center().items_center().child(
                div()
                    .id("empty-topic-selection")
                    .test_support()
                    .aria_label("Select a topic to inspect its latest value")
                    .text_color(cx.theme().muted_foreground)
                    .child("Select a topic to inspect its latest value"),
            );
        };
        let Some(node) = self.topics.nodes.get(path) else {
            return panel;
        };
        let mut breadcrumbs = div()
            .id("selected-topic")
            .test_support()
            .aria_label(path.clone())
            .h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .overflow_x_scroll();
        let mut end = 0;
        for (index, level) in path.split('/').enumerate() {
            if index > 0 {
                end += 1;
                breadcrumbs = breadcrumbs.child(div().flex_none().text_color(cx.theme().muted_foreground).child("/"));
            }
            end += level.len();
            let prefix = path[..end].to_owned();
            breadcrumbs = breadcrumbs.child(
                Button::new(SharedString::from(format!("topic-segment:{index}")))
                    .link()
                    .small()
                    .flex_none()
                    .font_medium()
                    .label(if level.is_empty() { "(empty level)" } else { level }.to_owned())
                    .accessibility_label(format!("Navigate to topic {prefix}"))
                    .tooltip(if prefix.is_empty() {
                        "(empty level)".to_owned()
                    } else {
                        prefix.clone()
                    })
                    .on_click(cx.listener(move |view, _, window, cx| view.select_topic(&prefix, window, cx))),
            );
        }
        let copy_path = path.clone();
        panel = panel.child(
            div()
                .id("details-heading")
                .test_support()
                .h_flex()
                .h_10()
                .flex_none()
                .px_4()
                .border_b_1()
                .border_color(cx.theme().border)
                .gap_3()
                .min_w_0()
                .child(breadcrumbs)
                .child(
                    Button::new("delete-topic")
                        .flex_none()
                        .ghost()
                        .small()
                        .icon(AssetIconName::Trash)
                        .accessibility_label("Delete topic and subtopics")
                        .tooltip("Clear retained topic and subtopics")
                        .disabled(!self.status.is_connected() || self.connection.is_none())
                        .on_click(cx.listener(|view, _, window, cx| view.confirm_clear_topic(window, cx))),
                )
                .child(
                    Button::new("copy-topic")
                        .flex_none()
                        .ghost()
                        .small()
                        .icon(IconName::Copy)
                        .accessibility_label("Copy topic name")
                        .on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()))),
                ),
        );
        let mut content = div().v_flex().p_4().gap_4().flex_1().min_h_0().min_w_0();
        if let Some(value) = &node.value {
            content = content
                .child(
                    div()
                        .v_flex()
                        .gap_2()
                        .child(
                            div()
                                .h_flex()
                                .id("topic-metadata")
                                .test_support()
                                .gap_2()
                                .flex_wrap()
                                .child(Tag::secondary().small().child(self.payload_format))
                                .child(Tag::secondary().small().child(format!("QoS {}", value.qos)))
                                .child(Tag::secondary().small().child(format!(
                                    "{} {}",
                                    value.payload.len(),
                                    if value.payload.len() == 1 { "byte" } else { "bytes" }
                                )))
                                .child(Tag::secondary().small().child(format!(
                                    "{} {}",
                                    value.messages,
                                    if value.messages == 1 { "message" } else { "messages" }
                                )))
                                .when(value.retained, |meta| meta.child(Tag::secondary().small().child("Retained"))),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Received {}", value.received_at.format("%Y-%m-%d %H:%M:%S%.3f"))),
                        ),
                )
                .child(
                    div()
                        .h_flex()
                        .flex_none()
                        .child(div().flex_1().font_medium().child("Latest value"))
                        .child(
                            Button::new("copy-value")
                                .ghost()
                                .small()
                                .icon(IconName::Copy)
                                .accessibility_label("Copy latest value")
                                .on_click(cx.listener(|view, _, _, cx| {
                                    if let Some(value) = view
                                        .selected
                                        .as_ref()
                                        .and_then(|path| view.topics.nodes.get(path))
                                        .and_then(|node| node.value.as_ref())
                                    {
                                        let text = std::str::from_utf8(&value.payload)
                                            .map(str::to_owned)
                                            .unwrap_or_else(|_| value.display_payload());
                                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    }
                                })),
                        ),
                )
                .child(
                    div().id("payload").test_support().flex_1().min_h_0().min_w_0().w_full().child(
                        Editor::new(&self.payload)
                            .readonly(true)
                            .h(relative(1.))
                            .w_full()
                            .text_sm()
                            .aria_label("Latest topic payload"),
                    ),
                )
                .when(value.payload.is_empty(), |content| {
                    content.child(div().text_sm().text_color(cx.theme().muted_foreground).child("Empty payload"))
                });
        } else {
            content = content
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("{} in this branch", topic_summary(node.topics, node.messages))),
                )
                .child("No message received on this exact topic.");
        }
        panel.child(content)
    }

    fn explorer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The resize API takes resolved pixels; derive all pane constraints from rem.
        let rem = window.rem_size();
        let width = self.topics_width_rem.unwrap_or(24.).clamp(15., 50.);
        let status = self.error.as_deref().unwrap_or_else(|| self.status.label()).to_owned();
        div()
            .v_flex()
            .size_full()
            .min_w_0()
            .child(self.header(cx))
            .child(
                div().flex_1().min_h_0().min_w_0().child(
                    h_resizable("panes")
                        .with_state(&self.panes)
                        .child(
                            resizable_panel()
                                .size(rems(width).to_pixels(rem))
                                .size_range(rems(15.).to_pixels(rem)..rems(50.).to_pixels(rem))
                                .child(self.tree(cx)),
                        )
                        .child(
                            resizable_panel()
                                .size_range(rems(20.).to_pixels(rem)..gpui_kit::Pixels::MAX)
                                .child(self.details(cx)),
                        ),
                ),
            )
            .child(
                div()
                    .id("status")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .h_flex()
                    .flex_none()
                    .min_h_8()
                    .px_4()
                    .py_2()
                    .gap_3()
                    .text_xs()
                    .min_w_0()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .when(self.status.is_connecting(), |bar| bar.child(Spinner::new().small()))
                    .when(!self.status.is_connecting(), |bar| {
                        bar.child(
                            div()
                                .size_1p5()
                                .flex_none()
                                .rounded(cx.theme().radius_full())
                                .bg(if self.status.is_connected() {
                                    cx.theme().success
                                } else {
                                    cx.theme().muted_foreground
                                }),
                        )
                    })
                    .child(div().flex_1().min_w_0().child(status))
                    .child(div().flex_none().text_color(cx.theme().muted_foreground).child("Readonly"))
                    .child(div().flex_none().child(topic_summary(self.topics.topics, self.topics.messages))),
            )
    }
}

impl Render for Explorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.restore_topics_width {
            self.restore_topics_width = false;
            let width_rem = self.topics_width_rem.unwrap_or(24.).clamp(15., 50.);
            let width_fraction = self.topics_width_fraction;
            let view = cx.weak_entity();
            window.on_next_frame(move |window, cx| {
                if let Some(view) = view.upgrade() {
                    let _ = view.update(cx, |view, cx| {
                        view.panes.update(cx, |state, cx| {
                            let width = width_fraction
                                .map(|fraction| state.container_size() * fraction)
                                .unwrap_or_else(|| rems(width_rem).to_pixels(window.rem_size()));
                            state.resize_panel(0, width, window, cx);
                        });
                    });
                }
            });
        }
        div()
            .id("explorer")
            .test_support()
            .key_context("Explorer")
            .track_focus(&self.focus)
            .on_action(cx.listener(|view, _: &OpenConnection, window, cx| view.open_connection(window, cx)))
            .on_action(cx.listener(|view, _: &CancelConnection, window, cx| view.cancel_connection(window, cx)))
            .on_action(cx.listener(|view, _: &FocusTopics, window, cx| view.focus_topics(window, cx)))
            .on_action(cx.listener(|view, _: &FirstTopic, _, cx| view.select_boundary(false, cx)))
            .on_action(cx.listener(|view, _: &LastTopic, _, cx| view.select_boundary(true, cx)))
            .on_action(cx.listener(|view, _: &NarrowTopics, window, cx| view.resize_topics(false, window, cx)))
            .on_action(cx.listener(|view, _: &WidenTopics, window, cx| view.resize_topics(true, window, cx)))
            .size_full()
            .text_sm()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.explorer(window, cx))
            .when(self.show_config, |root| root.child(self.settings_dialog(cx)))
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
