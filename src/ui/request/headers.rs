//! The Headers tab: a key-value table, with value suggestions on a
//! Content-Type row.

use iced::Element;

use crate::message::{AppMsg, MenuId, Message, RequestMsg};
use crate::state::tabs::RequestTabState;
use crate::ui::components::icon_button::{IconSize, icon_button_toggled};
use crate::ui::components::kv_table::{Actions, KeySuggestions, Row, kv_table};
use crate::ui::components::menu::{self, Align, Entry, Item};
use crate::ui::components::scroll;
use crate::ui::icons;
use crate::ui::theme::{TEXT_SM, Tokens};
use crate::ui::widgets::kv_table as bulk;

const CONTENT_TYPE_SUGGESTIONS: &[&str] = &[
    "application/json",
    "application/xml",
    "text/plain",
    "text/html",
    "text/css",
    "text/javascript",
    "application/javascript",
    "application/x-www-form-urlencoded",
    "multipart/form-data",
    "application/octet-stream",
    "application/pdf",
    "application/zip",
    "image/png",
    "image/jpeg",
    "image/gif",
    "image/svg+xml",
    "audio/mpeg",
    "video/mp4",
    "application/graphql-response+json",
    "application/ld+json",
    "application/merge-patch+json",
    "application/problem+json",
    "application/vnd.api+json",
    "application/yaml",
    "text/csv",
    "text/xml",
    "application/grpc",
    "application/x-protobuf",
    "application/x-ndjson",
];

const HELP: &str =
    "No custom headers. Content-Type comes from the body type, and auth headers from the Auth tab.";
/// Width of the suggestions menu.
const MENU_WIDTH: f32 = 320.0;
const NAME_SUGGESTIONS_LIMIT: usize = 6;

const HEADER_NAMES: &[&str] = &[
    "Accept",
    "Accept-Charset",
    "Accept-Encoding",
    "Accept-Language",
    "Access-Control-Request-Headers",
    "Access-Control-Request-Method",
    "Authorization",
    "Cache-Control",
    "Connection",
    "Content-Disposition",
    "Content-Encoding",
    "Content-Language",
    "Content-Length",
    "Content-MD5",
    "Content-Range",
    "Content-Type",
    "Cookie",
    "Date",
    "Expect",
    "Forwarded",
    "From",
    "Host",
    "Idempotency-Key",
    "If-Match",
    "If-Modified-Since",
    "If-None-Match",
    "If-Range",
    "If-Unmodified-Since",
    "Max-Forwards",
    "Origin",
    "Pragma",
    "Prefer",
    "Proxy-Authorization",
    "Range",
    "Referer",
    "Sec-Fetch-Dest",
    "Sec-Fetch-Mode",
    "Sec-Fetch-Site",
    "Sec-Fetch-User",
    "SOAPAction",
    "TE",
    "Traceparent",
    "Tracestate",
    "Trailer",
    "Transfer-Encoding",
    "Upgrade",
    "User-Agent",
    "Via",
    "Warning",
    "X-API-Key",
    "X-Correlation-ID",
    "X-CSRF-Token",
    "X-Forwarded-For",
    "X-Forwarded-Host",
    "X-Forwarded-Proto",
    "X-Request-ID",
    "X-Requested-With",
    "X-XSRF-Token",
];

pub(crate) fn name_suggestions(query: &str) -> impl Iterator<Item = &'static str> + '_ {
    let query = query.trim();
    HEADER_NAMES
        .iter()
        .copied()
        .filter(move |name| {
            !query.is_empty()
                && name
                    .get(..query.len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case(query))
        })
        .take(NAME_SUGGESTIONS_LIMIT)
}

const ACTIONS: Actions<Message> = Actions {
    toggle: |i| Message::Request(RequestMsg::HeaderToggled(i)),
    key: |i, s| Message::Request(RequestMsg::HeaderKeyChanged(i, s)),
    value: |i, s| Message::Request(RequestMsg::HeaderValueChanged(i, s)),
    remove: |i| Message::Request(RequestMsg::HeaderRemoved(i)),
    append: |field, s| Message::Request(RequestMsg::HeaderAppended(field, s)),
};

pub fn view<'a>(
    tab: &'a RequestTabState,
    open_menu: Option<MenuId>,
    t: &'a Tokens,
) -> Element<'a, Message> {
    if let Some(content) = &tab.headers_bulk {
        return bulk::bulk_panel(
            content,
            |a| Message::Request(RequestMsg::HeadersBulkEdited(a)),
            Message::Request(RequestMsg::HeadersBulkToggle),
        );
    }
    let rows =
        tab.headers
            .iter()
            .enumerate()
            .map(|(i, h)| Row {
                id: h.id.as_str(),
                enabled: h.enabled,
                key: h.key.as_str(),
                value: h.value.as_str(),
                key_suggestions: Some(header_name_suggestions(
                    &h.id,
                    &h.key,
                    open_menu == Some(MenuId::HeaderNameSuggestions(i)),
                    t,
                )),
                suffix: h.key.eq_ignore_ascii_case("content-type").then(|| {
                    suggestions(i, &h.id, open_menu == Some(MenuId::HeaderSuggestions(i)), t)
                }),
            })
            .collect();
    scroll::vertical(t, kv_table(t, rows, ACTIONS, "Add header", Some(HELP))).into()
}

fn header_name_suggestions<'a>(
    id: &str,
    key: &str,
    open: bool,
    t: &'a Tokens,
) -> KeySuggestions<'a, Message> {
    let mut choices = name_suggestions(if open { key } else { "" }).peekable();
    let select = |name| Message::Request(RequestMsg::HeaderNameSelected(id.to_owned(), name));
    let submit = choices.peek().copied().map(select);
    let panel = submit.is_some().then(|| {
        let entries = choices
            .map(|name| Item::new(name, select(name)).into())
            .collect();
        menu::panel(t, entries, MENU_WIDTH)
    });
    KeySuggestions {
        panel,
        submit,
        dismiss: Message::App(AppMsg::MenuDismissed),
    }
}

fn suggestions<'a>(row: usize, id: &str, open: bool, t: &'a Tokens) -> Element<'a, Message> {
    let trigger = icon_button_toggled(
        t,
        icons::chevron_down().size(TEXT_SM),
        IconSize::Small,
        open,
    )
    .on_press(Message::App(AppMsg::MenuToggled(
        MenuId::HeaderSuggestions(row),
    )));
    let panel = open.then(|| {
        let entries: Vec<Entry<'_, Message>> = CONTENT_TYPE_SUGGESTIONS
            .iter()
            .map(|s| {
                Item::new(
                    *s,
                    Message::Request(RequestMsg::HeaderValueChanged(
                        id.to_owned(),
                        (*s).to_owned(),
                    )),
                )
                .into()
            })
            .collect();
        menu::panel(t, entries, MENU_WIDTH)
    });
    menu::popover(
        trigger,
        panel,
        Align::End,
        Message::App(AppMsg::MenuDismissed),
    )
}
