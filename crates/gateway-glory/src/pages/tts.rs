use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let state = query(ctx, "tts.status", None);
    let providers = query(ctx, "tts.providers", None);
    let voice_data = ctx.scope.cage(Value::Null);
    let provider = ctx.scope.cage(String::new());
    let voice = ctx.scope.cage(String::new());
    let text_value = ctx.scope.cage(String::new());
    let speed = ctx.scope.cage("1.0".to_owned());
    let pitch = ctx.scope.cage("1.0".to_owned());
    let output = ctx.scope.cage(Value::Null);
    let source = state.value;
    ctx.scope.effect(move || {
        let value = source.get();
        if !value.is_null() {
            ui::set(provider, string(&value, &["provider"]));
            ui::set(voice, string(&value, &["voice"]));
            ui::set(speed, value["speed"].as_f64().unwrap_or(1.0).to_string());
            ui::set(pitch, value["pitch"].as_f64().unwrap_or(1.0).to_string());
        }
    });
    let mut children = vec![
        status(ctx, &state),
        status(ctx, &providers),
        refresh(ctx, &state),
        Box::new(text().content(source.map(|value| {
            format!(
                "Speech synthesis: {}",
                if value["enabled"].as_bool().unwrap_or(false) {
                    "enabled"
                } else {
                    "disabled"
                }
            )
        }))),
    ];
    let mut enable_actions = Vec::new();
    for (label, method) in [("Enable", "tts.enable"), ("Disable", "tts.disable")] {
        let context = ctx.clone();
        let state = state.clone();
        enable_actions.push(ui::action(ui::label_for(locale, label), move || {
            mutate(&context, &state, method, json!({}))
        }));
    }
    children.push(Box::new(row().class("actions").fill(enable_actions)));
    children.push(card(
        ui::label_for(locale, "Providers"),
        vec![records(ctx, &providers, &["providers"], move |entry, _| {
            let id = string(&entry, &["id", "provider"]);
            let selected = id.clone();
            Box::new(
                row().class("tts-provider-row").fill((
                    sapid::button()
                        .unstyled()
                        .class("btn-plain tts-provider-name")
                        .label(string(&entry, &["name", "id"]))
                        .on_press(move || ui::set(provider, selected.clone())),
                    text().class("tts-provider-status").content(ui::label_for(
                        locale,
                        if entry["is_configured"]
                            .as_bool()
                            .or_else(|| entry["configured"].as_bool())
                            .unwrap_or(false)
                        {
                            "Configured"
                        } else {
                            "Not configured"
                        },
                    )),
                )),
            )
        })],
    ));
    let mut voice_children = Vec::new();
    voice_children.push(ui::field(
        ui::label_for(locale, "Provider"),
        provider,
        false,
    ));
    let context = ctx.clone();
    let error = state.error;
    voice_children.push(ui::action(
        ui::label_for(locale, "List voices"),
        move || {
            let context_async = context.clone();
            let provider = provider.get();
            context.spawn(async move {
                match context_async
                    .rpc
                    .call::<Value>("tts.voices", Some(json!({"provider":provider})))
                    .await
                {
                    Ok(value) => ui::set(voice_data, value),
                    Err(message) => ui::set(error, message),
                }
            });
        },
    ));
    let voice_items = voice_data.map(|value| {
        array(value, &["voices"])
            .into_iter()
            .enumerate()
            .map(|(i, value)| (format!("{i}-{}", record_key(&value)), value))
            .collect::<Vec<_>>()
    });
    voice_children.push(Box::new(
        each(voice_items)
            .key(|(key, _)| key.clone())
            .children(move |each_row| {
                let item = each_row.value;
                let value = item.get_untracked().1;
                let name = value
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| string(&value, &["id", "name"]));
                let selected = name.clone();
                ui::action(name, move || ui::set(voice, selected.clone()))
            }),
    ));
    voice_children.push(ui::field(ui::label_for(locale, "Voice"), voice, false));
    let context = ctx.clone();
    let settings_state = state.clone();
    voice_children.push(ui::action(
        ui::label_for(locale, "Apply provider and voice"),
        move || {
            let context_async = context.clone();
            let state = settings_state.clone();
            let provider = provider.get();
            let voice = voice.get();
            ui::set(state.busy, true);
            context.spawn(async move {
                let result = async {
                    context_async
                        .rpc
                        .call::<Value>("tts.setProvider", Some(json!({"provider":provider})))
                        .await?;
                    context_async
                        .rpc
                        .call::<Value>("tts.setVoice", Some(json!({"voice":voice})))
                        .await
                }
                .await;
                match result {
                    Ok(_) => state.refresh(),
                    Err(message) => ui::set(state.error, message),
                }
                ui::set(state.busy, false);
            });
        },
    ));
    children.push(card(ui::label_for(locale, "Voice"), voice_children));
    let settings_children = vec![
        ui::field(ui::label_for(locale, "Speed (0.5 to 2)"), speed, false),
        ui::field(ui::label_for(locale, "Pitch (0.5 to 2)"), pitch, false),
    ];
    children.push(card(ui::label_for(locale, "Settings"), settings_children));
    let mut convert_children = Vec::new();
    convert_children.push(ui::field(
        ui::label_for(locale, "Text to convert"),
        text_value,
        true,
    ));
    let context = ctx.clone();
    let convert_state = state.clone();
    convert_children.push(Box::new(
        sapid::button()
            .label(ui::label_for(locale, "Convert to speech"))
            .loading(state.busy)
            .on_press(move || {
                if text_value.get().trim().is_empty() {
                    ui::set(convert_state.error, "Enter text to convert".to_owned());
                    return;
                }
                let valid = |value: String| {
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|v| v.is_finite() && (0.5..=2.0).contains(v))
                };
                let (Some(speed), Some(pitch)) = (valid(speed.get()), valid(pitch.get())) else {
                    ui::set(
                        convert_state.error,
                        "Speed and pitch must be between 0.5 and 2".to_owned(),
                    );
                    return;
                };
                let context_async = context.clone();
                let state = convert_state.clone();
                let text = text_value.get();
                ui::set(state.busy, true);
                ui::set(state.error, String::new());
                context.spawn(async move {
                    match context_async
                        .rpc
                        .call::<Value>(
                            "tts.convert",
                            Some(json!({"text":text,"speed":speed,"pitch":pitch})),
                        )
                        .await
                    {
                        Ok(value) => ui::set(output, value),
                        Err(message) => ui::set(state.error, message),
                    }
                    ui::set(state.busy, false);
                });
            }),
    ));
    convert_children.push(Box::new(text().content(output.map(|value| {
        value["audio_path"]
            .as_str()
            .or_else(|| value["audio_url"].as_str())
            .unwrap_or("")
            .to_owned()
    }))));
    convert_children.push(Box::new(when(
        output.map(|value| {
            value["audio_url"]
                .as_str()
                .or_else(|| value["audio_path"].as_str())
                .is_some()
        }),
        move || {
            let value = output.get();
            let url = value["audio_url"]
                .as_str()
                .or_else(|| value["audio_path"].as_str())
                .unwrap_or("")
                .to_owned();
            Box::new(
                link()
                    .target(url)
                    .fill(text().content(ui::label_for(locale, "Open generated audio"))),
            ) as Box<dyn Widget>
        },
    )));
    children.push(card(
        ui::label_for(locale, "Convert to speech"),
        convert_children,
    ));
    ui::localized_page(
        ctx,
        "nav.tts",
        "Speech providers, voices and text conversion",
        children,
    )
}
