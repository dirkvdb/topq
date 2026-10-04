//! Settings pages and broker management for the explorer.

use super::{ConnectionStatus, Explorer, connection_label};
use crate::{
    appearance::{self, Appearance},
    config::{self, ConnectionConfig, ConnectionField, SavedConnections, TopicSubscription},
};
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, ThemeRegistry,
    button::{Button, ButtonVariants},
    collapsible::Collapsible,
    input::{Input, InputGroup, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    select::Select,
    setting::{SettingGroup, SettingItem, SettingPage, Settings},
    tag::Tag,
    tooltip::Tooltip,
};
use gpui_kit::{
    App, AvailableSpace, Bounds, Context, Element, ElementId, Entity, GlobalElementId, InspectorElementId, IntoElement, LayoutId,
    MouseButton, Pixels, PromptButton, PromptLevel, Role, SharedString, Style, TestSupportExt, WeakEntity, Window, div, prelude::*, px,
    relative, rems, size,
};

// The native header's title row shrink-wraps its suffix. A large preferred width with a zero
// minimum lets that row fill the page without overflowing or replacing its full-width divider.
struct HeaderSpacer;

impl IntoElement for HeaderSpacer {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for HeaderSpacer {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        _: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style {
            flex_grow: 1.,
            ..Style::default()
        };
        style.min_size.width = px(0.).into();
        let layout = window.request_measured_layout(style, |known, available, window, _| {
            let width = known.width.unwrap_or_else(|| match available.width {
                AvailableSpace::MinContent => px(0.),
                AvailableSpace::MaxContent => window.viewport_size().width,
                AvailableSpace::Definite(width) => width.max(px(0.)),
            });
            size(width, known.height.unwrap_or(px(0.)))
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }
}

impl Explorer {
    pub(super) fn select_connection(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.saved_connections.connections.get(index).cloned() else {
            return;
        };
        let connections = SavedConnections {
            connections: self.saved_connections.connections.clone(),
            selected: Some(index),
        };
        if let Err(error) = config::save_connections(&connections) {
            self.error = Some(format!("Could not select connection: {error:#}"));
            cx.notify();
            return;
        }
        self.saved_connections = connections;
        self.editing = Some(index);
        self.connection_form_open = false;
        self.start_connection(config, window, cx);
    }

    pub(super) fn open_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_config {
            self.settings_generation += 1;
            self.restore_focus = window.focused(cx);
            if let Some(config) = self
                .editing
                .and_then(|index| self.saved_connections.connections.get(index))
                .cloned()
            {
                self.set_form(&config, window, cx);
            }
        }
        self.show_config = true;
        self.connection_form_open = self.saved_connections.connections.is_empty();
        self.field_error = None;
        if self.connection_form_open {
            self.new_connection(window, cx);
        } else {
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    pub(super) fn cancel_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.show_config {
            cx.propagate();
            return;
        }
        if self.topic_editor_open {
            self.cancel_topic_edit(window, cx);
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

    fn invalid_field(&mut self, field: ConnectionField, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.field_error = Some((field, message));
        if field == ConnectionField::Topics {
            self.topics_open = true;
            self.topic_editor_open = true;
        }
        let input = match field {
            ConnectionField::Name => &self.name,
            ConnectionField::Host => &self.host,
            ConnectionField::Port => &self.port,
            ConnectionField::ClientId => &self.client_id,
            ConnectionField::Topics => &self.topic_input,
            ConnectionField::Username => &self.username,
        };
        input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    pub(super) fn connect_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.persist_form(true, window, cx);
    }

    fn save_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.persist_form(false, window, cx);
    }

    fn persist_form(&mut self, connect: bool, window: &mut Window, cx: &mut Context<Self>) {
        if connect && self.status.is_connecting() {
            return;
        }
        self.field_error = None;
        self.error = None;
        let port = match self.port.read(cx).value().trim().parse::<u16>() {
            Ok(port) => port,
            Err(_) => {
                self.invalid_field(ConnectionField::Port, "Enter a port between 1 and 65535.".into(), window, cx);
                return;
            }
        };
        let name = self.name.read(cx).value().trim().to_owned();
        if name.is_empty() {
            self.invalid_field(ConnectionField::Name, "Enter a connection name.".into(), window, cx);
            return;
        }
        if self.topic_editor_open {
            self.invalid_field(ConnectionField::Topics, "Add or cancel the topic before saving.".into(), window, cx);
            return;
        }
        let config = ConnectionConfig {
            name,
            host: self.host.read(cx).value().trim().to_owned(),
            port,
            client_id: self.client_id.read(cx).value().trim().to_owned(),
            topics: self.subscription_topics.clone(),
            username: self.username.read(cx).value().to_string(),
            password: self.password.read(cx).value().to_string(),
            tls: self.protocol.read(cx).selected_value() == Some(&"mqtts://"),
            websocket: self.protocol.read(cx).selected_value() == Some(&"ws://"),
        };
        if let Err(error) = config.validate() {
            self.invalid_field(error.field(), error.to_string(), window, cx);
            return;
        }
        let mut connections = SavedConnections {
            connections: self.saved_connections.connections.clone(),
            selected: self.saved_connections.selected,
        };
        let index = match self
            .editing
            .and_then(|index| connections.connections.get_mut(index).map(|slot| (index, slot)))
        {
            Some((index, slot)) => {
                *slot = config.clone();
                index
            }
            None => {
                connections.connections.push(config.clone());
                connections.connections.len() - 1
            }
        };
        if connect || connections.selected.is_none() {
            connections.selected = Some(index);
        }
        if let Err(error) = config::save_connections(&connections) {
            self.error = Some(format!("Could not save connection: {error:#}"));
            cx.notify();
            return;
        }
        self.saved_connections = connections;
        self.editing = Some(index);
        self.connection_form_open = false;
        if connect {
            self.start_connection(config, window, cx);
        } else {
            cx.notify();
        }
    }

    fn set_form(&mut self, config: &ConnectionConfig, window: &mut Window, cx: &mut Context<Self>) {
        self.name.update(cx, |input, cx| input.set_value(config.name.clone(), window, cx));
        self.host.update(cx, |input, cx| input.set_value(config.host.clone(), window, cx));
        self.port
            .update(cx, |input, cx| input.set_value(config.port.to_string(), window, cx));
        self.client_id
            .update(cx, |input, cx| input.set_value(config.client_id.clone(), window, cx));
        self.subscription_topics = config.topics.clone();
        self.topics_open = true;
        self.topic_editor_open = false;
        self.topic_editor_restore_focus = None;
        self.topic_input.update(cx, |input, cx| input.set_value("", window, cx));
        self.topic_qos.update(cx, |state, cx| state.set_selected_value(&"0", window, cx));
        self.username
            .update(cx, |input, cx| input.set_value(config.username.clone(), window, cx));
        self.password
            .update(cx, |input, cx| input.set_value(config.password.clone(), window, cx));
        self.protocol
            .update(cx, |state, cx| state.set_selected_value(&config.protocol(), window, cx));
        self.field_error = None;
        self.error = None;
        self.name.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn begin_topic_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.topics_open = true;
        if !self.topic_editor_open {
            self.topic_editor_restore_focus = window.focused(cx);
            self.topic_input.update(cx, |input, cx| input.set_value("", window, cx));
            self.topic_qos.update(cx, |state, cx| state.set_selected_value(&"0", window, cx));
            self.field_error = None;
        }
        self.topic_editor_open = true;
        self.topic_input.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn cancel_topic_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.topic_editor_open = false;
        self.field_error = None;
        if let Some(handle) = self.topic_editor_restore_focus.take() {
            handle.focus(window, cx);
        } else {
            self.username.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    pub(super) fn add_topic_from_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = TopicSubscription {
            topic: self.topic_input.read(cx).value().to_string(),
            qos: match self.topic_qos.read(cx).selected_value() {
                Some(&"1") => 1,
                Some(&"2") => 2,
                _ => 0,
            },
        };
        if let Err(error) = subscription.validate() {
            self.invalid_field(ConnectionField::Topics, error.to_string(), window, cx);
            return;
        }
        if self.subscription_topics.iter().any(|saved| saved.topic == subscription.topic) {
            self.invalid_field(
                ConnectionField::Topics,
                "This topic filter is already in the list.".into(),
                window,
                cx,
            );
            return;
        }
        self.subscription_topics.push(subscription);
        self.cancel_topic_edit(window, cx);
    }

    fn remove_subscription_topic(&mut self, topic: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.subscription_topics.retain(|subscription| subscription.topic != topic);
        self.field_error = None;
        if window.last_input_was_keyboard() {
            self.username.update(cx, |input, cx| input.focus(window, cx));
        }
        cx.notify();
    }

    fn new_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.connection_form_open = true;
        self.editing = None;
        self.set_form(&ConnectionConfig::default(), window, cx);
    }

    fn edit_connection(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let config = self.saved_connections.connections.get(index).cloned();
        if let Some(config) = config {
            self.connection_form_open = true;
            self.editing = Some(index);
            self.set_form(&config, window, cx);
        }
    }

    fn confirm_remove_connection(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(config) = self.saved_connections.connections.get(index) else {
            return;
        };
        let message = format!("Delete connection \"{}\"?", connection_label(config));
        let detail = if self.saved_connections.selected == Some(index) && self.active_config.is_some() {
            "This will remove the saved connection, disconnect from the broker, and clear its topics."
        } else {
            "This will remove the saved connection."
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            &message,
            Some(detail),
            &[PromptButton::cancel("Cancel"), PromptButton::new("Delete")],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            if answer.await == Ok(1) {
                _ = view.update_in(cx, |view, window, cx| {
                    view.remove_connection_with_save(index, window, cx, config::save_connections);
                });
            }
        })
        .detach();
    }

    pub(super) fn remove_connection_with_save(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
        save: impl FnOnce(&SavedConnections) -> anyhow::Result<()>,
    ) {
        let mut connections = SavedConnections {
            connections: self.saved_connections.connections.clone(),
            selected: self.saved_connections.selected,
        };
        if index >= connections.connections.len() {
            return;
        }
        connections.connections.remove(index);
        connections.selected = match connections.selected {
            Some(selected) if selected == index => (!connections.connections.is_empty()).then_some(0),
            Some(selected) if selected > index => Some(selected - 1),
            selected => selected,
        };
        if let Err(error) = save(&connections) {
            self.error = Some(format!("Could not remove connection: {error:#}"));
            cx.notify();
            return;
        }
        if self.saved_connections.selected == Some(index) {
            self.connection = None;
            self.active_config = None;
            self.status = ConnectionStatus::Disconnected;
            self.clear_topics(window, cx);
        }
        self.saved_connections = connections;
        self.connection_form_open = false;
        self.editing = self.saved_connections.selected;
        self.error = None;
        cx.notify();
    }

    fn setting_input(
        &self,
        id: &'static str,
        label: &'static str,
        input: &Entity<InputState>,
        field: Option<ConnectionField>,
    ) -> SettingItem {
        let input = input.clone();
        let error = self
            .field_error
            .as_ref()
            .filter(|(which, _)| Some(*which) == field)
            .map(|(_, message)| message.clone());
        SettingItem::render(move |_, _, cx| Self::connection_input(id, label, &input, error.clone(), cx).px_1().pb_1()).keywords([label])
    }

    fn connection_input(
        id: &'static str,
        label: &'static str,
        input: &Entity<InputState>,
        error: Option<String>,
        cx: &App,
    ) -> impl IntoElement + Styled + use<> {
        div()
            .id(SharedString::from(format!("connection-field:{id}")))
            .test_support()
            .v_flex()
            .w_full()
            .min_w_0()
            .gap_1()
            .child(div().text_sm().font_medium().child(label))
            .child(
                InputGroup::new(SharedString::from(format!("field:{id}")))
                    .w_full()
                    .invalid(error.is_some())
                    .input(Input::new(input).id(id).aria_label(label)),
            )
            .when_some(error, |row, error| {
                row.child(
                    div()
                        .id(SharedString::from(format!("{id}-error")))
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
    }

    fn broker_settings(&self) -> SettingItem {
        let protocol = self.protocol.clone();
        let host = self.host.clone();
        let port = self.port.clone();
        let field_error = self.field_error.clone();
        SettingItem::render(move |_, _, cx| {
            let error_for = |field| {
                field_error
                    .as_ref()
                    .filter(|(which, _)| *which == field)
                    .map(|(_, message)| message.clone())
            };
            div()
                .id("broker-settings")
                .test_support()
                .aria_label("Broker settings")
                .v_flex()
                .w_full()
                .px_1()
                .pb_1()
                .gap_2()
                .child(div().text_sm().font_semibold().child("Broker settings"))
                .child(
                    div()
                        .h_flex()
                        .items_start()
                        .w_full()
                        .gap_2()
                        .child(
                            div()
                                .v_flex()
                                .w_32()
                                .flex_none()
                                .gap_1()
                                .child(div().text_sm().font_medium().child("Protocol"))
                                .child(
                                    Select::new(&protocol)
                                        .id("broker-protocol")
                                        .w_full()
                                        .accessibility_label("Protocol"),
                                ),
                        )
                        .child(div().flex_1().min_w_0().child(Self::connection_input(
                            "host",
                            "Host",
                            &host,
                            error_for(ConnectionField::Host),
                            cx,
                        )))
                        .child(div().w_24().flex_none().child(Self::connection_input(
                            "port",
                            "Port",
                            &port,
                            error_for(ConnectionField::Port),
                            cx,
                        ))),
                )
        })
        .keywords(["Broker settings", "Protocol", "Host", "Port", "MQTT", "TLS", "WebSocket"])
    }

    fn connections_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.weak_entity();
        if self.connection_form_open {
            return self.connection_form_page(cx);
        }
        let selected = self.saved_connections.selected;
        let connected = self.status.is_connected();
        let connecting = self.status.is_connecting();
        let servers: Vec<_> = self
            .saved_connections
            .connections
            .iter()
            .enumerate()
            .map(|(index, config)| {
                let details = format!("{}{}:{}", config.protocol(), config.host, config.port);
                (index, connection_label(config), details)
            })
            .collect();
        let mut keywords: Vec<_> = servers
            .iter()
            .flat_map(|(_, name, details)| [name.clone(), details.clone()])
            .collect();
        keywords.extend(["servers", "connections", "add", "edit", "delete", "connect"].map(str::to_owned));
        let add_view = view.clone();
        let list = SettingItem::render(move |_, _, cx| {
            div()
                .id("connections-overview")
                .test_support()
                .v_flex()
                .gap_4()
                .when(servers.is_empty(), |list| {
                    list.child(
                        div()
                            .p_4()
                            .rounded_lg()
                            .border_1()
                            .border_color(cx.theme().border)
                            .text_color(cx.theme().muted_foreground)
                            .child("No connections yet. Add a broker to get started."),
                    )
                })
                .children(servers.iter().map(|(index, name, details)| {
                    let index = *index;
                    let current = selected == Some(index);
                    let edit_view = view.clone();
                    let connect_view = view.clone();
                    let delete_view = view.clone();
                    div()
                        .id(SharedString::from(format!("connection-card:{index}")))
                        .test_support()
                        .h_flex()
                        .flex_wrap()
                        .gap_3()
                        .items_center()
                        .w_full()
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().sidebar)
                        .child(
                            div()
                                .id(SharedString::from(format!("connection-icon:{index}")))
                                .test_support()
                                .flex_none()
                                .w_10()
                                .h_10()
                                .rounded_lg()
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().background)
                                .flex()
                                .items_center()
                                .justify_center()
                                .child(Icon::new(IconName::Network).text_color(cx.theme().primary)),
                        )
                        .child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w(rems(10.))
                                .h_10()
                                .justify_between()
                                .line_height(rems(1.25))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("connection-name:{index}")))
                                        .test_support()
                                        .font_semibold()
                                        .truncate()
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("connection-details:{index}")))
                                        .test_support()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .truncate()
                                        .child(details.clone()),
                                ),
                        )
                        .when(!current || !connected && !connecting, |card| {
                            card.child(
                                Button::new(SharedString::from(format!("select-connection:{index}")))
                                    .small()
                                    .label(if current { "Reconnect" } else { "Connect" })
                                    .on_click(move |_, window, cx| {
                                        _ = connect_view.update(cx, |view, cx| view.select_connection(index, window, cx));
                                    }),
                            )
                        })
                        .child(
                            Button::new(SharedString::from(format!("edit-connection:{index}")))
                                .small()
                                .outline()
                                .icon(AssetIconName::Pencil)
                                .accessibility_label("Edit connection")
                                .tooltip("Edit connection")
                                .on_click(move |_, window, cx| {
                                    _ = edit_view.update(cx, |view, cx| view.edit_connection(index, window, cx));
                                }),
                        )
                        .child(
                            Button::new(SharedString::from(format!("delete-connection:{index}")))
                                .small()
                                .ghost()
                                .icon(AssetIconName::Trash)
                                .accessibility_label("Delete connection")
                                .tooltip("Delete connection")
                                .on_click(move |_, window, cx| {
                                    _ = delete_view.update(cx, |view, cx| view.confirm_remove_connection(index, window, cx));
                                }),
                        )
                }))
        })
        .keywords(keywords);
        let mut group = SettingGroup::new().item(list);
        if let Some(error) = self.error.clone() {
            group = group.item(
                SettingItem::render(move |_, _, cx| {
                    div()
                        .id("connection-error")
                        .test_support()
                        .role(Role::Alert)
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone())
                })
                .keywords(["connection", "error"]),
            );
        }
        SettingPage::new("Connections")
            .resettable(false)
            .title_suffix(move |_, _| {
                div()
                    .id("connections-header-actions")
                    .test_support()
                    .h_flex()
                    .min_w_0()
                    .flex_auto()
                    .child(HeaderSpacer)
                    .child(
                        Button::new("new-connection")
                            .small()
                            .primary()
                            .flex_shrink_0()
                            .icon(IconName::Plus)
                            .accessibility_label("Add connection")
                            .tooltip("Add connection")
                            .on_click({
                                let view = add_view.clone();
                                move |_, window, cx| {
                                    _ = view.update(cx, |view, cx| view.new_connection(window, cx));
                                }
                            }),
                    )
            })
            .group(group)
    }

    fn subscription_topics_section(&self, cx: &mut Context<Self>) -> SettingItem {
        let view = cx.weak_entity();
        let topics = self.subscription_topics.clone();
        let open = self.topics_open;
        let editing = self.topic_editor_open;
        let input = self.topic_input.clone();
        let qos = self.topic_qos.clone();
        let error = self
            .field_error
            .as_ref()
            .filter(|(field, _)| *field == ConnectionField::Topics)
            .map(|(_, message)| message.clone());
        SettingItem::render(move |_, _, cx| {
            let mut content = div().v_flex().min_w_0().gap_2();
            if topics.is_empty() {
                content = content.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("No topics. Add a topic filter to subscribe."),
                );
            } else {
                for subscription in &topics {
                    let topic = subscription.topic.clone();
                    let tooltip_topic = topic.clone();
                    let remove_view = view.clone();
                    let remove_topic = topic.clone();
                    content = content.child(
                        div()
                            .id(SharedString::from(format!("subscription:{topic}")))
                            .test_support()
                            .h_flex()
                            .min_w_0()
                            .gap_2()
                            .px_1()
                            .py_1()
                            .text_sm()
                            .child(
                                div()
                                    .id(SharedString::from(format!("topic-filter:{topic}")))
                                    .test_support()
                                    .aria_label(topic.clone())
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .tooltip(move |window, cx| Tooltip::new(tooltip_topic.clone()).build(window, cx))
                                    .child(topic.clone()),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("topic-qos:{topic}")))
                                    .test_support()
                                    .aria_label(format!("QoS {}", subscription.qos))
                                    .flex_none()
                                    .child(Tag::secondary().small().child(format!("QoS {}", subscription.qos))),
                            )
                            .child(
                                Button::new(SharedString::from(format!("remove-topic:{topic}")))
                                    .small()
                                    .ghost()
                                    .icon(AssetIconName::Trash)
                                    .accessibility_label(format!("Remove topic {topic}"))
                                    .tooltip("Remove subscription")
                                    .on_click(move |_, window, cx| {
                                        _ = remove_view.update(cx, |view, cx| view.remove_subscription_topic(&remove_topic, window, cx));
                                    }),
                            ),
                    );
                }
            }
            if editing {
                let add_view = view.clone();
                let cancel_view = view.clone();
                content = content.child(
                    div()
                        .id("topic-editor")
                        .test_support()
                        .v_flex()
                        .gap_2()
                        .p_1()
                        .when(!topics.is_empty(), |editor| {
                            editor.border_t_1().border_color(cx.theme().border).pt_3()
                        })
                        .child(
                            div()
                                .h_flex()
                                .items_start()
                                .gap_2()
                                .child(
                                    div()
                                        .id("connection-field:topic-filter")
                                        .test_support()
                                        .v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(div().text_sm().font_medium().child("Topic"))
                                        .child(
                                            InputGroup::new("field:topic-filter")
                                                .small()
                                                .w_full()
                                                .invalid(error.is_some())
                                                .input(Input::new(&input).id("topic-filter").aria_label("Topic")),
                                        ),
                                )
                                .child(
                                    div()
                                        .v_flex()
                                        .w_16()
                                        .flex_none()
                                        .gap_1()
                                        .child(div().text_sm().font_medium().child("QoS"))
                                        .child(Select::new(&qos).id("topic-qos").small().w_full().accessibility_label("QoS")),
                                ),
                        )
                        .when_some(error.clone(), |editor, error| {
                            editor.child(
                                div()
                                    .id("topics-error")
                                    .test_support()
                                    .role(Role::Alert)
                                    .aria_label(error.clone())
                                    .text_sm()
                                    .text_color(cx.theme().danger)
                                    .child(error),
                            )
                        })
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .justify_end()
                                .child(
                                    Button::new("cancel-topic")
                                        .small()
                                        .ghost()
                                        .label("Cancel")
                                        .on_click(move |_, window, cx| {
                                            _ = cancel_view.update(cx, |view, cx| view.cancel_topic_edit(window, cx));
                                        }),
                                )
                                .child(Button::new("add-topic").small().label("Add").on_click(move |_, window, cx| {
                                    _ = add_view.update(cx, |view, cx| view.add_topic_from_form(window, cx));
                                })),
                        ),
                );
            }
            let toggle_view = view.clone();
            let add_view = view.clone();
            div().px_1().py_1().child(
                div()
                    .id("subscription-topics")
                    .test_support()
                    .aria_label("Topics")
                    .aria_expanded(open)
                    .w_full()
                    .min_w_0()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .p_2()
                    .child(
                        Collapsible::new()
                            .open(open)
                            .gap_2()
                            .w_full()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_2()
                                    .child(
                                        Button::new("toggle-topics")
                                            .small()
                                            .ghost()
                                            .flex_none()
                                            .font_semibold()
                                            .icon(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                            .label("Topics")
                                            .accessibility_label(if open { "Collapse topics" } else { "Expand topics" })
                                            .on_click(move |_, _, cx| {
                                                _ = toggle_view.update(cx, |view, cx| {
                                                    view.topics_open = !view.topics_open;
                                                    cx.notify();
                                                });
                                            }),
                                    )
                                    .child(div().flex_1())
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(topics.len().to_string()),
                                    )
                                    .child(
                                        Button::new("begin-add-topic")
                                            .small()
                                            .ghost()
                                            .icon(IconName::Plus)
                                            .accessibility_label("Add topic")
                                            .tooltip("Add topic")
                                            .on_click(move |_, window, cx| {
                                                _ = add_view.update(cx, |view, cx| view.begin_topic_edit(window, cx));
                                            }),
                                    ),
                            )
                            .content(content),
                    ),
            )
        })
        .keywords(["Topics", "subscription", "QoS"])
    }

    fn connection_form_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let editing = self.editing;
        let connecting = self.status.is_connecting();
        let header_view = cx.weak_entity();
        let header = SettingItem::render(move |_, _, _| {
            div()
                .v_flex()
                .px_1()
                .gap_3()
                .child(Button::new("back-to-connections").small().ghost().label("← Connections").on_click({
                    let view = header_view.clone();
                    move |_, _, cx| {
                        _ = view.update(cx, |view, cx| {
                            view.connection_form_open = false;
                            view.field_error = None;
                            view.error = None;
                            cx.notify();
                        });
                    }
                }))
                .child(
                    div()
                        .text_lg()
                        .font_semibold()
                        .child(if editing.is_some() { "Edit connection" } else { "New connection" }),
                )
        })
        .keywords(["connection", "server", "edit", "new"]);
        let actions_view = cx.weak_entity();
        let actions = SettingItem::render(move |_, _, _| {
            div()
                .id("connection-form-actions")
                .test_support()
                .h_flex()
                .w_full()
                .justify_end()
                .flex_wrap()
                .p_1()
                .gap_2()
                .when_some(editing, |row, index| {
                    row.child(Button::new("remove-connection").small().label("Remove").on_click({
                        let view = actions_view.clone();
                        move |_, window, cx| {
                            _ = view.update(cx, |view, cx| view.confirm_remove_connection(index, window, cx));
                        }
                    }))
                })
                .child(Button::new("save-connection").small().label("Save").on_click({
                    let view = actions_view.clone();
                    move |_, window, cx| {
                        _ = view.update(cx, |view, cx| view.save_from_form(window, cx));
                    }
                }))
                .child(
                    Button::new("connect")
                        .small()
                        .primary()
                        .label("Save & connect")
                        .loading(connecting)
                        .disabled(connecting)
                        .on_click({
                            let view = actions_view.clone();
                            move |_, window, cx| {
                                _ = view.update(cx, |view, cx| view.connect_from_form(window, cx));
                            }
                        }),
                )
        })
        .keywords(["save", "connect", "remove"]);
        let mut group = SettingGroup::new().gap_1().item(header).items([
            self.setting_input("name", "Connection name", &self.name, Some(ConnectionField::Name)),
            self.broker_settings(),
            self.setting_input("client-id", "Client ID", &self.client_id, Some(ConnectionField::ClientId)),
            self.setting_input("username", "Username", &self.username, Some(ConnectionField::Username)),
            self.setting_input("password", "Password", &self.password, None),
            self.subscription_topics_section(cx),
        ]);
        if let Some(error) = self.error.clone() {
            group = group.item(
                SettingItem::render(move |_, _, cx| {
                    div()
                        .id("connection-error")
                        .test_support()
                        .role(Role::Alert)
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error.clone())
                })
                .keywords(["connection", "error"]),
            );
        }
        SettingPage::new("Connections")
            .default_open(true)
            .resettable(false)
            .group(group.item(actions))
    }

    fn appearance_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.weak_entity();
        let label = Appearance::selected(cx).unwrap_or("Follow system (Ayu)").to_owned();
        SettingPage::new("Appearance").resettable(false).group(
            SettingGroup::new().item(
                SettingItem::render(move |_, _, _| {
                    div()
                        .h_flex()
                        .gap_3()
                        .child(div().font_semibold().child("Color theme"))
                        .child(Self::theme_menu(view.clone(), label.clone()))
                })
                .keywords(["theme", "appearance", "color"]),
            ),
        )
    }

    fn theme_menu(view: WeakEntity<Self>, label: String) -> impl IntoElement {
        Button::new("theme")
            .small()
            .outline()
            .label(label)
            .icon(IconName::ChevronDown)
            .dropdown_menu(move |menu, window, cx| {
                let system_view = view.clone();
                let mut menu = menu
                    .min_w(rems(14.).to_pixels(window.rem_size()))
                    .max_h(rems(24.).to_pixels(window.rem_size()))
                    .scrollable(true)
                    .item(
                        PopupMenuItem::new("Follow system (Ayu)")
                            .checked(Appearance::selected(cx).is_none())
                            .on_click(move |_, window, cx| {
                                _ = system_view.update(cx, |view, cx| view.select_theme(None, window, cx));
                            }),
                    );
                for dark in [false, true] {
                    menu = menu.separator().label(if dark { "Dark themes" } else { "Light themes" });
                    for theme in ThemeRegistry::global(cx).sorted_themes() {
                        if theme.mode.is_dark() != dark {
                            continue;
                        }
                        let name = theme.name.clone();
                        let selected = Appearance::selected(cx) == Some(name.as_str());
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(name.clone()).checked(selected).on_click(move |_, window, cx| {
                            _ = view.update(cx, |view, cx| view.select_theme(Some(name.clone()), window, cx));
                        }));
                    }
                }
                menu
            })
    }

    fn select_theme(&mut self, name: Option<SharedString>, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = appearance::select(name, window, cx) {
            self.error = Some(format!("{error:#}"));
        }
        cx.notify();
    }

    pub(super) fn settings_dialog(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut search_header = div().hidden();
        let search_header_style = search_header.style().clone();

        div()
            .id("settings-dialog")
            .test_support()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background.opacity(0.8))
            .occlude()
            .on_mouse_down(MouseButton::Left, |_, _, _| {})
            .child(
                div()
                    .v_flex()
                    .w(relative(0.94))
                    .h(relative(0.88))
                    .max_w(rems(64.))
                    .min_h_0()
                    .overflow_hidden()
                    .rounded_lg()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .flex_none()
                            .p_3()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(div().flex_1().text_lg().font_semibold().child("Settings"))
                            .child(
                                Button::new("close-settings")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Close)
                                    .accessibility_label("Close settings")
                                    .tooltip("Close settings")
                                    .on_click(cx.listener(|view, _, window, cx| view.cancel_connection(window, cx))),
                            ),
                    )
                    .child(
                        div().flex_1().min_h_0().child(
                            Settings::new(SharedString::from(format!("settings-pages:{}", self.settings_generation)))
                                .pages([self.connections_page(cx), self.appearance_page(cx)])
                                .header_style(&search_header_style),
                        ),
                    ),
            )
    }
}
