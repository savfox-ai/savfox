use dioxus::prelude::*;

use crate::api::client::validate_token;
use crate::api::ws::set_token;

#[component]
pub fn AuthGate(on_authenticated: EventHandler<()>) -> Element {
    let mut token_input = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut loading = use_signal(|| false);

    let handle_submit = move |e: Event<FormData>| {
        e.prevent_default();
        let val = token_input().trim().to_string();
        if val.is_empty() || loading() {
            return;
        }
        loading.set(true);
        error.set(String::new());
        spawn(async move {
            if validate_token(&val).await {
                set_token(&val);
                on_authenticated(());
            } else {
                error.set("Invalid token".into());
            }
            loading.set(false);
        });
    };

    let has_error = !error().is_empty();
    let input_class = if has_error {
        "auth-input error"
    } else {
        "auth-input"
    };
    let btn_disabled = loading() || token_input().trim().is_empty();
    let btn_class = if btn_disabled {
        "auth-btn disabled"
    } else {
        "auth-btn"
    };

    rsx! {
        div { class: "auth-container",
            div { class: "auth-card",
                div { class: "auth-logo",
                    img { src: "assets/savfox.svg", alt: "Savfox", class: "auth-logo-img" }
                }
                h1 { class: "auth-title", "Savfox Gateway" }
                p { class: "auth-subtitle", "Enter your gateway token to continue" }
                form { class: "auth-form", onsubmit: handle_submit,
                    input {
                        r#type: "password",
                        class: "{input_class}",
                        value: "{token_input}",
                        oninput: move |e| token_input.set(e.value()),
                        placeholder: "Gateway token",
                        autofocus: true,
                    }
                    if has_error {
                        p { class: "auth-error", "{error}" }
                    }
                    button {
                        r#type: "submit",
                        class: "{btn_class}",
                        disabled: btn_disabled,
                        if loading() { "Validating..." } else { "Connect" }
                    }
                }
            }
        }
        style { {r#"
            .auth-container {
                display: flex;
                align-items: center;
                justify-content: center;
                min-height: 100vh;
                padding: 16px;
                background: var(--bg-primary);
            }
            
            .auth-card {
                width: 100%;
                max-width: 400px;
                padding: 32px;
                background: var(--bg-secondary);
                border-radius: var(--radius);
                border: 1px solid var(--border);
            }
            
            .auth-logo {
                display: flex;
                justify-content: center;
                margin-bottom: 16px;
            }
            
            .auth-logo-img {
                width: 48px;
                height: 48px;
            }
            
            .auth-title {
                font-size: 24px;
                margin-bottom: 8px;
                text-align: center;
            }
            
            .auth-subtitle {
                color: var(--text-secondary);
                margin-bottom: 24px;
                text-align: center;
                font-size: 14px;
            }
            
            .auth-form {
                display: flex;
                flex-direction: column;
            }
            
            .auth-input {
                width: 100%;
                padding: 12px 14px;
                background: var(--bg-tertiary);
                border: 1px solid var(--border);
                border-radius: var(--radius);
                color: var(--text-primary);
                outline: none;
                font-size: 14px;
                transition: border-color 0.15s ease;
            }
            
            .auth-input:focus {
                border-color: var(--accent);
            }
            
            .auth-input.error {
                border-color: var(--danger);
            }
            
            .auth-input::placeholder {
                color: var(--text-muted);
            }
            
            .auth-error {
                color: var(--danger);
                font-size: 13px;
                margin: 8px 0;
                text-align: center;
            }
            
            .auth-btn {
                width: 100%;
                padding: 12px 14px;
                margin-top: 16px;
                background: var(--accent);
                color: #fff;
                border: none;
                border-radius: var(--radius);
                font-weight: 500;
                font-size: 14px;
                cursor: pointer;
                transition: opacity 0.15s ease, transform 0.15s ease;
            }
            
            .auth-btn:active {
                transform: scale(0.98);
            }
            
            .auth-btn.disabled {
                opacity: 0.6;
                cursor: not-allowed;
            }
            
            @media screen and (max-width: 480px) {
                .auth-container {
                    padding: 12px;
                }
                
                .auth-card {
                    padding: 24px 20px;
                }
                
                .auth-logo-img {
                    width: 40px;
                    height: 40px;
                }
                
                .auth-title {
                    font-size: 20px;
                }
                
                .auth-subtitle {
                    font-size: 13px;
                    margin-bottom: 20px;
                }
                
                .auth-input {
                    padding: 14px;
                    font-size: 16px;
                }
                
                .auth-btn {
                    padding: 14px;
                    font-size: 16px;
                }
            }
            
            @media (hover: none) and (pointer: coarse) {
                .auth-input {
                    font-size: 16px;
                }
            }
        "#} }
    }
}
