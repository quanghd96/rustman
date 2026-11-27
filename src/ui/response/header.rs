//! The response header at the end of the response tab bar: colored status,
//! muted timing and size, then the body actions.

use std::sync::LazyLock;

use iced::widget::{Text, button, column, container, row, text, text_editor};
use iced::{Alignment, Background, Border, Color, Element, Length};

use crate::domain::response::{ConnectionDetails, HttpResponse};
use crate::message::{AppMsg, MenuId, Message, ResponseMsg};
use crate::state::response_view::{ConnectionField, ResponseView};
use crate::state::tabs::RequestTabState;
use crate::ui::components::icon_button::{IconSize, style as icon_style};
use crate::ui::components::menu::{self, Align};
use crate::ui::components::tooltip::shortcut;
use crate::ui::components::{field, icon_button, status_dot, tooltip, vsep};
use crate::ui::icons;
use crate::ui::theme::{
    CONTROL_SM, ICON_MD, INLINE_SEPARATOR, MONO, SPACE_LG, SPACE_MD, SPACE_XS, TEXT_MD, TEXT_SM,
    Tokens, UI_FONT_MEDIUM,
};
use crate::ui::tree_label::Face;

/// The gap between the header's muted items: one space of its text, so
/// "2 ms · 1.4 KB" reads as the one line it was before it was split up for
/// the tooltips.
static GAP: LazyLock<f32> = LazyLock::new(|| Face::Regular.advance(' ', TEXT_SM));
const DOT: &str = "·";

/// `None` until there is a response to describe.
pub fn trailing<'a>(
    tab: &'a RequestTabState,
    open: bool,
    t: &'a Tokens,
) -> Option<Element<'a, Message>> {
    let resp = tab.response.as_ref().filter(|r| r.error.is_none())?;
    let view = tab.response_view.as_ref()?;
    let status = status_dot::status_text(view.status.as_str(), t.status.success);
    let status = match &resp.connection {
        Some(connection) => connection_control(connection, &view.connection_fields, status, open, t),
        None => status,
    };
    Some(
        row![
            status,
            meta(view, t),
            vsep(t, INLINE_SEPARATOR),
            actions(resp, t),
        ]
        .spacing(SPACE_MD)
        .align_y(Alignment::Center)
        .into(),
    )
}

fn connection_control<'a>(
    connection: &'a ConnectionDetails,
    fields: &'a [ConnectionField],
    status: Element<'a, Message>,
    open: bool,
    t: &'a Tokens,
) -> Element<'a, Message> {
    let icon = match &connection.tls {
        Some(tls) if tls.verified == Some(true) => icons::lock().color(t.status.success),
        Some(tls) if tls.verified == Some(false) => icons::lock_open().color(t.status.error),
        Some(_) => icons::shield_alert().color(t.status.warning),
        None => icons::globe().color(t.text_muted),
    };
    let trigger = button(
        row![icon.size(ICON_MD), status]
            .spacing(SPACE_XS)
            .align_y(Alignment::Center),
    )
    .height(CONTROL_SM)
    .padding([0.0, SPACE_XS])
    .style(move |_, state| icon_style(t, state, open))
    .on_press(Message::App(AppMsg::MenuToggled(
        MenuId::ResponseConnection,
    )));
    let panel = open.then(|| connection_panel(fields, t));
    let trigger = menu::interactive_popover(
        trigger,
        panel,
        Align::End,
        Message::App(AppMsg::MenuDismissed),
    );
    if open {
        trigger
    } else {
        tooltip(t, trigger, "Connection details", None)
    }
}

fn connection_panel<'a>(fields: &'a [ConnectionField], t: &'a Tokens) -> Element<'a, Message> {
    let mut details = column![
        text("Connection details")
            .size(TEXT_MD)
            .font(UI_FONT_MEDIUM),
    ]
    .spacing(SPACE_MD);
    for (index, field) in fields.iter().enumerate() {
        details = details.push(
            row![
                text(field.label).size(TEXT_SM).color(t.text_muted).width(110.0),
                connection_value(index, &field.value, t),
            ]
            .spacing(SPACE_XS),
        );
    }
    container(details)
        .width(344.0)
        .padding(SPACE_LG)
        .style(move |_| menu::panel_style(t))
        .into()
}

fn connection_value<'a>(index: usize, value: &'a text_editor::Content, t: &'a Tokens) -> Element<'a, Message> {
    use text_editor::Binding;

    text_editor(value)
        .on_action(move |action| Message::Response(ResponseMsg::ConnectionFieldSelected(index, action)))
        .key_binding(|press| Binding::from_key_press(press).filter(|binding| {
            matches!(binding, Binding::Move(_) | Binding::Select(_) | Binding::SelectAll | Binding::Copy | Binding::Unfocus)
        }))
        .size(TEXT_SM)
        .font(MONO)
        .height(Length::Shrink)
        .padding(0)
        .wrapping(iced_core::text::Wrapping::WordOrGlyph)
        .style(move |_, _| text_editor::Style {
            background: Background::Color(Color::TRANSPARENT),
            border: Border::default(),
            placeholder: t.text_subtle,
            value: t.text,
            selection: field::selection(t),
        })
        .into()
}

/// "2 ms · 1.4 KB · ↪ 2 redirects", muted. Only the size and the redirects
/// have more to say on hover.
fn meta<'a>(view: &'a ResponseView, t: &'a Tokens) -> Element<'a, Message> {
    fn muted<'a>(s: &'a str, t: &Tokens) -> Text<'a> {
        text(s).size(TEXT_SM).color(t.text_muted)
    }
    let with_tip = |item: Text<'a>, tip: Option<&'a str>| -> Element<'a, Message> {
        match tip {
            Some(tip) => tooltip(t, item, tip, None),
            None => item.into(),
        }
    };
    let mut line = row![muted(&view.elapsed, t)].spacing(*GAP).align_y(Alignment::Center);
    if let Some(size) = &view.size {
        line = line.push(muted(DOT, t)).push(with_tip(muted(size, t), view.size_tip.as_deref()));
    }
    if let Some(redirects) = &view.redirects {
        line = line.push(muted(DOT, t)).push(with_tip(muted(&redirects.label, t), Some(&redirects.hops)));
    }
    line.into()
}

fn actions<'a>(resp: &HttpResponse, t: &'a Tokens) -> Element<'a, Message> {
    let action = |icon, msg: Option<ResponseMsg>, label: &'static str, keys: Option<String>| {
        tooltip(
            t,
            icon_button(t, icon, IconSize::Small).on_press_maybe(msg.map(Message::Response)),
            label,
            keys,
        )
    };
    let savable = !resp.body_stored || resp.binary_data.is_some();
    row![
        action(icons::search(), Some(ResponseMsg::SearchOpen), "Search", Some(shortcut("F"))),
        action(icons::import(), savable.then_some(ResponseMsg::SaveBody), "Save to file", None),
        action(icons::copy(), (!resp.is_binary).then_some(ResponseMsg::CopyBody), "Copy body", None),
    ]
    .spacing(SPACE_MD)
    .into()
}

#[cfg(test)]
mod tests {
    use iced::advanced::{clipboard, layout, mouse, renderer::Headless, widget::Tree, Clipboard, Layout, Shell};
    use iced::{Event, Point, Rectangle, Size, Vector, keyboard};

    use super::*;

    #[derive(Default)]
    struct TestClipboard(Option<String>);

    impl Clipboard for TestClipboard {
        fn read(&self, _: clipboard::Kind) -> Option<String> {
            self.0.clone()
        }

        fn write(&mut self, _: clipboard::Kind, value: String) {
            self.0 = Some(value);
        }
    }

    #[tokio::test]
    async fn drag_and_copy_a_connection_value_without_dismissing_the_panel() {
        let renderer = iced::Renderer::new(MONO, TEXT_SM.into(), Some("tiny-skia"))
            .await
            .unwrap();
        let dir = crate::test_support::TempDir::new("connection-popup");
        let mut state = crate::app::scratch_state(dir.path().to_path_buf());
        state.tabs.active_tab_mut().set_response(HttpResponse {
            status: 401,
            connection: Some(ConnectionDetails {
                http_version: "HTTP/1.1",
                local_addr: None,
                remote_addr: Some("127.0.0.1:443".parse().unwrap()),
                tls: None,
            }),
            ..HttpResponse::default()
        }, 1000);
        state.chrome.open_menu = Some(MenuId::ResponseConnection);
        let mut tree = Tree::empty();
        let mut clipboard = TestClipboard::default();
        let window = Size::new(900.0, 700.0);
        let viewport = Rectangle::new(Point::ORIGIN, window);
        let mut drive = |event: &dyn Fn(Point) -> Event, right: bool| {
            let t = Tokens::for_theme(0);
            let tab = state.tabs.active_tab();
            let connection = tab.response.as_ref().unwrap().connection.as_ref().unwrap();
            let fields = &tab.response_view.as_ref().unwrap().connection_fields;
            let mut header = connection_control(connection, fields, text("401 Unauthorized").into(), true, t);
            tree.diff(header.as_widget());
            let anchor = header.as_widget_mut().layout(&mut tree, &renderer, &layout::Limits::new(Size::ZERO, window));
            let mut overlay = header.as_widget_mut()
                .overlay(&mut tree, Layout::new(&anchor), &renderer, &viewport, Vector::ZERO)
                .unwrap();
            let panel = overlay.as_overlay_mut().layout(&renderer, window);
            let layout = Layout::new(&panel);
            let field = layout.child(0).child(3).child(1).bounds();
            let x = if right { field.x + field.width - 1.0 } else { field.x + 0.25 };
            let position = Point::new(x, field.center_y());
            let mut messages = Vec::new();
            overlay.as_overlay_mut().update(
                &event(position), layout, mouse::Cursor::Available(position), &renderer,
                &mut clipboard, &mut Shell::new(&mut messages),
            );
            drop(overlay);
            drop(header);
            for message in messages {
                let _ = crate::app::test_update(&mut state, message);
            }
            assert_eq!(state.chrome.open_menu, Some(MenuId::ResponseConnection));
        };
        drive(&|_| Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)), false);
        drive(&|position| Event::Mouse(mouse::Event::CursorMoved { position }), true);
        drive(&|_| Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)), true);
        drive(&|_| Event::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Character("c".into()),
            modified_key: keyboard::Key::Character("c".into()),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyC),
            location: keyboard::Location::Standard,
            modifiers: keyboard::Modifiers::COMMAND,
            text: None,
            repeat: false,
        }), true);
        assert_eq!(clipboard.0.as_deref(), Some("127.0.0.1:443"));
    }
}
