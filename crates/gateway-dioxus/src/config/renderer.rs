use dioxus::prelude::*;

use crate::config::schema::{FieldType, SchemaField};

#[component]
pub fn FieldRenderer(
    field: SchemaField,
    value: String,
    on_change: EventHandler<String>,
) -> Element {
    let label = field.label.clone();
    let description = field.description.clone();
    let placeholder = field.placeholder.clone();
    let required = field.required;

    rsx! {
        div { class: "field-wrapper mb-4",
            label { class: "block text-sm font-medium mb-1",
                "{label}"
                if required {
                    span { class: "text-red-500 ml-1", "*" }
                }
            }

            match field.field_type.clone() {
                FieldType::String | FieldType::Url | FieldType::Email => rsx! {
                    StringField {
                        placeholder: placeholder,
                        sensitive: field.sensitive,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                FieldType::Text => rsx! {
                    TextAreaField {
                        placeholder: placeholder,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                FieldType::Password | FieldType::Secret => rsx! {
                    PasswordField {
                        placeholder: placeholder,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                FieldType::Number | FieldType::Integer => rsx! {
                    NumberField {
                        min: field.min,
                        max: field.max,
                        step: field.step,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                FieldType::Boolean => rsx! {
                    BooleanField {
                        label: label.clone(),
                        checked: value == "true",
                        on_change: on_change.clone()
                    }
                },
                FieldType::Enum(opts) => rsx! {
                    EnumField {
                        options: opts,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                FieldType::Color => rsx! {
                    ColorField {
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                FieldType::Code(lang) => rsx! {
                    CodeField {
                        language: lang,
                        placeholder: placeholder,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
                _ => rsx! {
                    StringField {
                        placeholder: placeholder,
                        sensitive: false,
                        value: value.clone(),
                        on_change: on_change.clone()
                    }
                },
            }

            if let Some(desc) = description {
                p { class: "text-sm text-gray-500 mt-1", "{desc}" }
            }
        }
    }
}

#[component]
fn StringField(
    placeholder: Option<String>,
    sensitive: bool,
    value: String,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        input {
            class: "w-full px-3 py-2 border rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500",
            r#type: if sensitive { "password" } else { "text" },
            value: value,
            placeholder: placeholder.unwrap_or_default(),
            oninput: move |e| {
                on_change.call(e.value());
            }
        }
    }
}

#[component]
fn TextAreaField(
    placeholder: Option<String>,
    value: String,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        textarea {
            class: "w-full px-3 py-2 border rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500 font-mono text-sm",
            rows: 4,
            placeholder: placeholder.unwrap_or_default(),
            value: value,
            oninput: move |e| {
                on_change.call(e.value());
            }
        }
    }
}

#[component]
fn PasswordField(
    placeholder: Option<String>,
    value: String,
    on_change: EventHandler<String>,
) -> Element {
    let mut show = use_signal(|| false);

    rsx! {
        div { class: "relative",
            input {
                class: "w-full px-3 py-2 pr-16 border rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500",
                r#type: if show() { "text" } else { "password" },
                value: value,
                placeholder: placeholder.unwrap_or_default(),
                oninput: move |e| {
                    on_change.call(e.value());
                }
            }
            button {
                class: "absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 hover:text-gray-700 text-sm",
                r#type: "button",
                onclick: move |_| show.toggle(),
                if show() { "Hide" } else { "Show" }
            }
        }
    }
}

#[component]
fn NumberField(
    min: Option<f64>,
    max: Option<f64>,
    step: Option<f64>,
    value: String,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        input {
            class: "w-full px-3 py-2 border rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500",
            r#type: "number",
            value: value,
            min: min.map(|v| v.to_string()),
            max: max.map(|v| v.to_string()),
            step: step.unwrap_or(1.0).to_string(),
            oninput: move |e| {
                on_change.call(e.value());
            }
        }
    }
}

#[component]
fn BooleanField(label: String, checked: bool, on_change: EventHandler<String>) -> Element {
    rsx! {
        label { class: "flex items-center cursor-pointer",
            input {
                class: "w-4 h-4 text-blue-600 rounded focus:ring-2 focus:ring-blue-500",
                r#type: "checkbox",
                checked: checked,
                onchange: move |e| {
                    on_change.call(e.checked().to_string());
                }
            }
            span { class: "ml-2 text-sm", "{label}" }
        }
    }
}

#[component]
fn EnumField(options: Vec<String>, value: String, on_change: EventHandler<String>) -> Element {
    rsx! {
        select {
            class: "w-full px-3 py-2 border rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500",
            value: value,
            onchange: move |e| {
                on_change.call(e.value());
            },
            option { value: "", "-- Select --" }
            for opt in options {
                option {
                    value: opt.clone(),
                    "{opt}"
                }
            }
        }
    }
}

#[component]
fn ColorField(value: String, on_change: EventHandler<String>) -> Element {
    let color_val = if value.is_empty() {
        "#000000".to_string()
    } else {
        value.clone()
    };

    rsx! {
        div { class: "flex items-center gap-2",
            input {
                class: "w-12 h-10 rounded cursor-pointer",
                r#type: "color",
                value: color_val.clone(),
                oninput: move |e| {
                    on_change.call(e.value());
                }
            }
            input {
                class: "flex-1 px-3 py-2 border rounded-md font-mono text-sm",
                r#type: "text",
                value: value,
                placeholder: "#000000",
                oninput: move |e| {
                    on_change.call(e.value());
                }
            }
        }
    }
}

#[component]
fn CodeField(
    language: String,
    placeholder: Option<String>,
    value: String,
    on_change: EventHandler<String>,
) -> Element {
    rsx! {
        div { class: "relative",
            span {
                class: "absolute top-2 right-2 text-xs text-gray-400 bg-gray-100 px-2 py-1 rounded",
                "{language}"
            }
            textarea {
                class: "w-full px-3 py-2 border rounded-md focus:outline-none focus:ring-2 focus:ring-blue-500 font-mono text-sm min-h-32",
                placeholder: placeholder.unwrap_or_default(),
                value: value,
                oninput: move |e| {
                    on_change.call(e.value());
                }
            }
        }
    }
}

#[component]
pub fn ConfigSection(
    title: String,
    icon: Option<String>,
    description: Option<String>,
    children: Element,
) -> Element {
    rsx! {
        div { class: "bg-white rounded-lg shadow-sm border mb-4",
            div { class: "px-4 py-3 border-b flex items-center gap-2",
                if let Some(icon_name) = icon {
                    span { class: "text-gray-500", "{icon_name}" }
                }
                h3 { class: "font-medium", "{title}" }
            }
            if let Some(desc) = description {
                p { class: "px-4 py-2 text-sm text-gray-500 border-b bg-gray-50", "{desc}" }
            }
            div { class: "p-4",
                {children}
            }
        }
    }
}
