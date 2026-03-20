use dioxus::prelude::*;

use crate::api::client::validate_token;
use crate::api::ws::set_token;
use crate::i18n::{self, Locale, save_locale, use_i18n};

const SAVFOX_LOGO: Asset = asset!("/assets/savfox.svg");

#[component]
pub fn AuthGate(on_authenticated: EventHandler<()>) -> Element {
    let (mut locale_sig, t) = use_i18n();
    let mut token_input = use_signal(String::new);
    let mut error = use_signal(String::new);
    let mut loading = use_signal(|| false);

    let error_key = i18n::t(locale_sig(), "auth.invalid_token");
    let handle_submit = move |e: Event<FormData>| {
        e.prevent_default();
        let val = token_input().trim().to_string();
        if val.is_empty() || loading() {
            return;
        }
        loading.set(true);
        error.set(String::new());
        let error_msg = error_key.clone();
        spawn(async move {
            if validate_token(&val).await {
                set_token(&val);
                on_authenticated(());
            } else {
                error.set(error_msg);
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

    let auth_title = t("auth.title");
    let auth_subtitle = t("auth.subtitle");
    let auth_placeholder = t("auth.placeholder");
    let auth_validating = t("auth.validating");
    let auth_connect = t("auth.connect");

    rsx! {
        div { class: "auth-container",
            div { class: "auth-card",
                div { class: "auth-logo",
                    img { src: SAVFOX_LOGO, alt: "Savfox", class: "auth-logo-img" }
                }
                // Language switcher
                div { class: "auth-lang-switcher",
                    for locale_option in Locale::ALL {
                        button {
                            class: if *locale_option == locale_sig() { "auth-lang-btn active" } else { "auth-lang-btn" },
                            onclick: move |_| {
                                locale_sig.set(*locale_option);
                                save_locale(*locale_option);
                            },
                            "{locale_option.label()}"
                        }
                    }
                }
                h1 { class: "auth-title", "{auth_title}" }
                p { class: "auth-subtitle", "{auth_subtitle}" }
                form { class: "auth-form", onsubmit: handle_submit,
                    input {
                        r#type: "password",
                        class: "{input_class}",
                        value: "{token_input}",
                        oninput: move |e| token_input.set(e.value()),
                        placeholder: "{auth_placeholder}",
                        autofocus: true,
                    }
                    if has_error {
                        p { class: "auth-error", "{error}" }
                    }
                    button {
                        r#type: "submit",
                        class: "{btn_class}",
                        disabled: btn_disabled,
                        if loading() { "{auth_validating}" } else { "{auth_connect}" }
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
                background: var(--button-primary-surface);
                color: #fff;
                border: none;
                border-radius: var(--radius);
                font-weight: 500;
                font-size: 14px;
                cursor: pointer;
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-primary-shadow);
                transition: opacity 0.15s ease, transform 0.15s ease, box-shadow 0.2s ease, background 0.15s ease;
            }

            .auth-btn:hover:not(.disabled) {
                background: var(--button-primary-surface-hover);
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-primary-shadow-hover);
                transform: translateY(-1px);
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
            
            .auth-lang-switcher {
                display: flex;
                justify-content: center;
                gap: 4px;
                margin-bottom: 12px;
            }

            .auth-lang-btn {
                padding: 4px 12px;
                border: 1px solid var(--border);
                border-radius: 6px;
                background: var(--button-surface);
                color: var(--text-secondary);
                font-size: 13px;
                cursor: pointer;
                box-shadow: inset 0 1px 0 var(--button-highlight), var(--button-soft-shadow);
                transition: all 0.15s ease;
            }

            .auth-lang-btn:hover {
                background: var(--button-surface-hover);
                border-color: color-mix(in srgb, var(--border) 72%, var(--accent) 28%);
                color: var(--text-primary);
                box-shadow: inset 0 1px 0 var(--button-highlight), var(--button-soft-shadow-hover);
                transform: translateY(-1px);
            }

            .auth-lang-btn.active {
                background: var(--button-primary-surface);
                border-color: transparent;
                color: #fff;
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-primary-shadow);
            }

            @media (hover: none) and (pointer: coarse) {
                .auth-input {
                    font-size: 16px;
                }
            }
        "#} }
    }
}
