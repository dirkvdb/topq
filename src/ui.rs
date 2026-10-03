//! Connection configuration, live topic navigation, and selected-topic details.

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, ThemeRegistry,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    form::{Field, Form},
    input::{Editor, EditorState, Input, InputEvent, InputGroup, InputState},
    list::ListItem,
    menu::{DropdownMenu, PopupMenuItem},
    resizable::{ResizablePanelEvent, ResizableState, h_resizable, resizable_panel},
    scroll::ScrollableElement,
    spinner::Spinner,
    tag::Tag,
    tree::{Tree, TreeEntry, TreeEvent, TreeItem, TreeState},
};
use gpui_kit::{
    App, ClipboardItem, Context, Entity, FocusHandle, IntoElement, KeyBinding, MouseButton, Render,
    Role, ScrollStrategy, SharedString, Subscription, Task, TestSupportExt, Window, div,
    prelude::*, relative, rems,
};

use crate::{
    appearance::{self, Appearance},
    config::{self, ConnectionConfig, ConnectionField},
    mqtt::{self, BrokerEvent, Connection},
    topics::{FLASH_DURATION, TopicStore},
};

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
        KeyBinding::new(
            "enter",
            gpui_kit::base::actions::Confirm { secondary: false },
            Some("Tree"),
        ),
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
    host: Entity<InputState>,
    port: Entity<InputState>,
    username: Entity<InputState>,
    password: Entity<InputState>,
    tls: bool,
    show_config: bool,
    active_config: Option<ConnectionConfig>,
    connection: Option<Connection>,
    status: ConnectionStatus,
    error: Option<String>,
    field_error: Option<(ConnectionField, String)>,
    saved: bool,
    topics: TopicStore,
    tree_state: Entity<TreeState>,
    expanded: BTreeSet<String>,
    selected: Option<String>,
    topic_name: Entity<InputState>,
    payload: Entity<EditorState>,
    payload_format: &'static str,
    panes: Entity<ResizableState>,
    topics_width_rem: Option<f32>,
    focus: FocusHandle,
    restore_focus: Option<FocusHandle>,
    flash_until: Option<Instant>,
    _poll: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Explorer {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (saved_config, error) = match config::load() {
            Ok(config) => (config, None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        let width = config::load_topics_width().unwrap_or_default();
        Self::with_settings(saved_config, error, width, window, cx)
    }

    fn with_settings(
        saved_config: Option<ConnectionConfig>,
        error: Option<String>,
        width: Option<f32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if !cx.has_global::<Appearance>() {
            cx.set_global(Appearance::default());
        }
        let initial = saved_config.clone().unwrap_or_default();
        let host = cx.new(|cx| InputState::new(window, cx).default_value(initial.host));
        let port = cx.new(|cx| InputState::new(window, cx).default_value(initial.port.to_string()));
        let username = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Optional")
                .default_value(initial.username)
        });
        let password = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Optional")
                .default_value(initial.password)
                .masked(true)
        });
        let topic_name = cx.new(|cx| InputState::new(window, cx));
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
        let mut subscriptions: Vec<_> = [&host, &port, &username, &password]
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |view, _, event, window, cx| match event {
                    InputEvent::PressEnter { .. } if view.show_config => {
                        view.connect_from_form(window, cx)
                    }
                    InputEvent::Change if view.field_error.is_some() => {
                        view.field_error = None;
                        cx.notify();
                    }
                    _ => {}
                })
            })
            .collect();
        subscriptions.push(cx.subscribe_in(
            &tree_state,
            window,
            |view, _, event, _, _| match event {
                TreeEvent::Expanded(path) => {
                    view.expanded.insert(path.to_string());
                }
                TreeEvent::Collapsed(path) => {
                    view.expanded.remove(path.as_str());
                }
            },
        ));
        subscriptions.push(
            cx.observe_in(&tree_state, window, |view, state, window, cx| {
                let selected = state
                    .read(cx)
                    .selected_item()
                    .map(|item| item.id.to_string());
                if view.selected != selected {
                    view.selected = selected;
                    view.refresh_details(window, cx);
                    cx.notify();
                }
            }),
        );
        subscriptions.push(cx.observe_window_appearance(window, |_, window, cx| {
            appearance::sync_system(window, cx);
        }));
        subscriptions.push(cx.subscribe_in(
            &panes,
            window,
            |view, state, _: &ResizablePanelEvent, window, cx| {
                view.persist_split(state, window, cx);
            },
        ));
        let poll = cx.spawn_in(window, async move |view, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
                if view
                    .update_in(cx, |view, window, cx| view.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let mut view = Self {
            host,
            port,
            username,
            password,
            topic_name,
            payload,
            payload_format: "Text",
            tree_state,
            panes,
            tls: initial.tls,
            show_config: saved_config.is_none(),
            active_config: None,
            connection: None,
            status: ConnectionStatus::Disconnected,
            error,
            field_error: None,
            saved: saved_config.is_some(),
            topics: TopicStore::default(),
            expanded: BTreeSet::new(),
            selected: None,
            topics_width_rem: width,
            focus: cx.focus_handle(),
            restore_focus: None,
            flash_until: None,
            _poll: poll,
            _subscriptions: subscriptions,
        };
        if let Some(config) = saved_config {
            view.start_connection(config, window, cx);
        } else {
            view.host.update(cx, |input, cx| input.focus(window, cx));
        }
        view
    }

    fn invalid_field(
        &mut self,
        field: ConnectionField,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.field_error = Some((field, message));
        let input = match field {
            ConnectionField::Host => &self.host,
            ConnectionField::Port => &self.port,
            ConnectionField::Username => &self.username,
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn connect_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.is_connecting() {
            return;
        }
        self.field_error = None;
        let port = match self.port.read(cx).value().trim().parse::<u16>() {
            Ok(port) => port,
            Err(_) => {
                self.invalid_field(
                    ConnectionField::Port,
                    "Enter a port between 1 and 65535.".into(),
                    window,
                    cx,
                );
                return;
            }
        };
        let config = ConnectionConfig {
            host: self.host.read(cx).value().trim().to_owned(),
            port,
            username: self.username.read(cx).value().to_string(),
            password: self.password.read(cx).value().to_string(),
            tls: self.tls,
        };
        if let Err(error) = config.validate() {
            self.invalid_field(error.field(), error.to_string(), window, cx);
            return;
        }
        self.saved = false;
        self.start_connection(config, window, cx);
    }

    fn start_connection(
        &mut self,
        config: ConnectionConfig,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connection = None;
        self.error = None;
        self.field_error = None;
        match mqtt::connect(config.clone()) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.active_config = Some(config);
                self.show_config = false;
                self.status = ConnectionStatus::Connecting;
                self.topics = TopicStore::default();
                self.expanded.clear();
                self.selected = None;
                self.flash_until = None;
                self.sync_tree(cx);
                self.refresh_details(window, cx);
                self.focus_topics(window, cx);
            }
            Err(error) => {
                self.status = ConnectionStatus::Disconnected;
                self.error = Some(format!("Could not start the MQTT connection: {error:#}"));
            }
        }
        cx.notify();
    }

    fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.connection = None;
        self.status = ConnectionStatus::Disconnected;
        self.error = None;
        cx.notify();
    }

    fn open_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_config {
            self.restore_focus = window.focused(cx);
            if let Some(config) = &self.active_config {
                self.host.update(cx, |input, cx| {
                    input.set_value(config.host.clone(), window, cx)
                });
                self.port.update(cx, |input, cx| {
                    input.set_value(config.port.to_string(), window, cx)
                });
                self.username.update(cx, |input, cx| {
                    input.set_value(config.username.clone(), window, cx)
                });
                self.password.update(cx, |input, cx| {
                    input.set_value(config.password.clone(), window, cx)
                });
                self.tls = config.tls;
            }
        }
        self.show_config = true;
        self.field_error = None;
        self.host.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn cancel_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_config || self.active_config.is_none() {
            cx.propagate();
            return;
        }
        self.show_config = false;
        self.field_error = None;
        if let Some(handle) = self.restore_focus.take() {
            handle.focus(window, cx);
        } else {
            self.focus_topics(window, cx);
        }
        cx.notify();
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
                    if !self.saved
                        && let Some(config) = &self.active_config
                    {
                        match config::save(config) {
                            Ok(()) => self.saved = true,
                            Err(error) => {
                                self.error = Some(format!(
                                    "Connected, but could not save settings: {error:#}"
                                ))
                            }
                        }
                    }
                }
                BrokerEvent::Status(status) => self.status = ConnectionStatus::Failed(status),
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
            self.status =
                ConnectionStatus::Failed(format!("Connection stopped · {}", self.status.label()));
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
            let children: Vec<_> = node
                .children
                .iter()
                .filter_map(|child| items.remove(child))
                .collect();
            let label = if path.is_empty() {
                "(empty level)".into()
            } else {
                path.clone()
            };
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
        let topic = self.selected.clone().unwrap_or_default();
        self.topic_name
            .update(cx, |input, cx| input.set_value(topic, window, cx));
        let value = self
            .selected
            .as_ref()
            .and_then(|path| self.topics.nodes.get(path))
            .and_then(|node| node.value.as_ref());
        self.payload_format = value.map_or("Text", |value| value.format_label());
        let value = value
            .map(|value| value.display_payload())
            .unwrap_or_default();
        let language = if self.payload_format == "JSON" {
            "json"
        } else {
            "plaintext"
        };
        self.payload.update(cx, |input, cx| {
            if input.language_name() != language {
                input.set_highlighter(language, cx);
            }
            input.set_value(value, window, cx);
        });
    }

    fn focus_topics(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.show_config {
            return;
        }
        self.tree_state
            .update(cx, |state, cx| state.focus(window, cx));
    }

    fn select_boundary(&mut self, last: bool, cx: &mut Context<Self>) {
        let count = self.topics.visible_paths(&self.expanded).len();
        if count == 0 {
            return;
        }
        let ix = if last { count - 1 } else { 0 };
        self.tree_state.update(cx, |state, cx| {
            state.set_selected_index(Some(ix), cx);
            state.scroll_to_item(
                ix,
                if last {
                    ScrollStrategy::Bottom
                } else {
                    ScrollStrategy::Top
                },
            );
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
        self.panes.update(cx, |state, cx| {
            state.resize_panel(0, current + delta, window, cx)
        });
    }

    fn persist_split(
        &mut self,
        state: &Entity<ResizableState>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(width) = state.read(cx).sizes().first() {
            let width = *width / window.rem_size();
            self.topics_width_rem = Some(width);
            if let Err(error) = config::save_topics_width(width) {
                self.error = Some(format!("Could not save the pane layout: {error:#}"));
            }
            cx.notify();
        }
    }

    fn field(
        &self,
        id: &'static str,
        label: &'static str,
        input: &Entity<InputState>,
        field: Option<ConnectionField>,
        cx: &Context<Self>,
    ) -> Field {
        let error = self
            .field_error
            .as_ref()
            .filter(|(which, _)| Some(*which) == field)
            .map(|(_, message)| message.clone());
        let danger = cx.theme().danger;
        Field::new()
            .label(label)
            .child(
                InputGroup::new(SharedString::from(format!("field:{id}")))
                    .invalid(error.is_some())
                    .input(Input::new(input).id(id).aria_label(label)),
            )
            .when_some(error, |field, error| {
                field.description_fn(move |_, _| {
                    div()
                        .id(SharedString::from(format!("{id}-error")))
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_sm()
                        .text_color(danger)
                        .child(error.clone())
                })
            })
    }

    fn connection_form(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let connecting = self.status.is_connecting();
        let content = div().size_full().v_flex()
            .child(div().v_flex().min_h_full().items_center().justify_center().p_8()
                .child(div().v_flex().w_full().max_w(rems(28.)).gap_6()
                    .child(div().v_flex().gap_2()
                        .child(div().text_xl().font_semibold().child("Connect to a broker"))
                        .child(div().text_sm().text_color(cx.theme().muted_foreground).child("Your last successful connection opens automatically next time.")))
                    .child(Form::new()
                        .child(self.field("host", "Host", &self.host, Some(ConnectionField::Host), cx))
                        .child(self.field("port", "Port", &self.port, Some(ConnectionField::Port), cx))
                        .child(Field::new().label_indent(false).child(
                            Checkbox::new("tls").label("Use TLS").checked(self.tls).on_change(cx.listener(|view, checked, window, cx| {
                                view.tls = *checked;
                                let port = view.port.read(cx).value();
                                if port == "1883" && *checked || port == "8883" && !checked {
                                    view.port.update(cx, |input, cx| input.set_value(if *checked { "8883" } else { "1883" }, window, cx));
                                }
                                cx.notify();
                            }))))
                        .child(self.field("username", "Username", &self.username, Some(ConnectionField::Username), cx))
                        .child(self.field("password", "Password", &self.password, None, cx))
                        .footer(div().h_flex().gap_2()
                            .when(self.active_config.is_some(), |buttons| buttons.child(
                                Button::new("cancel").label("Cancel").on_click(cx.listener(|view, _, window, cx| view.cancel_connection(window, cx)))))
                            .child(Button::new("connect").primary().label("Connect").loading(connecting).disabled(connecting)
                                .on_click(cx.listener(|view, _, window, cx| view.connect_from_form(window, cx))))))
                    .when_some(self.error.clone(), |form, error| form.child(
                        div().text_sm().text_color(cx.theme().danger).child(error)))
                    .when(connecting, |form| form.child(div().h_flex().gap_2().text_sm()
                        .child(Spinner::new().small()).child("Connecting to broker")))))
            .overflow_y_scrollbar();
        div()
            .id("connection-form")
            .test_support()
            .size_full()
            .child(content)
    }

    fn theme_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.weak_entity();
        Button::new("theme")
            .small()
            .outline()
            .label("Theme")
            .icon(IconName::ChevronDown)
            .dropdown_menu(move |menu, window, cx| {
                let system_view = view.clone();
                let mut menu = menu
                    // PopupMenu sizing takes resolved pixels rather than rems.
                    .min_w(rems(14.).to_pixels(window.rem_size()))
                    .max_h(rems(24.).to_pixels(window.rem_size()))
                    .scrollable(true)
                    .item(
                        PopupMenuItem::new("Follow system (Ayu)")
                            .checked(Appearance::selected(cx).is_none())
                            .on_click(move |_, window, cx| {
                                _ = system_view
                                    .update(cx, |view, cx| view.select_theme(None, window, cx));
                            }),
                    );
                for dark in [false, true] {
                    menu =
                        menu.separator()
                            .label(if dark { "Dark themes" } else { "Light themes" });
                    for theme in ThemeRegistry::global(cx).sorted_themes() {
                        if theme.mode.is_dark() != dark {
                            continue;
                        }
                        let name = theme.name.clone();
                        let selected = Appearance::selected(cx) == Some(name.as_str());
                        let view = view.clone();
                        menu =
                            menu.item(PopupMenuItem::new(name.clone()).checked(selected).on_click(
                                move |_, window, cx| {
                                    _ = view.update(cx, |view, cx| {
                                        view.select_theme(Some(name.clone()), window, cx)
                                    });
                                },
                            ));
                    }
                }
                menu
            })
    }

    fn select_theme(
        &mut self,
        name: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = appearance::select(name, window, cx) {
            self.error = Some(format!("{error:#}"));
        }
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let broker = self
            .active_config
            .as_ref()
            .map(|config| format!("{}:{}", config.host, config.port))
            .unwrap_or_default();
        div()
            .id("toolbar")
            .test_support()
            .h_flex()
            .flex_none()
            .h_12()
            .px_4()
            .gap_3()
            .min_w_0()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().flex_none().font_semibold().child("MQTT UI"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().muted_foreground)
                    .child(broker),
            )
            .child(self.theme_menu(cx))
            .when(self.connection.is_some(), |header| {
                header.child(
                    Button::new("disconnect")
                        .small()
                        .label("Disconnect")
                        .on_click(cx.listener(|view, _, _, cx| view.disconnect(cx))),
                )
            })
            .when(self.connection.is_none(), |header| {
                header.child(
                    Button::new("reconnect")
                        .small()
                        .label("Reconnect")
                        .on_click(cx.listener(|view, _, window, cx| {
                            if let Some(config) = view.active_config.clone() {
                                view.start_connection(config, window, cx);
                            }
                        })),
                )
            })
            .child(
                Button::new("settings")
                    .small()
                    .outline()
                    .label("Connection…")
                    .tooltip(if cfg!(target_os = "macos") {
                        "Connection settings (Cmd+,)"
                    } else {
                        "Connection settings (Ctrl+,)"
                    })
                    .on_click(cx.listener(|view, _, window, cx| view.open_connection(window, cx))),
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
            .h_7()
            .text_sm()
            .text_color(
                cx.theme()
                    .foreground
                    .blend(cx.theme().warning.opacity(highlight)),
            )
            .rounded(cx.theme().radius_tokens().sm)
            .pl(rems(0.75 + entry.depth() as f32))
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
                                    "Use Connection… to check the broker settings."
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
                        .child(Tree::new(
                            &self.tree_state,
                            move |_, entry, _, window, cx| {
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
                            },
                        )),
                )
            })
    }

    fn details(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .id("details-pane")
            .test_support()
            .v_flex()
            .size_full()
            .min_h_0()
            .min_w_0();
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
                .child(div().flex_1().font_medium().child("Topic"))
                .child(
                    Button::new("copy-topic")
                        .ghost()
                        .small()
                        .icon(IconName::Copy)
                        .accessibility_label("Copy topic name")
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()))
                        }),
                ),
        );
        let mut content = div()
            .v_flex()
            .p_4()
            .gap_4()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .child(
                Input::new(&self.topic_name)
                    .id("selected-topic")
                    .aria_label("Selected topic")
                    .readonly(true)
                    .font_family("monospace")
                    // Keep the full text line inside the input's padding and border.
                    .h_auto()
                    .flex_none()
                    .w_full(),
            );
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
                                    if value.payload.len() == 1 {
                                        "byte"
                                    } else {
                                        "bytes"
                                    }
                                )))
                                .child(Tag::secondary().small().child(format!(
                                    "{} {}",
                                    value.messages,
                                    if value.messages == 1 {
                                        "message"
                                    } else {
                                        "messages"
                                    }
                                )))
                                .when(value.retained, |meta| {
                                    meta.child(Tag::secondary().small().child("Retained"))
                                }),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!(
                                    "Received {}",
                                    value.received_at.format("%Y-%m-%d %H:%M:%S%.3f")
                                )),
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
                    div()
                        .id("payload")
                        .test_support()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .w_full()
                        .child(
                            Editor::new(&self.payload)
                                .readonly(true)
                                .h(relative(1.))
                                .w_full()
                                .text_sm()
                                .aria_label("Latest topic payload"),
                        ),
                )
                .when(value.payload.is_empty(), |content| {
                    content.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("Empty payload"),
                    )
                });
        } else {
            content = content
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "{} in this branch",
                            topic_summary(node.topics, node.messages)
                        )),
                )
                .child("No message received on this exact topic.");
        }
        panel.child(content)
    }

    fn explorer(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The resize API takes resolved pixels; derive all pane constraints from rem.
        let rem = window.rem_size();
        let width = self.topics_width_rem.unwrap_or(24.).clamp(15., 50.);
        let status = self
            .error
            .as_deref()
            .unwrap_or_else(|| self.status.label())
            .to_owned();
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
                    .when(self.status.is_connecting(), |bar| {
                        bar.child(Spinner::new().small())
                    })
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
                    .child(
                        div()
                            .flex_none()
                            .text_color(cx.theme().muted_foreground)
                            .child("Readonly"),
                    )
                    .child(
                        div()
                            .flex_none()
                            .child(topic_summary(self.topics.topics, self.topics.messages)),
                    ),
            )
    }
}

impl Render for Explorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("explorer")
            .test_support()
            .key_context("Explorer")
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|view, _: &OpenConnection, window, cx| {
                    view.open_connection(window, cx)
                }),
            )
            .on_action(cx.listener(|view, _: &CancelConnection, window, cx| {
                view.cancel_connection(window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &FocusTopics, window, cx| view.focus_topics(window, cx)),
            )
            .on_action(cx.listener(|view, _: &FirstTopic, _, cx| view.select_boundary(false, cx)))
            .on_action(cx.listener(|view, _: &LastTopic, _, cx| view.select_boundary(true, cx)))
            .on_action(cx.listener(|view, _: &NarrowTopics, window, cx| {
                view.resize_topics(false, window, cx)
            }))
            .on_action(
                cx.listener(|view, _: &WidenTopics, window, cx| {
                    view.resize_topics(true, window, cx)
                }),
            )
            .size_full()
            .text_sm()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(if self.show_config {
                div()
                    .v_flex()
                    .size_full()
                    .when(self.active_config.is_none(), |screen| {
                        screen.child(
                            div()
                                .h_flex()
                                .h_12()
                                .flex_none()
                                .px_4()
                                .justify_end()
                                .child(self.theme_menu(cx)),
                        )
                    })
                    .when(self.active_config.is_some(), |screen| {
                        screen.child(self.header(cx))
                    })
                    .child(div().flex_1().min_h_0().child(self.connection_form(cx)))
                    .into_any_element()
            } else {
                self.explorer(window, cx).into_any_element()
            })
    }
}

#[cfg(test)]
#[path = "ui_tests.rs"]
mod tests;
