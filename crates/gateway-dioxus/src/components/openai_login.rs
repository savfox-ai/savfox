use dioxus::prelude::*;
use serde_json::json;

use crate::api::ws::WsRpc;

#[derive(Clone, Debug)]
pub enum OpenAILoginState {
    PickMethod,
    ChatGPTWaiting {
        auth_url: String,
    },
    DeviceCodeWaiting {
        verification_url: String,
        user_code: String,
    },
    ApiKeyInput,
    Success {
        account_email: Option<String>,
        plan_type: Option<String>,
    },
    Error {
        message: String,
    },
}

#[component]
pub fn OpenAILogin(
    ws: WsRpc,
    on_success: EventHandler<()>,
    on_cancel: Option<EventHandler<()>>,
    compact: bool,
) -> Element {
    inject_openai_login_styles_once();
    let mut login_state = use_signal(|| OpenAILoginState::PickMethod);
    let mut api_key = use_signal(String::new);
    let mut loading = use_signal(|| false);
    let mut login_id = use_signal(String::new);

    // Auto-poll for auth completion while in a waiting state.
    {
        let ws = ws.clone();
        let on_success = on_success.clone();
        use_future(move || {
            let ws = ws.clone();
            let on_success = on_success.clone();
            async move {
                loop {
                    crate::utils::sleep_ms(2000).await;
                    match login_state() {
                        // Actively waiting for the user to complete auth — poll below.
                        OpenAILoginState::ChatGPTWaiting { .. }
                        | OpenAILoginState::DeviceCodeWaiting { .. } => {}
                        // Terminal states: auth resolved, stop the polling loop.
                        OpenAILoginState::Success { .. } | OpenAILoginState::Error { .. } => break,
                        // Transient pre-waiting states (method picker, API key entry):
                        // keep the loop alive until the user enters a waiting state.
                        OpenAILoginState::PickMethod | OpenAILoginState::ApiKeyInput => continue,
                    }
                    if let Ok(resp) = ws
                        .call::<serde_json::Value>(
                            "account/read",
                            Some(json!({"refreshToken": true})),
                        )
                        .await
                    {
                        if let Some(account) = resp.get("account") {
                            let account_type =
                                account.get("type").and_then(|v| v.as_str()).unwrap_or("");
                            if account_type == "chatgpt" {
                                let email = account
                                    .get("email")
                                    .and_then(|v| v.as_str())
                                    .map(ToString::to_string);
                                let plan_type = account
                                    .get("planType")
                                    .and_then(|v| v.as_str())
                                    .map(ToString::to_string);
                                login_state.set(OpenAILoginState::Success {
                                    account_email: email,
                                    plan_type,
                                });
                                on_success.call(());
                                return;
                            }
                        }
                    }
                }
            }
        });
    }

    let state = login_state();
    let is_loading = loading();

    rsx! {
        div { class: if compact { "openai-login openai-login--compact" } else { "openai-login" },
            match state {
                OpenAILoginState::PickMethod => rsx! {
                    div { class: "openai-login__header",
                        if !compact {
                            h3 { class: "openai-login__title", "Sign in to OpenAI" }
                            p { class: "openai-login__subtitle", "Choose how you want to authenticate" }
                        }
                    }
                    div { class: "openai-login__options",
                        button {
                            class: "openai-login__option",
                            onclick: {
                                let ws = ws.clone();
                                move |_| {
                                    loading.set(true);
                                    let ws = ws.clone();
                                    spawn(async move {
                                        match ws.call::<serde_json::Value>("account/login/start", Some(json!({
                                            "type": "chatgpt",
                                            "openBrowser": false
                                        }))).await {
                                            Ok(resp) => {
                                                if let Some(auth_url) = resp.get("authUrl").and_then(|v| v.as_str()) {
                                                    if let Some(id) = resp.get("loginId").and_then(|v| v.as_str()) {
                                                        login_id.set(id.to_string());
                                                    }
                                                    login_state.set(OpenAILoginState::ChatGPTWaiting {
                                                        auth_url: auth_url.to_string(),
                                                    });
                                                } else {
                                                    login_state.set(OpenAILoginState::Error {
                                                        message: "Failed to get auth URL".to_string(),
                                                    });
                                                }
                                            }
                                            Err(e) => {
                                                login_state.set(OpenAILoginState::Error {
                                                    message: format!("Failed to start login: {}", e),
                                                });
                                            }
                                        }
                                        loading.set(false);
                                    });
                                }
                            },
                            disabled: is_loading,
                            div { class: "openai-login__option-icon", "C" }
                            div { class: "openai-login__option-content",
                                div { class: "openai-login__option-title", "Sign in with ChatGPT" }
                                if !compact {
                                    div { class: "openai-login__option-desc", "Usage included with Plus, Pro, Team, and Enterprise plans." }
                                }
                            }
                        }
                        button {
                            class: "openai-login__option",
                            onclick: {
                                let ws = ws.clone();
                                move |_| {
                                    loading.set(true);
                                    let ws = ws.clone();
                                    spawn(async move {
                                        match ws.call::<serde_json::Value>("account/login/start", Some(json!({
                                            "type": "deviceCode"
                                        }))).await {
                                            Ok(resp) => {
                                                let verification_url = resp.get("verificationUrl").and_then(|v| v.as_str()).unwrap_or("");
                                                let user_code = resp.get("userCode").and_then(|v| v.as_str()).unwrap_or("");
                                                if !verification_url.is_empty() && !user_code.is_empty() {
                                                    if let Some(id) = resp.get("loginId").and_then(|v| v.as_str()) {
                                                        login_id.set(id.to_string());
                                                    }
                                                    login_state.set(OpenAILoginState::DeviceCodeWaiting {
                                                        verification_url: verification_url.to_string(),
                                                        user_code: user_code.to_string(),
                                                    });
                                                } else {
                                                    login_state.set(OpenAILoginState::Error {
                                                        message: "Device code login not available. Use browser login instead.".to_string(),
                                                    });
                                                }
                                            }
                                            Err(e) => {
                                                login_state.set(OpenAILoginState::Error {
                                                    message: format!("Failed to start device login: {}", e),
                                                });
                                            }
                                        }
                                        loading.set(false);
                                    });
                                }
                            },
                            disabled: is_loading,
                            div { class: "openai-login__option-icon", "D" }
                            div { class: "openai-login__option-content",
                                div { class: "openai-login__option-title", "Sign in with Device Code" }
                                if !compact {
                                    div { class: "openai-login__option-desc", "Sign in from another device with a one-time code." }
                                }
                            }
                        }
                        button {
                            class: "openai-login__option",
                            onclick: move |_| login_state.set(OpenAILoginState::ApiKeyInput),
                            disabled: is_loading,
                            div { class: "openai-login__option-icon", "K" }
                            div { class: "openai-login__option-content",
                                div { class: "openai-login__option-title", "Provide your own API key" }
                                if !compact {
                                    div { class: "openai-login__option-desc", "Pay for what you use." }
                                }
                            }
                        }
                    }
                },
                OpenAILoginState::ChatGPTWaiting { ref auth_url } => rsx! {
                    div { class: "openai-login__header",
                        h3 { class: "openai-login__title", "Complete sign in" }
                    }
                    div { class: "openai-login__waiting",
                        p { class: "openai-login__waiting-text",
                            "Click the link below to sign in with your ChatGPT account."
                        }
                        a {
                            class: "openai-login__auth-link",
                            href: "{auth_url}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            "Open sign in page"
                        }
                        p { class: "openai-login__waiting-hint",
                            "The page will open in a new tab. After signing in, click \"Check status\" below."
                        }
                        div { class: "openai-login__actions",
                            button {
                                class: "openai-login__btn openai-login__btn--primary",
                                onclick: {
                                    let ws = ws.clone();
                                    move |_| {
                                        let ws = ws.clone();
                                        spawn(async move {
                                            match ws.call::<serde_json::Value>("account/read", Some(json!({
                                                "refreshToken": true
                                            }))).await {
                                                Ok(resp) => {
                                                    if let Some(account) = resp.get("account") {
                                                        let account_type = account.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                                        if account_type == "chatgpt" {
                                                            let email = account.get("email").and_then(|v| v.as_str()).map(ToString::to_string);
                                                            let plan_type = account.get("planType").and_then(|v| v.as_str()).map(ToString::to_string);
                                                            login_state.set(OpenAILoginState::Success { account_email: email, plan_type });
                                                            on_success.call(());
                                                        }
                                                    }
                                                }
                                                Err(_) => {}
                                            }
                                        });
                                    }
                                },
                                disabled: is_loading,
                                if is_loading { "Checking..." } else { "Check status" }
                            }
                            button {
                                class: "openai-login__btn openai-login__btn--secondary",
                                onclick: {
                                    let ws = ws.clone();
                                    move |_| {
                                        let current_login_id = login_id();
                                        if !current_login_id.is_empty() {
                                            let ws = ws.clone();
                                            let id = current_login_id.clone();
                                            spawn(async move {
                                                let _ = ws.call::<serde_json::Value>("account/login/cancel", Some(json!({
                                                    "loginId": id
                                                }))).await;
                                            });
                                        }
                                        login_state.set(OpenAILoginState::PickMethod);
                                        loading.set(false);
                                        login_id.set(String::new());
                                        if let Some(handler) = on_cancel.as_ref() {
                                            handler.call(());
                                        }
                                    }
                                },
                                "Cancel"
                            }
                        }
                    }
                },
                OpenAILoginState::DeviceCodeWaiting { ref verification_url, ref user_code } => rsx! {
                    div { class: "openai-login__header",
                        h3 { class: "openai-login__title", "Sign in with device code" }
                    }
                    div { class: "openai-login__device-code",
                        p { class: "openai-login__device-code-text",
                            "Open the link below and enter this code:"
                        }
                        div { class: "openai-login__code-display",
                            "{user_code}"
                        }
                        a {
                            class: "openai-login__auth-link",
                            href: "{verification_url}",
                            target: "_blank",
                            rel: "noopener noreferrer",
                            "Open verification page"
                        }
                        p { class: "openai-login__device-code-hint",
                            "Code expires in 15 minutes"
                        }
                        div { class: "openai-login__actions",
                            button {
                                class: "openai-login__btn openai-login__btn--primary",
                                onclick: {
                                    let ws = ws.clone();
                                    move |_| {
                                        let ws = ws.clone();
                                        spawn(async move {
                                            match ws.call::<serde_json::Value>("account/read", Some(json!({
                                                "refreshToken": true
                                            }))).await {
                                                Ok(resp) => {
                                                    if let Some(account) = resp.get("account") {
                                                        let account_type = account.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                                        if account_type == "chatgpt" {
                                                            let email = account.get("email").and_then(|v| v.as_str()).map(ToString::to_string);
                                                            let plan_type = account.get("planType").and_then(|v| v.as_str()).map(ToString::to_string);
                                                            login_state.set(OpenAILoginState::Success { account_email: email, plan_type });
                                                            on_success.call(());
                                                        }
                                                    }
                                                }
                                                Err(_) => {}
                                            }
                                        });
                                    }
                                },
                                disabled: is_loading,
                                if is_loading { "Checking..." } else { "Check status" }
                            }
                            button {
                                class: "openai-login__btn openai-login__btn--secondary",
                                onclick: {
                                    let ws = ws.clone();
                                    move |_| {
                                        let current_login_id = login_id();
                                        if !current_login_id.is_empty() {
                                            let ws = ws.clone();
                                            let id = current_login_id.clone();
                                            spawn(async move {
                                                let _ = ws.call::<serde_json::Value>("account/login/cancel", Some(json!({
                                                    "loginId": id
                                                }))).await;
                                            });
                                        }
                                        login_state.set(OpenAILoginState::PickMethod);
                                        loading.set(false);
                                        login_id.set(String::new());
                                        if let Some(handler) = on_cancel.as_ref() {
                                            handler.call(());
                                        }
                                    }
                                },
                                "Cancel"
                            }
                        }
                    }
                },
                OpenAILoginState::ApiKeyInput => rsx! {
                    div { class: "openai-login__header",
                        h3 { class: "openai-login__title", "Enter API Key" }
                    }
                    div { class: "openai-login__api-key",
                        p { class: "openai-login__api-key-text",
                            "Enter your OpenAI API key. It will be stored locally in your configuration."
                        }
                        input {
                            r#type: "password",
                            class: "openai-login__input",
                            placeholder: "sk-...",
                            value: "{api_key}",
                            oninput: move |e| api_key.set(e.value()),
                            autofocus: true,
                        }
                        div { class: "openai-login__actions",
                            button {
                                class: "openai-login__btn openai-login__btn--primary",
                                onclick: {
                                    let ws = ws.clone();
                                    move |_| {
                                        let key = api_key().trim().to_string();
                                        if key.is_empty() {
                                            return;
                                        }
                                        loading.set(true);
                                        let ws = ws.clone();
                                        spawn(async move {
                                            match ws.call::<serde_json::Value>("account/login/start", Some(json!({
                                                "type": "apiKey",
                                                "apiKey": key
                                            }))).await {
                                                Ok(resp) => {
                                                    let account_type = resp.get("type").and_then(|v| v.as_str()).unwrap_or("");
                                                    if account_type == "apiKey" {
                                                        login_state.set(OpenAILoginState::Success {
                                                            account_email: None,
                                                            plan_type: Some("Usage-based".to_string()),
                                                        });
                                                        on_success.call(());
                                                    } else {
                                                        login_state.set(OpenAILoginState::Error {
                                                            message: "Failed to save API key".to_string(),
                                                        });
                                                    }
                                                }
                                                Err(e) => {
                                                    login_state.set(OpenAILoginState::Error {
                                                        message: format!("Failed to save API key: {}", e),
                                                    });
                                                }
                                            }
                                            loading.set(false);
                                        });
                                    }
                                },
                                disabled: api_key().trim().is_empty() || is_loading,
                                if is_loading { "Saving..." } else { "Save API Key" }
                            }
                            button {
                                class: "openai-login__btn openai-login__btn--secondary",
                                onclick: move |_| {
                                    login_state.set(OpenAILoginState::PickMethod);
                                    loading.set(false);
                                    if let Some(handler) = on_cancel.as_ref() {
                                        handler.call(());
                                    }
                                },
                                "Back"
                            }
                        }
                    }
                },
                OpenAILoginState::Success { ref account_email, ref plan_type } => rsx! {
                    div { class: "openai-login__header",
                        h3 { class: "openai-login__title openai-login__title--success", "Signed in successfully" }
                    }
                    div { class: "openai-login__success",
                        div { class: "openai-login__success-icon", "OK" }
                        if let Some(email) = account_email {
                            p { class: "openai-login__success-email", "{email}" }
                        }
                        if let Some(plan) = plan_type {
                            p { class: "openai-login__success-plan", "Plan: {plan}" }
                        }
                    }
                },
                OpenAILoginState::Error { ref message } => rsx! {
                    div { class: "openai-login__header",
                        h3 { class: "openai-login__title openai-login__title--error", "Sign in failed" }
                    }
                    div { class: "openai-login__error",
                        p { class: "openai-login__error-message", "{message}" }
                        button {
                            class: "openai-login__btn openai-login__btn--secondary",
                            onclick: move |_| {
                                login_state.set(OpenAILoginState::PickMethod);
                                loading.set(false);
                            },
                            "Try again"
                        }
                    }
                },
            }
        }
    }
}

const OPENAI_LOGIN_STYLES: &str = r#"
    .openai-login {
        display: flex;
        flex-direction: column;
        gap: 20px;
        padding: 24px;
        background: var(--bg-secondary);
        border-radius: var(--radius);
        border: 1px solid var(--border);
    }

    .openai-login--compact {
        padding: 16px;
        gap: 12px;
    }

    .openai-login__header {
        text-align: center;
    }

    .openai-login__title {
        font-size: 20px;
        font-weight: 700;
        margin: 0 0 8px;
        color: var(--text-primary);
    }

    .openai-login__title--success {
        color: var(--success);
    }

    .openai-login__title--error {
        color: var(--danger);
    }

    .openai-login__subtitle {
        font-size: 14px;
        color: var(--text-muted);
        margin: 0;
    }

    .openai-login__options {
        display: flex;
        flex-direction: column;
        gap: 12px;
    }

    .openai-login__option {
        display: flex;
        align-items: center;
        gap: 16px;
        padding: 16px;
        background: var(--bg-tertiary);
        border: 2px solid var(--border);
        border-radius: var(--radius);
        cursor: pointer;
        text-align: left;
        transition: border-color 0.15s, background 0.15s;
    }

    .openai-login__option:hover {
        border-color: var(--accent);
        background: var(--bg-hover);
    }

    .openai-login__option-icon {
        width: 40px;
        height: 40px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: var(--accent);
        color: #fff;
        border-radius: 10px;
        font-size: 18px;
        font-weight: 700;
        flex-shrink: 0;
    }

    .openai-login__option-content {
        flex: 1;
        min-width: 0;
    }

    .openai-login__option-title {
        font-size: 15px;
        font-weight: 600;
        color: var(--text-primary);
        margin-bottom: 4px;
    }

    .openai-login__option-desc {
        font-size: 13px;
        color: var(--text-muted);
    }

    .openai-login__waiting,
    .openai-login__device-code,
    .openai-login__api-key,
    .openai-login__success,
    .openai-login__error {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 16px;
        text-align: center;
    }

    .openai-login__waiting-text,
    .openai-login__api-key-text {
        font-size: 14px;
        color: var(--text-secondary);
        margin: 0;
    }

    .openai-login__auth-link {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        padding: 12px 24px;
        background: var(--accent);
        color: #fff;
        border-radius: var(--radius);
        text-decoration: none;
        font-weight: 600;
        font-size: 14px;
        transition: opacity 0.15s;
    }

    .openai-login__auth-link:hover {
        opacity: 0.9;
    }

    .openai-login__waiting-hint,
    .openai-login__device-code-hint {
        font-size: 12px;
        color: var(--text-muted);
        margin: 0;
    }

    .openai-login__code-display {
        font-family: monospace;
        font-size: 24px;
        font-weight: 700;
        letter-spacing: 4px;
        padding: 16px 32px;
        background: var(--bg-tertiary);
        border: 2px dashed var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
    }

    .openai-login__input {
        width: 100%;
        max-width: 400px;
        padding: 12px 16px;
        background: var(--bg-tertiary);
        border: 1px solid var(--border);
        border-radius: var(--radius);
        color: var(--text-primary);
        font-size: 14px;
        outline: none;
        transition: border-color 0.15s;
    }

    .openai-login__input:focus {
        border-color: var(--accent);
    }

    .openai-login__input::placeholder {
        color: var(--text-muted);
    }

    .openai-login__actions {
        display: flex;
        gap: 12px;
        margin-top: 8px;
    }

    .openai-login__btn {
        padding: 10px 20px;
        border-radius: var(--radius);
        font-size: 14px;
        font-weight: 600;
        cursor: pointer;
        transition: opacity 0.15s;
        border: none;
    }

    .openai-login__btn:disabled {
        opacity: 0.6;
        cursor: not-allowed;
    }

    .openai-login__btn--primary {
        background: var(--button-cta-surface);
        color: #fff;
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow);
    }

    .openai-login__btn--secondary {
        background: var(--bg-tertiary);
        color: var(--text-primary);
        border: 1px solid var(--border);
    }

    .openai-login__btn:not(:disabled):hover {
        opacity: 1;
    }

    .openai-login__btn--primary:not(:disabled):hover {
        background: var(--button-cta-surface-hover);
        box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow-hover);
    }

    .openai-login__success-icon {
        width: 48px;
        height: 48px;
        display: flex;
        align-items: center;
        justify-content: center;
        background: var(--success);
        color: #fff;
        border-radius: 50%;
        font-size: 18px;
        font-weight: 700;
    }

    .openai-login__success-email {
        font-size: 14px;
        color: var(--text-primary);
        margin: 0;
    }

    .openai-login__success-plan {
        font-size: 13px;
        color: var(--text-muted);
        margin: 0;
    }

    .openai-login__error-message {
        font-size: 14px;
        color: var(--danger);
        margin: 0;
    }

    @media (max-width: 480px) {
        .openai-login {
            padding: 16px;
        }

        .openai-login__title {
            font-size: 18px;
        }

        .openai-login__option {
            padding: 12px;
            gap: 12px;
        }

        .openai-login__option-icon {
            width: 36px;
            height: 36px;
            font-size: 16px;
        }

        .openai-login__code-display {
            font-size: 20px;
            letter-spacing: 2px;
            padding: 12px 20px;
        }
    }
"#;

/// Injects the OpenAI-login stylesheet into the document head exactly once,
/// instead of emitting an inline `<style>` block on every render.
fn inject_openai_login_styles_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        if let Some(doc) = web_sys::window().and_then(|w| w.document()) {
            if let Ok(el) = doc.create_element("style") {
                el.set_inner_html(OPENAI_LOGIN_STYLES);
                if let Some(head) = doc.head() {
                    let _ = head.append_child(&el);
                }
            }
        }
    });
}
