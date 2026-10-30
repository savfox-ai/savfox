use glory::prelude::*;
use serde_json::{Value, json};

use super::common::*;
use crate::ui::{self, AppContext};
pub fn render(ctx: &AppContext) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let page = ctx.scope.cage(1_u64);
    let size = ctx.scope.cage("50".to_owned());
    let category = ctx.scope.cage(String::new());
    let filter = ctx.scope.cage(String::new());
    let url = ctx.scope.cage(String::new());
    let bins = ctx.scope.cage(Value::Null);
    let state = query(ctx, "skills.status", None);
    let error = state.error;
    let busy = state.busy;
    let revision = state.revision;
    let context = ctx.clone();
    ctx.scope.effect(move||{let connected=context.connected.get();let page=page.get();let page_size=size.get().parse::<u64>().unwrap_or(50);let category=category.get();let search=filter.get();let _=revision.get();if !connected{return;}let context_async=context.clone();context.spawn(async move{ui::set(busy,true);match context_async.rpc.call::<Value>("skills.bins",Some(json!({"page":page,"page_size":page_size,"category":category,"search":search}))).await{Ok(value)=>ui::set(bins,value),Err(message)=>ui::set(error,message)}ui::set(busy,false);});});
    let mut controls = vec![
        refresh(ctx, &state),
        ui::field(ui::label_for(locale, "Search skills"), filter, false),
        choice_for(
            locale,
            "Category",
            category,
            &[
                ("", "All"),
                ("workspace", "Workspace"),
                ("builtin", "Built in"),
                ("third_party", "Third party"),
            ],
        ),
        choice_for(
            locale,
            "Page size",
            size,
            &[("25", "25"), ("50", "50"), ("100", "100")],
        ),
        ui::field(ui::label_for(locale, "Git URL"), url, false),
    ];
    let context = ctx.clone();
    let install = state.clone();
    controls.push(ui::action(
        ui::label_for(locale, "Install from URL"),
        move || {
            if url.get().trim().is_empty() {
                ui::set(install.error, "Enter a Git URL".to_owned());
                return;
            }
            mutate(
                &context,
                &install,
                "skills.install_url",
                json!({"url":url.get()}),
            );
        },
    ));
    let context = ctx.clone();
    let zip_state = state.clone();
    controls.push(ui::action(ui::label_for(locale, "Upload ZIP"), move || {
        upload_zip(&context, &zip_state)
    }));
    let stats = [
        ("Installed", "installed_count"),
        ("Available", "available_count"),
        ("Enabled", "enabled_count"),
        ("Blocked", "blocked_count"),
    ]
    .into_iter()
    .map(|(label, key)| {
        let source = state.value;
        Box::new(
            col().class("skill-stat").fill((
                text()
                    .class("skill-stat__label")
                    .content(ui::label_for(locale, label)),
                text()
                    .class("skill-stat__value")
                    .content(ctx.scope.bond(move || {
                        source.get()[key]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_else(|| {
                                let entries = array(&bins.get(), &["bins", "skills"]);
                                if entries.is_empty() {
                                    "—".into()
                                } else {
                                    entries
                                        .iter()
                                        .filter(|entry| {
                                            if key == "enabled_count" {
                                                entry["enabled"].as_bool().unwrap_or(false)
                                            } else {
                                                entry["allowlist_blocked"]
                                                    .as_bool()
                                                    .unwrap_or(false)
                                            }
                                        })
                                        .count()
                                        .to_string()
                                }
                            })
                    })),
            )),
        ) as Box<dyn Widget>
    })
    .collect::<Vec<_>>();
    let mut children = vec![
        status(ctx, &state),
        Box::new(row().class("skills-toolbar").fill(controls)),
        Box::new(row().class("skills-stats").fill(stats)),
    ];
    let context = ctx.clone();
    let row_state = state.clone();
    let items = bins.map(|value| {
        array(value, &["bins", "skills"])
            .into_iter()
            .enumerate()
            .map(|(i, value)| (format!("{i}-{}", record_key(&value)), value))
            .collect::<Vec<_>>()
    });
    children.push(Box::new(each(items).key(|(key, _)| key.clone()).children(
        move |each_row| {
            let item = each_row.value;
            let skill = item.get_untracked().1;
            let name = string(&skill, &["name"]);
            let enabled = skill["enabled"].as_bool().unwrap_or(true);
            let installed = skill["installed"].as_bool().unwrap_or(false);
            let mut actions = vec![];
            if installed {
                let context = context.clone();
                let state = row_state.clone();
                let name = name.clone();
                actions.push(Box::new(
                    sapid::switch()
                        .label(ui::label_for(locale, "Enabled"))
                        .checked(enabled)
                        .on_checked_change(move |enabled| {
                            mutate(
                                &context,
                                &state,
                                "skills.update",
                                json!({"name":name,"enabled":enabled}),
                            )
                        }),
                ) as Box<dyn Widget>);
            } else {
                let context = context.clone();
                let state = row_state.clone();
                let name = name.clone();
                actions.push(ui::action(ui::label_for(locale, "Install"), move || {
                    mutate(
                        &context,
                        &state,
                        "skills.registry.install",
                        json!({"name":name}),
                    )
                }));
            }
            let env_name = string(&skill, &["primary_env"]);
            if !env_name.is_empty() {
                let value = context.scope.cage(String::new());
                let context_env = context.clone();
                let state = row_state.clone();
                actions.push(secret(env_name.clone(), value));
                actions.push(ui::action(
                    ui::label_for(locale, "Save credential"),
                    move || {
                        let context_async = context_env.clone();
                        let state = state.clone();
                        let key = env_name.clone();
                        let secret = value.get();
                        context_env.spawn(async move {
                            match context_async
                                .rpc
                                .call::<Value>(
                                    "skills.setEnv",
                                    Some(json!({"key":key,"value":secret})),
                                )
                                .await
                            {
                                Ok(_) => {
                                    ui::set(value, String::new());
                                    state.refresh();
                                }
                                Err(message) => ui::set(state.error, message),
                            }
                        });
                    },
                ));
            }
            let primary = actions.remove(0);
            let details = col().class("skill-details").fill((
                metadata_for(
                    locale,
                    &skill,
                    &[
                        ("version", "Version"),
                        ("path", "Path"),
                        ("missing_deps", "Missing dependencies"),
                        ("disabled_reason", "Disabled reason"),
                        ("conflicts", "Conflicts"),
                    ],
                ),
                col().class("actions").fill(actions),
            ));
            Box::new(
                col().class("skill-card").fill((
                    row().class("skill-card__header").fill((
                        text().class("skill-card__name").content(name.clone()),
                        text()
                            .class("skill-card__source")
                            .content(string(&skill, &["category"])),
                        text()
                            .class("skill-card__eligibility")
                            .content(ui::label_for(
                                locale,
                                if skill["eligible"].as_bool().unwrap_or(false) {
                                    "Eligible"
                                } else {
                                    "Blocked"
                                },
                            )),
                        primary,
                    )),
                    text()
                        .class("skill-card__description")
                        .content(string(&skill, &["description"])),
                    ui::details(
                        &select_key(&format!("skill-{name}-{}", record_key(&skill))),
                        ui::label_for(locale, "More"),
                        Box::new(details),
                    ),
                )),
            )
        },
    )));
    children.push(Box::new(text().content(ctx.scope.bond(move || {
        format!(
            "Page {} of {} · {} skills",
            page.get(),
            bins.get()["total_pages"].as_u64().unwrap_or(1),
            bins.get()["total"].as_u64().unwrap_or(0)
        )
    }))));
    children.push(ui::action(
        ui::label_for(locale, "Previous page"),
        move || ui::set(page, page.get().saturating_sub(1).max(1)),
    ));
    children.push(ui::action(ui::label_for(locale, "Next page"), move || {
        let total = bins.get()["total_pages"].as_u64().unwrap_or(1);
        if page.get() < total {
            ui::set(page, page.get() + 1);
        }
    }));
    ui::localized_page(
        ctx,
        "nav.skills",
        "Install skills, manage credentials and control availability",
        children,
    )
}
#[cfg(target_arch = "wasm32")]
fn upload_zip(ctx: &AppContext, state: &Query) {
    use base64::Engine;
    use wasm_bindgen::JsCast;
    use wasm_bindgen::prelude::*;
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(element) = document.create_element("input") else {
        return;
    };
    let Ok(input) = element.dyn_into::<web_sys::HtmlInputElement>() else {
        return;
    };
    input.set_type("file");
    input.set_accept(".zip");
    let picker = input.clone();
    let context = ctx.clone();
    let state = state.clone();
    let on_change = Closure::once_into_js(move || {
        let Some(file) = picker.files().and_then(|files| files.get(0)) else {
            return;
        };
        if file.size() > 50.0 * 1024.0 * 1024.0 {
            ui::set(state.error, "ZIP archive exceeds 50 MiB".to_owned());
            return;
        }
        let context_async = context.clone();
        context.spawn(async move {
            match wasm_bindgen_futures::JsFuture::from(file.array_buffer()).await {
                Ok(buffer) => {
                    let bytes = js_sys::Uint8Array::new(&buffer).to_vec();
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                    mutate(
                        &context_async,
                        &state,
                        "skills.install_zip",
                        json!({"data":data}),
                    );
                }
                Err(message) => {
                    ui::set(state.error, format!("Cannot read ZIP archive: {message:?}"))
                }
            }
        });
    });
    input.set_onchange(Some(on_change.unchecked_ref()));
    input.click();
}
#[cfg(not(target_arch = "wasm32"))]
fn upload_zip(_ctx: &AppContext, _state: &Query) {}
