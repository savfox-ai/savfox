use dioxus::prelude::*;

use crate::api::types::ChatAttachment;

#[component]
pub fn AttachmentPreview(
    attachments: Vec<ChatAttachment>,
    on_remove: EventHandler<usize>,
) -> Element {
    if attachments.is_empty() {
        return rsx! {};
    }

    rsx! {
        div { class: "attachment-preview",
            for (i, att) in attachments.iter().enumerate() {
                div { key: "{i}", class: "attachment-preview__item",
                    if att.mime_type.starts_with("image/") {
                        img {
                            src: "data:{att.mime_type};base64,{att.data_base64}",
                            alt: "{att.filename}",
                            class: "attachment-preview__thumb",
                        }
                    } else {
                        span { class: "attachment-preview__name", "{att.filename}" }
                    }
                    button {
                        class: "attachment-preview__remove",
                        onclick: move |_| on_remove(i),
                        "x"
                    }
                }
            }
        }
    }
}
