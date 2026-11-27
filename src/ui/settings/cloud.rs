use iced::widget::{column, row, text, text_input};
use iced::{Alignment, Element, Length};

use crate::app::AppState;
use crate::app::cloud::{CloudMsg, Field, Phase};
use crate::message::Message;
use crate::ui::components::button::{Size, Variant, button};
use crate::ui::components::field;
use crate::ui::theme::{SPACE_LG, SPACE_MD, SPACE_XS, TEXT_MD, TEXT_SM, Tokens};

pub(super) fn view<'a>(state: &'a AppState, t: &'a Tokens) -> Element<'a, Message> {
    let cloud = &state.cloud;
    let cfg = &cloud.draft;
    let mut controls = row![
        button(t, "Save & connect", Variant::Primary, Size::Regular)
            .on_press_maybe((!cloud.busy()).then_some(Message::Cloud(CloudMsg::Connect))),
    ]
    .spacing(SPACE_MD)
    .align_y(Alignment::Center);
    if cloud.connected() {
        controls = controls
            .push(
                button(t, "Sync now", Variant::Secondary, Size::Regular)
                    .on_press_maybe((!cloud.busy()).then_some(Message::Cloud(CloudMsg::Sync))),
            )
            .push(
                button(t, "Disconnect", Variant::Ghost, Size::Regular).on_press_maybe(
                    (!cloud.busy()).then_some(Message::Cloud(CloudMsg::Disconnect)),
                ),
            );
    }

    let mut page = column![
        text(cloud.label()).size(TEXT_MD).color(t.text),
        text("Use the same private S3-compatible bucket and object key on each device. Saving a request syncs your collections, and sending a request syncs your last 100 history entries.")
            .size(TEXT_SM).color(t.text_muted),
    ].spacing(SPACE_LG);
    match &cloud.phase {
        Phase::Error(error) => page = page.push(text(error).size(TEXT_SM).color(t.status.error)),
        Phase::Conflict(conflict) => {
            let detail = match conflict.cloud_collections {
                Some(count) => format!(
                    "This device has {} collections and the cloud has {count}. The workspace you choose will replace the other collection set. Open drafts stay on this device.",
                    conflict.device_collections,
                ),
                None => {
                    "The cloud object is missing. Use this device's collections to recreate it."
                        .to_owned()
                }
            };
            let mut choices = row![
                button(t, "Use this device", Variant::Secondary, Size::Regular)
                    .on_press(Message::Cloud(CloudMsg::UseLocal)),
            ]
            .spacing(SPACE_MD);
            if conflict.cloud_collections.is_some() {
                choices = choices.push(
                    button(t, "Use cloud workspace", Variant::Secondary, Size::Regular)
                        .on_press(Message::Cloud(CloudMsg::UseCloud)),
                );
            }
            page = page.push(
                column![text(detail).size(TEXT_SM).color(t.status.warning), choices]
                    .spacing(SPACE_MD),
            );
        }
        _ => {}
    }
    if let Some(error) = cloud.history_error() {
        page = page.push(text(format!("History sync failed. {error}")).size(TEXT_SM).color(t.status.error));
    }

    page.push(column![
        input(t, Field::Endpoint, "Endpoint URL", "https://s3.us-east-1.amazonaws.com", &cfg.endpoint),
        row![
            input(t, Field::Bucket, "Bucket", "my-private-bucket", &cfg.bucket),
            input(t, Field::Region, "Signing region", "us-east-1 · auto for R2", &cfg.region),
        ].spacing(SPACE_MD),
        input(t, Field::Key, "Object key", "rustman/workspace.json", &cfg.key),
        input(t, Field::AccessKey, "Access key ID", "Access key ID", &cfg.access_key),
        input(t, Field::SecretKey, "Secret access key", "Secret access key", &cfg.secret_key),
        input(t, Field::SessionToken, "Session token (optional)", "Temporary credentials only", &cfg.session_token),
    ].spacing(SPACE_MD))
    .push(controls)
    .push(text("Your endpoint must support conditional PutObject. Use a bucket with versioning off to keep only the latest snapshot.")
        .size(TEXT_SM).color(t.text_muted))
    .push(text("Sync includes URLs, query parameters and body values. Environment values, recognized auth secrets, attachment bytes and oversized history bodies stay on their original device.")
        .size(TEXT_SM).color(t.text_muted))
    .into()
}

fn input<'a>(
    t: &'a Tokens,
    kind: Field,
    label: &'static str,
    placeholder: &'static str,
    value: &'a str,
) -> Element<'a, Message> {
    column![
        text(label).size(TEXT_SM).color(t.text),
        text_input(placeholder, value)
            .secure(matches!(kind, Field::SecretKey | Field::SessionToken))
            .on_input(move |value| Message::Cloud(CloudMsg::Edit(kind, value)))
            .size(TEXT_MD)
            .padding([8, 10])
            .style(move |_, status| field::field_on(
                t,
                status,
                crate::ui::components::Surface::Raised
            )),
    ]
    .width(Length::Fill)
    .spacing(SPACE_XS)
    .into()
}
