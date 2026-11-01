use super::*;

type Errors = Cage<HashMap<String, String>>;

const GROUPS: &[(&str, &str, &[&str])] = &[
    ("gateway", "Gateway and security", &["gateway"]),
    (
        "execution",
        "Execution and tools",
        &[
            "approval_policy",
            "sandbox_mode",
            "sandbox_workspace_write",
            "shell_environment_policy",
            "mcp_servers",
            "mcp_oauth_credentials_store",
            "mcp_oauth_callback_port",
            "tools",
            "web_search",
            "agents",
            "skills",
        ],
    ),
    (
        "context",
        "Sessions and context",
        &[
            "model_context_window",
            "model_auto_compact_token_limit",
            "tool_output_token_limit",
            "history",
            "compact_prompt",
            "project_doc_max_bytes",
            "project_doc_fallback_filenames",
        ],
    ),
    (
        "environment",
        "Runtime environment",
        &[
            "check_for_update_on_startup",
            "otel",
            "analytics",
            "feedback",
            "notify",
            "registry",
        ],
    ),
];

fn name(key: &str, schema: &Value) -> String {
    schema["title"]
        .as_str()
        .filter(|title| title.contains(' ') || title.eq_ignore_ascii_case(key))
        .map(str::to_owned)
        .unwrap_or_else(|| {
            let label = key.replace('_', " ");
            let mut chars = label.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                .unwrap_or_default()
        })
}

fn resolve(schema: &Value, root: &Value, current: &Value) -> Value {
    let mut schema = schema.clone();
    for _ in 0..8 {
        if let Some(target) = schema["$ref"]
            .as_str()
            .and_then(|r| r.strip_prefix('#'))
            .and_then(|p| root.pointer(p))
        {
            schema = target.clone();
            continue;
        }
        if let Some(variants) = schema.get_mut("anyOf").and_then(Value::as_array_mut) {
            for variant in variants {
                if let Some(target) = variant["$ref"]
                    .as_str()
                    .and_then(|r| r.strip_prefix('#'))
                    .and_then(|p| root.pointer(p))
                {
                    *variant = target.clone();
                }
            }
        }
        // An inherited optional value still needs the non-null editor. Null is
        // represented by "Use default", not by an unusable text field.
        let next = resolve_schema_variant(&schema, (!current.is_null()).then_some(current));
        if next == schema {
            break;
        }
        schema = next;
    }
    schema
}

fn matches_search(
    key: &str,
    schema: &Value,
    root: &Value,
    search: &str,
    locale: crate::i18n::Locale,
) -> bool {
    if search.is_empty() {
        return true;
    }
    let schema = resolve(schema, root, &Value::Null);
    let title = name(key, &schema);
    format!(
        "{key} {title} {} {}",
        ui::translate(locale, &title),
        schema["description"].as_str().unwrap_or_default()
    )
    .to_lowercase()
    .contains(search)
        || schema["properties"].as_object().is_some_and(|props| {
            props
                .iter()
                .any(|(key, schema)| matches_search(key, schema, root, search, locale))
        })
}

pub(super) fn render(ctx: &AppContext, path: &str) -> Box<dyn Widget> {
    let locale = ctx.locale;
    let config = query(ctx, "config.get", None);
    let schema = query(ctx, "config.schema", None);
    let draft = ctx.scope.cage(json!({}));
    let baseline = ctx.scope.cage(json!({}));
    let revision = ctx.scope.cage(String::new());
    let initialized = ctx.scope.cage(false);
    let errors: Errors = ctx.scope.cage(HashMap::new());
    let message = ctx.scope.cage(String::new());
    let selected = ctx.scope.cage(
        path.strip_prefix("/config/")
            .filter(|p| *p == "advanced" || GROUPS.iter().any(|(id, ..)| id == p))
            .unwrap_or("overview")
            .to_owned(),
    );
    let search = ctx.scope.cage(String::new());
    let source = config.value;
    ctx.scope.effect(move || {
        let data = source.get();
        if !initialized.get_untracked()
            && let Some(value) = data.get("config")
        {
            ui::set(draft, value.clone());
            ui::set(baseline, value.clone());
            ui::set(
                revision,
                data["revision"].as_str().unwrap_or_default().to_owned(),
            );
            ui::set(initialized, true);
        }
    });
    let busy = config.busy;
    let error = config.error;
    let context = ctx.clone();
    let save = sapid::button()
        .class("btn btn--primary")
        .label(ui::label_for(locale, "Save configuration"))
        .loading(busy)
        .enabled(ctx.scope.bond(move || {
            initialized.get()
                && !busy.get()
                && errors.get().is_empty()
                && (draft.get() != baseline.get()
                    || source.get()["removed_fields"]
                        .as_array()
                        .is_some_and(|v| !v.is_empty()))
        }))
        .on_press(move || {
            if busy.get_untracked() || !errors.get_untracked().is_empty() {
                return;
            }
            let context_async = context.clone();
            let document = draft.get();
            let expected = revision.get();
            ui::set(busy, true);
            ui::set(error, String::new());
            context.spawn(async move {
                match context_async
                    .rpc
                    .call::<Value>(
                        "config.set",
                        Some(json!({"config":document,"revision":expected})),
                    )
                    .await
                {
                    Ok(value) => {
                        let saved = value.get("config").cloned().unwrap_or(document);
                        ui::set(source,json!({"config":saved.clone(),"revision":value["revision"].clone(),"removed_fields":[]}));
                        ui::set(draft, saved.clone());
                        ui::set(baseline, saved);
                        ui::set(
                            revision,
                            value["revision"].as_str().unwrap_or_default().to_owned(),
                        );
                        ui::set(
                            message,
                            ui::translate(
                                locale.get(),
                                if value["restart_required"].as_bool().unwrap_or(false) {
                                    "Configuration saved. Restart the gateway to activate changes."
                                } else {
                                    "No configuration changes."
                                },
                            ),
                        );
                    }
                    Err(message) => ui::set(error, message),
                }
                ui::set(busy, false);
            });
        });
    let reload = config.clone();
    let refresh = ui::action(ui::label_for(locale, "Reload configuration"), move || {
        if (draft.get() != baseline.get() || !errors.get().is_empty())
            && !confirmed("Discard unsaved configuration changes?")
        {
            return;
        }
        ui::set(initialized, false);
        ui::set(errors, HashMap::new());
        ui::set(message, String::new());
        reload.refresh();
    });
    let revert = ui::action(ui::label_for(locale, "Discard changes"), move || {
        ui::set(draft, baseline.get());
        ui::set(errors, HashMap::new());
        ui::set(message, String::new());
    });
    let export_context = ctx.clone();
    let export = ui::action(
        ui::label_for(locale, "Export system configuration"),
        move || {
            let context = export_context.clone();
            export_context.spawn(async move {
                match context
                    .rpc
                    .call::<Value>(
                        "config.export",
                        Some(json!({"format":"json","redacted":true})),
                    )
                    .await
                {
                    Ok(value) => crate::utils::download::trigger_download(
                        "savfox-system-config.json",
                        value["content"].as_str().unwrap_or("{}"),
                        "application/json",
                    ),
                    Err(message) => ui::set(error, message),
                }
            });
        },
    );
    let mut links: Vec<Box<dyn Widget>> = Vec::new();
    for (id, label) in std::iter::once(("overview", "Configuration overview"))
        .chain(GROUPS.iter().map(|(id, label, _)| (*id, *label)))
        .chain(std::iter::once(("advanced", "Advanced configuration")))
    {
        let target = if id == "overview" {
            "/config".to_owned()
        } else {
            format!("/config/{id}")
        };
        links.push(Box::new(
            sapid::button()
                .unstyled()
                .class(selected.map(move |current| {
                    if current == id {
                        "btn-plain config-nav-item config-nav-item--active".into()
                    } else {
                        "btn-plain config-nav-item".into()
                    }
                }))
                .label(ui::label_for(locale, label))
                .on_press(move || {
                    ui::set(selected, id.to_owned());
                    ui::set(search, String::new());
                    crate::utils::deep_link::replace_url(&target);
                }),
        ));
    }
    let view_context = ctx.clone();
    let schema_value = schema.value;
    let content = each(ctx.scope.bond(move || {
        vec![(selected.get(), search.get().to_lowercase(), schema_value.get(), locale.get(), initialized.get())]
    }))
    .key(|(section, search, schema, locale, ready)| {
        use std::hash::{Hash, Hasher};
        let mut hash = std::hash::DefaultHasher::new();
        schema.to_string().hash(&mut hash);
        format!("{section}-{search}-{}-{}-{ready}",hash.finish(),locale.code())
    })
    .children(move |each_row| {
        let (section, search, payload, _, ready) = each_row.value.get_untracked();
        let root = payload["schema"].clone();
        let context = view_context.clone();
        widget(move |scope| {
            let context = context.with_scope(scope);
            let mut body: Box<dyn Widget> = if !ready || !root.is_object() {
                    paragraph(ui::label_for(locale,"Loading…"))
                } else if section == "advanced" && search.is_empty() {
                card(ui::label_for(locale,"Advanced configuration"), vec![
                    nav(&context,"Diagnostics and manual RPC","/config/diagnostics"),
                    paragraph(ui::label_for(locale,"Edit the system configuration document. Accounts, channels, speech and scheduled jobs are managed on their own pages. This is not a complete backup.")),
                    json_editor(&context,draft,Vec::new(),root.clone(),errors,ui::label_for(locale,"System configuration JSON")),
                ])
            } else if section == "overview" && search.is_empty() {
                overview(&context,source)
            } else {
                let fields = root["properties"].as_object().into_iter().flat_map(|p|p.iter())
                    .filter(|(key,schema)| {
                        let in_group = GROUPS.iter().filter(|(id,..)| !search.is_empty() || *id == section)
                            .any(|(_,_,fields)| fields.contains(&key.as_str()));
                        in_group && matches_search(key,schema,&root,&search,locale.get())
                    })
                    .map(|(key,schema)|field(&context,draft,vec![key.clone()],schema,&root,errors,0))
                    .collect::<Vec<_>>();
                if fields.is_empty() {paragraph(ui::label_for(locale,"No matching configuration fields"))}
                else {Box::new(col().class("config-fields").fill(fields))}
            };
            body.build(scope)?;
            scope.retain(body)
        })
    });
    Box::new(
        row()
            .class("gateway-page management-page config-page config-layout")
            .fill((
                col().class("config-sidebar").fill((
                    col().class("config-sidebar__header").fill(
                        text()
                            .class("page-title")
                            .content(ui::label_for(locale, "System configuration")),
                    ),
                    col().class("config-sidebar__search").fill(ui::field(
                        ui::label_for(locale, "Search configuration fields"),
                        search,
                        false,
                    )),
                    col().class("config-nav").fill(links),
                )),
                col().class("config-main").fill((
                    row().class("config-actions").fill((
                        refresh,
                        export,
                        revert,
                        Box::new(save) as Box<dyn Widget>,
                    )),
                    col().class("config-scroll").fill((
                        text()
                            .class("page-title config-section-title")
                            .kind(TextKind::Heading(1))
                            .content(ctx.scope.bond(move || {
                                let section = selected.get();
                                let title = if !search.get().is_empty() {
                                    "Search results"
                                } else if section == "overview" {
                                    "Configuration overview"
                                } else if section == "advanced" {
                                    "Advanced configuration"
                                } else {
                                    GROUPS
                                        .iter()
                                        .find(|(id, ..)| *id == section)
                                        .map(|(_, title, _)| *title)
                                        .unwrap_or("System configuration")
                                };
                                ui::translate(locale.get(), title)
                            })),
                        status(ctx, &config),
                        status(ctx, &schema),
                        text().class("form-help").content(ui::label_for(
                            locale,
                            "Configuration changes take effect after restarting the gateway.",
                        )),
                        text().class("form-status").content(message),
                        text()
                            .class("config-dirty-status")
                            .content(ctx.scope.bond(move || {
                                ui::translate(
                                    locale.get(),
                                    if draft.get() != baseline.get() {
                                        "Unsaved configuration changes"
                                    } else {
                                        "No unsaved changes"
                                    },
                                )
                            })),
                        content,
                        ui::details(
                            "config-review",
                            ui::label_for(locale, "Review changes"),
                            Box::new(text().class("json-output").content(ctx.scope.bond(
                                move || {
                                    let original = baseline
                                        .get()
                                        .as_object()
                                        .cloned()
                                        .unwrap_or_default()
                                        .into_iter()
                                        .collect();
                                    let current = draft
                                        .get()
                                        .as_object()
                                        .cloned()
                                        .unwrap_or_default()
                                        .into_iter()
                                        .collect();
                                    compute_deep_diff(&original, &current)
                                        .iter()
                                        .map(|e| format!("{:?}: {}", e.kind, e.path))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                },
                            ))),
                        ),
                    )),
                )),
            )),
    )
}

fn overview(ctx: &AppContext, source: Cage<Value>) -> Box<dyn Widget> {
    let locale = ctx.locale;
    card(
        ui::label_for(locale, "Resource management"),
        vec![
            paragraph(ui::label_for(
                locale,
                "Global runtime policies live here. Use the dedicated management pages for accounts, agents, channels, skills and scheduled jobs.",
            )),
            Box::new(
                row().class("actions").fill(
                    [
                        ("Models and accounts", "/models"),
                        ("Agents", "/agents"),
                        ("Channels", "/channels"),
                        ("Skills", "/skills"),
                        ("Cron Jobs", "/cron"),
                        ("Speech", "/speech"),
                        ("Personal preferences", "/settings"),
                    ]
                    .into_iter()
                    .map(|(label, path)| nav(ctx, label, path))
                    .collect::<Vec<_>>(),
                ),
            ),
            Box::new(text().class("form-help").content(source.map(move |value| {
                let fields = value["removed_fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>();
                if fields.is_empty() {
                    String::new()
                } else {
                    format!(
                        "{}: {}",
                        ui::translate(locale.get(), "Obsolete fields will be removed when saving"),
                        fields.join(", ")
                    )
                }
            }))),
        ],
    )
}

fn set_error(errors: Errors, path: &str, message: Option<String>) {
    let mut entries = errors.get();
    if let Some(message) = message {
        entries.insert(path.into(), message);
    } else {
        entries.remove(path);
    }
    ui::set(errors, entries);
}

fn unset(draft: Cage<Value>, path: &[String]) {
    let mut document = draft.get();
    if let Some(parent) = document
        .pointer_mut(&pointer(&path[..path.len() - 1]))
        .and_then(Value::as_object_mut)
    {
        parent.remove(path.last().expect("field path"));
    }
    ui::set(draft, document);
}

pub(super) fn field(
    ctx: &AppContext,
    draft: Cage<Value>,
    path: Vec<String>,
    schema: &Value,
    root: &Value,
    errors: Errors,
    depth: usize,
) -> Box<dyn Widget> {
    let nullable = schema_is_nullable(schema);
    let ptr = pointer(&path);
    let current = draft
        .get_untracked()
        .pointer(&ptr)
        .cloned()
        .unwrap_or(Value::Null);
    let schema = resolve(schema, root, &current);
    let title = name(
        path.last().map(String::as_str).unwrap_or("Configuration"),
        &schema,
    );
    let kind = schema_primary_type(&schema).unwrap_or_else(|| "object".into());
    if kind == "object"
        && depth < 8
        && schema["additionalProperties"].as_object().is_none()
        && let Some(props) = schema["properties"].as_object()
    {
        let mut children = Vec::new();
        if let Some(description) = schema["description"].as_str() {
            children.push(paragraph(ui::label(ctx, description)));
        }
        for (key, schema) in props {
            let mut child = path.clone();
            child.push(key.clone());
            children.push(field(ctx, draft, child, schema, root, errors, depth + 1));
        }
        return card(ui::label(ctx, title), children);
    }
    if kind == "object" || kind == "array" {
        return json_editor(
            ctx,
            draft,
            path,
            schema.clone(),
            errors,
            ui::label(ctx, title),
        );
    }
    let checkbox = kind == "boolean" && !nullable;
    let key = ptr.clone();
    let control: Box<dyn Widget> = if kind == "boolean" && nullable {
        Box::new(
            crate::ui::select_control(select_key(&format!("config-{ptr}")))
                .label(ui::label(ctx, title.clone()))
                .value(draft.map(move |v| {
                    Some(
                        v.pointer(&key)
                            .and_then(Value::as_bool)
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "inherit".into()),
                    )
                }))
                .options(
                    [
                        ("inherit", "Use default"),
                        ("true", "Enabled"),
                        ("false", "Disabled"),
                    ]
                    .into_iter()
                    .map(|(key, label)| {
                        sapid::select_option(key, ui::translate(ctx.locale.get(), label)).value(key)
                    })
                    .collect::<Vec<_>>(),
                )
                .on_value_change(move |key| match key.as_deref() {
                    Some("true") => update(draft, &path, json!(true)),
                    Some("false") => update(draft, &path, json!(false)),
                    _ => unset(draft, &path),
                }),
        )
    } else if kind == "boolean" {
        let default = schema["default"].as_bool().unwrap_or(false);
        Box::new(
            sapid::checkbox()
                .label(ui::label(ctx, title.clone()))
                .checked(
                    draft.map(move |v| v.pointer(&key).and_then(Value::as_bool).unwrap_or(default)),
                )
                .on_checked_change(move |value| update(draft, &path, json!(value))),
        )
    } else if let Some(options) = schema["enum"].as_array() {
        let values = std::rc::Rc::new(options.clone());
        let read = values.clone();
        let mut choices = options
            .iter()
            .enumerate()
            .map(|(i, v)| {
                sapid::select_option(
                    format!("option-{i}"),
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string()),
                )
                .value(v.to_string())
            })
            .collect::<Vec<_>>();
        if nullable {
            choices.insert(
                0,
                sapid::select_option("inherit", ui::translate(ctx.locale.get(), "Use default"))
                    .value("null"),
            );
        }
        Box::new(
            crate::ui::select_control(select_key(&format!("config-{ptr}")))
                .label(ui::label(ctx, title.clone()))
                .value(draft.map(move |v| {
                    v.pointer(&key)
                        .and_then(|v| read.iter().position(|option| option == v))
                        .map(|i| format!("option-{i}"))
                        .or_else(|| nullable.then(|| "inherit".to_owned()))
                }))
                .options(choices)
                .on_value_change(move |key| {
                    if key.as_deref() == Some("inherit") {
                        unset(draft, &path);
                        return;
                    }
                    if let Some(value) = key
                        .and_then(|k| {
                            k.strip_prefix("option-")
                                .and_then(|i| i.parse::<usize>().ok())
                        })
                        .and_then(|i| values.get(i))
                    {
                        update(draft, &path, value.clone());
                    }
                }),
        )
    } else {
        let default_hint = schema
            .get("default")
            .filter(|v| !v.is_null())
            .map(|v| {
                format!(
                    "{}: {}",
                    ui::translate(ctx.locale.get(), "Default"),
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                )
            })
            .unwrap_or_else(|| ui::translate(ctx.locale.get(), "Use default"));
        let value = draft.map(move |v| match v.pointer(&key) {
            Some(Value::String(s)) => s.clone(),
            None | Some(Value::Null) => String::new(),
            Some(v) => v.to_string(),
        });
        let error_key = ptr.clone();
        let check = schema.clone();
        let secret = path_is_secret(&ptr);
        let change = move |text: String| {
            if text.is_empty() {
                let mut document = draft.get();
                if let Some(parent) = document
                    .pointer_mut(&pointer(&path[..path.len() - 1]))
                    .and_then(Value::as_object_mut)
                {
                    parent.remove(path.last().unwrap());
                }
                ui::set(draft, document);
                set_error(errors, &error_key, None);
                return;
            }
            let parsed = match kind.as_str() {
                "integer" => text
                    .parse::<i64>()
                    .map(|v| json!(v))
                    .map_err(|_| "Expected an integer".to_owned()),
                "number" => text
                    .parse::<f64>()
                    .ok()
                    .filter(|v| v.is_finite())
                    .map(|v| json!(v))
                    .ok_or("Expected a finite number".into()),
                _ => Ok(json!(text)),
            };
            match parsed {
                Ok(value) => {
                    if let Some(message) = field_validation_error(&check, Some(&value)) {
                        set_error(errors, &error_key, Some(message));
                    } else {
                        set_error(errors, &error_key, None);
                        update(draft, &path, value);
                    }
                }
                Err(message) => set_error(errors, &error_key, Some(message)),
            }
        };
        if secret {
            Box::new(
                sapid::password_input()
                    .name(ui::label(ctx, title.clone()))
                    .value(value)
                    .on_value_change(change),
            )
        } else {
            Box::new(
                sapid::text_field()
                    .class("form-input")
                    .name(ui::label(ctx, title.clone()))
                    .placeholder(default_hint)
                    .value(value)
                    .on_value_change(change),
            )
        }
    };
    let error_key = ptr;
    Box::new(col().class("form-group form-field").fill(
        (
            text().class("form-label").content(if checkbox {
                String::new().into()
            } else {
                ui::label(ctx, title)
            }),
            control,
            text().class("form-help").content(ui::label(
                ctx,
                schema["description"].as_str().unwrap_or_default(),
            )),
            text().class("error-message").content(
                errors.map(move |errors| errors.get(&error_key).cloned().unwrap_or_default()),
            ),
        ),
    ))
}

fn path_is_secret(path: &str) -> bool {
    path.split('/')
        .any(|part| matches!(part, "token" | "api_key" | "password" | "secret"))
}

fn json_editor(
    ctx: &AppContext,
    draft: Cage<Value>,
    path: Vec<String>,
    schema: Value,
    errors: Errors,
    label: impl Into<Vary<String>>,
) -> Box<dyn Widget> {
    let label = label.into();
    let ptr = pointer(&path);
    let empty = if schema_primary_type(&schema).as_deref() == Some("array") {
        json!([])
    } else {
        json!({})
    };
    let current = draft
        .get_untracked()
        .pointer(&ptr)
        .cloned()
        .unwrap_or_else(|| empty.clone());
    let raw = ctx
        .scope
        .cage(serde_json::to_string_pretty(&current).unwrap_or_default());
    let observed = ptr.clone();
    ctx.scope.effect(move || {
        let document = draft.get();
        let value = document
            .pointer(&observed)
            .cloned()
            .unwrap_or_else(|| empty.clone());
        if !errors.get_untracked().contains_key(&observed) {
            ui::set(
                raw,
                serde_json::to_string_pretty(&value).unwrap_or_default(),
            );
        }
    });
    let error_key = ptr.clone();
    let input_key = ptr;
    Box::new(
        col().class("form-group form-field").fill((
            text().class("form-label").content(label.clone()),
            sapid::text_field()
                .class("form-input config-json-editor")
                .name(label)
                .multiline(true)
                .value(raw)
                .on_value_change(move |text| {
                    ui::set(raw, text.clone());
                    match serde_json::from_str::<Value>(&text) {
                        Ok(value) => {
                            let validation = if path.is_empty() {
                                if value.is_object() {
                                    None
                                } else {
                                    Some("Configuration must be a JSON object".into())
                                }
                            } else {
                                let target = &schema;
                                field_validation_error(target, Some(&value))
                            };
                            if let Some(message) = validation {
                                set_error(errors, &input_key, Some(message));
                            } else {
                                set_error(errors, &input_key, None);
                                if path.is_empty() {
                                    ui::set(draft, value);
                                } else {
                                    update(draft, &path, value);
                                }
                            }
                        }
                        Err(error) => set_error(errors, &input_key, Some(error.to_string())),
                    }
                }),
            text().class("error-message").content(
                errors.map(move |errors| errors.get(&error_key).cloned().unwrap_or_default()),
            ),
        )),
    )
}
