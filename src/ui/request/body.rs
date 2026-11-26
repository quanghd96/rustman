use iced::{
    Background, Border, Color, Element, Length,
    widget::{Space, button, checkbox, column, container, row, text},
};

use crate::{
    domain::request::{BodyType, FormField, FormFieldType, file_size},
    message::{Message, RequestMsg},
    state::tabs::RequestTabState,
    ui::{icons, theme::Palette},
};

pub fn view(tab: &RequestTabState) -> Element<'_, Message> {
    match tab.body_type {
        BodyType::None => container(
            column![
                text("No body").size(13).color(Palette::text_muted()),
                text("Pick a body type at the end of the tab bar")
                    .size(11)
                    .color(Palette::text_subtle()),
            ]
            .spacing(4)
            .align_x(iced::Alignment::Center),
        )
        .center_x(Length::Fill)
        .padding([24, 8])
        .into(),

        BodyType::Json | BodyType::Text => tab
            .body_editor
            .view()
            .map(|m| Message::Request(RequestMsg::BodyEdited(m))),

        BodyType::FormData | BodyType::Multipart => form_data_view(tab, true),
        BodyType::UrlEncoded => form_data_view(tab, false),
        BodyType::Binary => binary_body_view(tab),

        BodyType::GraphQL => {
            let query: Element<Message> = tab
                .body_editor
                .view()
                .map(|m| Message::Request(RequestMsg::BodyEdited(m)));
            let variables: Element<Message> = tab
                .graphql_variables_editor
                .view()
                .map(|m| Message::Request(RequestMsg::GraphQLVariablesEdited(m)));

            row![
                column![pane_label("Query"), query]
                    .width(Length::FillPortion(3))
                    .height(Length::Fill),
                crate::ui::components::vhairline(Palette::tokens()),
                column![pane_label("Variables"), variables]
                    .width(Length::FillPortion(2))
                    .height(Length::Fill),
            ]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }
}

fn binary_body_view(tab: &RequestTabState) -> Element<'_, Message> {
    use crate::ui::components::button::{Size, Variant, button as control, button_with_icon};
    use crate::ui::theme::{SPACE_LG, SPACE_MD, TEXT_MD, TEXT_SM};

    let t = Palette::tokens();
    let file = tab.binary_file.as_ref();
    let data = file.and_then(|file| file.data.as_deref());
    let picking = tab.binary_file_pick.is_some();
    let select_label = match (picking, data.is_some()) {
        (true, _) => "Selecting file…",
        (false, true) => "Replace file…",
        (false, false) => "Select file…",
    };
    let select = button_with_icon(
        t,
        icons::file_input(),
        select_label,
        Variant::Secondary,
        Size::Regular,
    )
    .on_press_maybe((!picking).then_some(Message::Request(RequestMsg::BinaryFilePick)));
    let mut actions = row![select]
        .spacing(SPACE_MD)
        .align_y(iced::Alignment::Center);
    if file.is_some() {
        actions = actions.push(
            control(t, "Remove", Variant::Ghost, Size::Regular)
                .on_press(Message::Request(RequestMsg::BinaryFileRemoved)),
        );
    }
    let label = file.map_or("No file selected", |file| file.name.as_str());
    let detail = data.map_or_else(
        || {
            if file.is_some() {
                "Select the local file to attach its contents.".to_owned()
            } else {
                "The file is sent as the entire request body.".to_owned()
            }
        },
        |data| rustman_core::text::human_size(file_size(data)),
    );
    container(
        column![
            text(crate::services::format::head_capped(label, 512))
                .size(TEXT_MD)
                .color(t.text),
            text(detail).size(TEXT_SM).color(t.text_muted),
            actions,
        ]
        .spacing(SPACE_MD),
    )
    .padding(SPACE_LG)
    .width(Length::Fill)
    .into()
}

/// Expand + copy buttons for a form row, shown only when the value is in
/// preview mode (too big/multiline for an inline input). Small values stay
/// directly editable/selectable, so they need neither.
fn preview_actions<'a>(
    tab: &'a RequestTabState,
    field: &'a FormField,
    i: usize,
) -> Element<'a, Message> {
    let is_preview = field.field_type == FormFieldType::Text
        && !crate::ui::request::cell::is_inline(&field.value);
    if !is_preview {
        return Space::new().width(0).into();
    }
    let expanded = tab.form_viewers.contains_key(&field.id);
    row![
        button(
            (if expanded {
                icons::chevron_down()
            } else {
                icons::chevron_right()
            })
            .size(10)
            .color(Palette::text_muted())
        )
        .on_press(Message::Request(RequestMsg::FormValueExpandToggled(i)))
        .style(iced::widget::button::text)
        .padding([3, 4]),
        button(icons::copy().size(10).color(Palette::text_muted()))
            .on_press(Message::Request(RequestMsg::FormValueCopy(i)))
            .style(iced::widget::button::text)
            .padding([3, 4]),
    ]
    .spacing(0)
    .align_y(iced::Alignment::Center)
    .into()
}

/// Height of an expanded form value, in pixels. The page editor scrolls
/// internally, so the row never grows with the value.
const FORM_VIEWER_HEIGHT: f32 = 240.0;

/// Paged full-value editor under an expanded preview row. Pages reuse the
/// response viewer's windowing (`page_starts`), and the page itself renders in
/// the shared code-editor UI — selectable, searchable, scrollable and
/// editable — so working with everything costs one bounded page regardless of
/// value size. Edits write back into the field value at once.
fn form_value_viewer<'a>(
    tab: &'a RequestTabState,
    field: &'a FormField,
) -> Option<Element<'a, Message>> {
    let viewer = tab.form_viewers.get(&field.id)?;
    let count = viewer.page_count();
    let page = viewer.page().min(count - 1);
    let (start, end) = viewer.page_span(&field.value);
    let index = field_index(tab, field);
    let editor: Element<'a, Message> = viewer.editor().view().map(move |editor_message| {
        Message::Request(RequestMsg::FormValueEdited(index, editor_message))
    });

    let prev = button(icons::arrow_left().size(11).color(Palette::text_muted()))
        .padding([4, 6])
        .style(iced::widget::button::text);
    let prev = if page > 0 {
        prev.on_press(Message::Request(RequestMsg::FormValuePagePrev(index)))
    } else {
        prev
    };
    let next = button(icons::arrow_right().size(11).color(Palette::text_muted()))
        .padding([4, 6])
        .style(iced::widget::button::text);
    let next = if page + 1 < count {
        next.on_press(Message::Request(RequestMsg::FormValuePageNext(index)))
    } else {
        next
    };
    Some(
        container(
            column![
                row![
                    prev,
                    text(format!("Page {}/{}", page + 1, count))
                        .size(10)
                        .color(Palette::text_muted()),
                    next,
                    Space::new().width(Length::Fill),
                    text(format!(
                        "{} of {}",
                        rustman_core::text::human_size(end - start),
                        rustman_core::text::human_size(field.value.len())
                    ))
                    .size(10)
                    .color(Palette::text_subtle()),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center),
                container(editor)
                    .height(Length::Fixed(FORM_VIEWER_HEIGHT))
                    .width(Length::Fill),
            ]
            .spacing(4),
        )
        .style(|_| iced::widget::container::Style {
            background: Some(Background::Color(Palette::surface_high())),
            border: Border {
                color: Palette::border_subtle(),
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .padding([6, 8])
        .width(Length::Fill)
        .into(),
    )
}

/// Row index of `field` for page messages (ids are stable, indices are not —
/// resolve at render time).
fn field_index(tab: &RequestTabState, field: &FormField) -> usize {
    tab.form_fields
        .iter()
        .position(|f| f.id == field.id)
        .unwrap_or(usize::MAX)
}

/// Section header above a split pane (GraphQL's Query / Variables).
fn pane_label(label: &'static str) -> Element<'static, Message> {
    container(text(label).size(10).color(Palette::text_muted()))
        .style(|_| iced::widget::container::Style {
            background: Some(Background::Color(Palette::background())),
            ..Default::default()
        })
        .padding(iced::Padding {
            top: 3.0,
            right: 8.0,
            bottom: 3.0,
            left: 8.0,
        })
        .width(Length::Fill)
        .into()
}

fn form_data_view(tab: &RequestTabState, allow_files: bool) -> Element<'_, Message> {
    let mut col = column![].spacing(0);
    for (i, field) in tab.form_fields.iter().enumerate() {
        let is_file = field.field_type == FormFieldType::File;
        if is_file && !allow_files {
            continue;
        }
        let accent = Palette::accent();
        let muted = Palette::text_muted();

        let type_btn = button(
            text(if is_file { "File" } else { "Text" })
                .size(10)
                .color(if is_file { accent } else { muted }),
        )
        .on_press(Message::Request(RequestMsg::FormFieldTypeToggled(i)))
        .style(move |_, _| iced::widget::button::Style {
            background: if is_file {
                Some(Background::Color(Palette::accent_soft()))
            } else {
                None
            },
            text_color: if is_file { accent } else { muted },
            border: iced::Border {
                color: if is_file { accent } else { Color::TRANSPARENT },
                width: 1.0,
                radius: 6.0.into(),
            },
            ..Default::default()
        })
        .padding([2, 6]);

        let value_widget: Element<Message> = if is_file {
            let label = field.file_name.as_deref().unwrap_or("Choose file…");
            row![
                button(text(label).size(11).color(Palette::text()))
                    .on_press(Message::Request(RequestMsg::FormFieldPickFile(i)))
                    .style(|_, s| iced::widget::button::Style {
                        background: Some(Background::Color(
                            if matches!(s, iced::widget::button::Status::Hovered) {
                                Palette::surface_raised()
                            } else {
                                Palette::surface_high()
                            }
                        )),
                        text_color: Palette::text(),
                        border: iced::Border {
                            color: Palette::border_subtle(),
                            width: 1.0,
                            radius: 6.0.into()
                        },
                        ..Default::default()
                    })
                    .padding([4, 10])
                    .width(Length::Fill),
            ]
            .width(Length::Fill)
            .into()
        } else {
            crate::ui::request::cell::text_cell(
                "Value",
                &field.value,
                12.0,
                [4, 6],
                Length::Fill,
                move |s| Message::Request(RequestMsg::FormFieldValueChanged(i, s)),
            )
        };

        let enabled = checkbox(field.enabled)
            .size(14)
            .on_toggle(move |_| Message::Request(RequestMsg::FormFieldToggled(i)));
        let mut cells = row![enabled].spacing(6).align_y(iced::Alignment::Center);
        if allow_files {
            cells = cells.push(type_btn);
        }
        let row_el = container(
            cells
                .push(crate::ui::request::cell::text_cell(
                    "Key",
                    &field.key,
                    12.0,
                    [4, 6],
                    120,
                    move |s| Message::Request(RequestMsg::FormFieldKeyChanged(i, s)),
                ))
                .push(value_widget)
                .push(preview_actions(tab, field, i))
                .push(
                    button(icons::close().size(10).color(Palette::text_muted()))
                        .on_press(Message::Request(RequestMsg::FormFieldRemoved(i)))
                        .style(iced::widget::button::text)
                        .padding([3, 6]),
                ),
        )
        .style(move |_| iced::widget::container::Style {
            background: if i % 2 == 0 {
                Some(Background::Color(Palette::row_odd()))
            } else {
                None
            },
            ..Default::default()
        })
        .padding(iced::Padding {
            top: 2.0,
            right: 8.0,
            bottom: 2.0,
            left: 8.0,
        })
        .width(Length::Fill);
        col = col.push(row_el);
        if let Some(expanded) = form_value_viewer(tab, field) {
            col = col.push(expanded);
        }
    }
    col = col.push(
        container(
            button(
                row![
                    text("+").size(13).color(Palette::accent()),
                    text(" Add field").size(11).color(Palette::text_muted()),
                ]
                .spacing(4)
                .align_y(iced::Alignment::Center),
            )
            .on_press(Message::Request(RequestMsg::FormFieldAdded))
            .style(iced::widget::button::text)
            .padding([6, 14]),
        )
        .width(Length::Fill),
    );
    crate::ui::components::scroll::vertical(crate::ui::theme::Palette::tokens(), col)
        .height(Length::Fill)
        .into()
}
