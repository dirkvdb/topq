//! Settings pages and broker management for the explorer.

use super::{ConnectionStatus, Explorer, connection_label};
use crate::{
    appearance::{self, Appearance},
    config::{self, ConnectionConfig, ConnectionField, SavedConnections},
};
use gpui_kit::assets::IconName as AssetIconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, IconName, Sizable, StyledExt, ThemeRegistry,
    button::{Button, ButtonVariants},
    checkbox::Checkbox,
    input::{Input, InputGroup, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    setting::{SettingGroup, SettingItem, SettingPage, Settings},
};
use gpui_kit::{
    Context, Entity, IntoElement, MouseButton, PromptButton, PromptLevel, Role, SharedString, TestSupportExt, WeakEntity, Window, div,
    prelude::*, relative, rems,
};

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
        let input = match field {
            ConnectionField::Name => &self.name,
            ConnectionField::Host => &self.host,
            ConnectionField::Port => &self.port,
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
        let config = ConnectionConfig {
            name,
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
        self.username
            .update(cx, |input, cx| input.set_value(config.username.clone(), window, cx));
        self.password
            .update(cx, |input, cx| input.set_value(config.password.clone(), window, cx));
        self.tls = config.tls;
        self.field_error = None;
        self.error = None;
        self.name.update(cx, |input, cx| input.focus(window, cx));
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
        SettingItem::render(move |_, _, cx| {
            div()
                .id(SharedString::from(format!("connection-field:{id}")))
                .test_support()
                .v_flex()
                .w_full()
                .p_1()
                .gap_1()
                .child(div().text_sm().font_medium().child(label))
                .child(
                    InputGroup::new(SharedString::from(format!("field:{id}")))
                        .w_full()
                        .invalid(error.is_some())
                        .input(Input::new(&input).id(id).aria_label(label)),
                )
                .when_some(error.clone(), |row, error| {
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
        })
        .keywords([label])
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
                let details = format!("{}:{} · {}", config.host, config.port, if config.tls { "TLS" } else { "No TLS" });
                (index, connection_label(config), details)
            })
            .collect();
        let mut keywords: Vec<_> = servers
            .iter()
            .flat_map(|(_, name, details)| [name.clone(), details.clone()])
            .collect();
        keywords.extend(["servers", "connections", "add", "edit", "delete", "connect"].map(str::to_owned));
        let list = SettingItem::render(move |_, _, cx| {
            div()
                .id("connections-overview")
                .test_support()
                .v_flex()
                .gap_4()
                .child(
                    div()
                        .h_flex()
                        .justify_end()
                        .child(Button::new("new-connection").primary().label("Add connection").on_click({
                            let view = view.clone();
                            move |_, window, cx| {
                                _ = view.update(cx, |view, cx| view.new_connection(window, cx));
                            }
                        })),
                )
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
        SettingPage::new("Connections").resettable(false).group(group)
    }

    fn connection_form_page(&self, cx: &mut Context<Self>) -> SettingPage {
        let view = cx.weak_entity();
        let editing = self.editing;
        let connecting = self.status.is_connecting();
        let tls = self.tls;
        let tls_item = SettingItem::render(move |_, _, _| {
            let view = view.clone();
            div().p_2().child(
                Checkbox::new("tls")
                    .label("Use TLS")
                    .checked(tls)
                    .on_change(move |checked, window, cx| {
                        _ = view.update(cx, |view, cx| {
                            view.tls = *checked;
                            let port = view.port.read(cx).value();
                            if port == "1883" && *checked || port == "8883" && !checked {
                                view.port
                                    .update(cx, |input, cx| input.set_value(if *checked { "8883" } else { "1883" }, window, cx));
                            }
                            cx.notify();
                        });
                    }),
            )
        })
        .keywords(["TLS"]);
        let header_view = cx.weak_entity();
        let header = SettingItem::render(move |_, _, cx| {
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
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("Broker details and optional credentials"),
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
                    row.child(Button::new("remove-connection").label("Remove").on_click({
                        let view = actions_view.clone();
                        move |_, window, cx| {
                            _ = view.update(cx, |view, cx| view.confirm_remove_connection(index, window, cx));
                        }
                    }))
                })
                .child(Button::new("save-connection").label("Save").on_click({
                    let view = actions_view.clone();
                    move |_, window, cx| {
                        _ = view.update(cx, |view, cx| view.save_from_form(window, cx));
                    }
                }))
                .child(
                    Button::new("connect")
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
        let mut group = SettingGroup::new().item(header).items([
            self.setting_input("name", "Connection name", &self.name, Some(ConnectionField::Name)),
            self.setting_input("host", "Host", &self.host, Some(ConnectionField::Host)),
            self.setting_input("port", "Port", &self.port, Some(ConnectionField::Port)),
            tls_item,
            self.setting_input("username", "Username", &self.username, Some(ConnectionField::Username)),
            self.setting_input("password", "Password", &self.password, None),
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
        div()
            .id("settings-dialog")
            .test_support()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background.opacity(0.8))
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
                                .pages([self.connections_page(cx), self.appearance_page(cx)]),
                        ),
                    ),
            )
    }
}
