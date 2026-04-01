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
                    img { src: SAVFOX_LOGO, alt: "Savfox", class: "auth-logo-img", width: "48", height: "48" }
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
                background:
                    radial-gradient(circle at top, color-mix(in srgb, var(--accent) 10%, transparent) 0%, transparent 34%),
                    linear-gradient(180deg, color-mix(in srgb, var(--bg-secondary) 22%, transparent) 0%, transparent 100%);
            }
            
            .auth-card {
                position: relative;
                overflow: hidden;
                width: 100%;
                max-width: 400px;
                padding: 32px;
                background: var(--surface-panel-strong);
                border-radius: var(--radius-xl);
                border: 1px solid color-mix(in srgb, var(--surface-stroke) 72%, var(--ornament) 28%);
                box-shadow: var(--surface-inner), var(--surface-shadow), var(--surface-glow);
                backdrop-filter: blur(var(--panel-blur)) saturate(150%);
                -webkit-backdrop-filter: blur(var(--panel-blur)) saturate(150%);
            }

            .auth-card::before,
            .auth-card::after {
                content: "";
                position: absolute;
                pointer-events: none;
            }

            .auth-card::before {
                inset: -18% auto auto -12%;
                width: 180px;
                height: 180px;
                background: radial-gradient(circle, color-mix(in srgb, var(--accent) 22%, transparent) 0%, transparent 60%);
                filter: blur(12px);
                opacity: 0.9;
            }

            .auth-card::after {
                inset: 0;
                background: linear-gradient(120deg, rgba(255, 255, 255, 0.06) 0%, transparent 28%, transparent 70%, rgba(255, 255, 255, 0.03) 100%);
                opacity: 0.55;
            }
            
            .auth-logo {
                position: relative;
                z-index: 1;
                display: flex;
                justify-content: center;
                margin-bottom: 18px;
            }
            
            .auth-logo-img {
                width: 48px;
                height: 48px;
                filter:
                    drop-shadow(0 0 14px color-mix(in srgb, var(--accent) 24%, transparent))
                    drop-shadow(0 10px 24px rgba(0, 0, 0, 0.18));
            }
            
            .auth-title {
                position: relative;
                z-index: 1;
                font-family: var(--font-display);
                font-size: 24px;
                font-weight: 600;
                letter-spacing: 0.02em;
                margin-bottom: 8px;
                text-align: center;
                background: linear-gradient(135deg, var(--accent-ember) 0%, var(--accent) 56%, var(--ornament) 100%);
                -webkit-background-clip: text;
                background-clip: text;
                color: transparent;
                -webkit-text-fill-color: transparent;
            }
            
            .auth-subtitle {
                position: relative;
                z-index: 1;
                color: color-mix(in srgb, var(--text-secondary) 88%, var(--ornament) 12%);
                margin-bottom: 24px;
                text-align: center;
                font-size: 14px;
                line-height: 1.6;
            }
            
            .auth-form {
                position: relative;
                z-index: 1;
                display: flex;
                flex-direction: column;
            }
            
            .auth-input {
                width: 100%;
                padding: 12px 14px;
                background: var(--field-surface);
                border: 1px solid var(--field-stroke);
                border-radius: var(--radius);
                color: var(--text-primary);
                outline: none;
                font-size: 14px;
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), var(--surface-shadow-soft);
                transition: border-color 0.15s ease, box-shadow 0.2s ease, background 0.15s ease;
            }
            
            .auth-input:focus {
                background: var(--field-hover);
                border-color: color-mix(in srgb, var(--accent) 76%, var(--field-stroke) 24%);
                box-shadow: var(--field-focus);
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
                min-height: 46px;
                padding: 12px 18px;
                margin-top: 16px;
                background: var(--button-cta-surface);
                color: #fff;
                border: none;
                border-radius: 999px;
                font-weight: 600;
                letter-spacing: 0.04em;
                font-size: 14px;
                cursor: pointer;
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow);
                transition: opacity 0.15s ease, transform 0.15s ease, box-shadow 0.2s ease, background 0.15s ease;
            }

            .auth-btn:hover:not(.disabled) {
                background: var(--button-cta-surface-hover);
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.16), var(--button-cta-shadow-hover);
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
                position: relative;
                z-index: 1;
                display: flex;
                justify-content: center;
                gap: 6px;
                width: fit-content;
                margin: 0 auto 14px;
                padding: 5px;
                border-radius: 999px;
                border: 1px solid color-mix(in srgb, var(--surface-stroke) 66%, transparent);
                background: color-mix(in srgb, var(--surface-flat-soft) 92%, transparent);
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08);
            }

            .auth-lang-btn {
                min-width: 58px;
                padding: 6px 14px;
                border: 1px solid transparent;
                border-radius: 999px;
                background: transparent;
                color: var(--text-secondary);
                font-size: 12px;
                font-weight: 600;
                cursor: pointer;
                box-shadow: none;
                transition: all 0.18s ease;
            }

            .auth-lang-btn:hover {
                background: color-mix(in srgb, var(--accent) 8%, var(--surface-flat-hover) 92%);
                border-color: color-mix(in srgb, var(--surface-stroke) 60%, var(--accent) 40%);
                color: var(--text-primary);
                box-shadow: inset 0 1px 0 rgba(255, 255, 255, 0.08), 0 10px 20px color-mix(in srgb, var(--accent) 8%, transparent);
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
