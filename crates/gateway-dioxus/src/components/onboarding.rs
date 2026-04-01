use dioxus::prelude::*;

/// A single onboarding step definition.
#[derive(Clone, Debug, PartialEq)]
pub struct OnboardingStep {
    pub title: String,
    pub description: String,
    pub completed: bool,
    pub action_label: String,
    pub route: crate::route::Route,
}

/// Read the set of completed step indices from localStorage.
fn load_completed_steps(storage_key: &str) -> Vec<usize> {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
            .and_then(|s| s.get_item(storage_key).ok())
            .flatten()
            .and_then(|json| serde_json::from_str::<Vec<usize>>(&json).ok())
            .unwrap_or_default()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = storage_key;
        Vec::new()
    }
}

/// Persist the set of completed step indices to localStorage.
fn save_completed_steps(storage_key: &str, completed: &[usize]) {
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
        {
            if let Ok(json) = serde_json::to_string(completed) {
                let _ = storage.set_item(storage_key, &json);
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (storage_key, completed);
    }
}

/// Check whether the onboarding has been dismissed.
fn load_dismissed(storage_key: &str) -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        let dismiss_key = format!("{storage_key}_dismissed");
        web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
            .and_then(|s| s.get_item(&dismiss_key).ok())
            .flatten()
            .map(|v| v == "true")
            .unwrap_or(false)
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = storage_key;
        false
    }
}

/// Persist the dismissed flag to localStorage.
fn save_dismissed(storage_key: &str, dismissed: bool) {
    #[cfg(target_arch = "wasm32")]
    {
        let dismiss_key = format!("{storage_key}_dismissed");
        if let Some(storage) = web_sys::window()
            .and_then(|w| w.local_storage().ok())
            .flatten()
        {
            let _ = storage.set_item(&dismiss_key, if dismissed { "true" } else { "false" });
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (storage_key, dismissed);
    }
}

const STORAGE_KEY: &str = "savfox_onboarding";

/// Vertical stepper component for first-use onboarding.
///
/// Accepts a list of [`OnboardingStep`] items and renders them as a
/// vertical checklist.  The first incomplete step shows an action button
/// that navigates to the relevant page.  Completion state is persisted
/// in localStorage so the user picks up where they left off.
#[component]
pub fn OnboardingStepper(
    steps: Vec<OnboardingStep>,
    #[props(default)] on_dismiss: Option<EventHandler<()>>,
) -> Element {
    let nav = use_navigator();

    // Completion / dismissed state initialised from localStorage.
    let mut completed = use_signal(|| load_completed_steps(STORAGE_KEY));
    let mut dismissed = use_signal(|| load_dismissed(STORAGE_KEY));

    // If dismissed, render nothing.
    if dismissed() {
        return rsx! {};
    }

    // Determine which step index is "current" (first non-completed).
    let current_idx: Option<usize> = steps
        .iter()
        .enumerate()
        .find(|(i, s)| !s.completed && !completed().contains(i))
        .map(|(i, _)| i);

    // If all steps are done, auto-dismiss.
    let all_done = current_idx.is_none();

    rsx! {
        div { class: "onboarding",
            h3 { class: "onboarding__heading", "Getting Started" }
            p { class: "onboarding__subheading",
                "Complete these steps to set up your gateway."
            }

            for (i, step) in steps.iter().enumerate() {
                {
                    let is_completed = step.completed || completed().contains(&i);
                    let is_current = current_idx == Some(i);
                    let class_name = if is_completed {
                        "onboarding__step onboarding__step--completed"
                    } else if is_current {
                        "onboarding__step onboarding__step--current"
                    } else {
                        "onboarding__step"
                    };
                    let step_num = i + 1;
                    let title = step.title.clone();
                    let desc = step.description.clone();
                    let action_label = step.action_label.clone();
                    let route = step.route.clone();
                    let nav = nav.clone();
                    rsx! {
                        div { key: "{i}", class: "{class_name}",
                            div { class: "onboarding__circle",
                                if is_completed {
                                    span { class: "onboarding__check", "\u{2713}" }
                                } else {
                                    span { "{step_num}" }
                                }
                            }
                            div { class: "onboarding__body",
                                div { class: "onboarding__title", "{title}" }
                                div { class: "onboarding__desc", "{desc}" }
                                if is_current {
                                    button {
                                        class: "onboarding__action",
                                        onclick: move |_| {
                                            let mut new_completed = completed();
                                            if !new_completed.contains(&i) {
                                                new_completed.push(i);
                                                save_completed_steps(STORAGE_KEY, &new_completed);
                                                completed.set(new_completed);
                                            }
                                            nav.push(route.clone());
                                        },
                                        "{action_label}"
                                    }
                                } else {
                                    { rsx! {} }
                                }
                            }
                        }
                    }
                }
            }

            div { class: "onboarding__footer",
                if all_done {
                    button {
                        class: "onboarding__action",
                        onclick: move |_| {
                            save_dismissed(STORAGE_KEY, true);
                            dismissed.set(true);
                            if let Some(ref handler) = on_dismiss {
                                handler.call(());
                            }
                        },
                        "Start Using Gateway"
                    }
                } else {
                    a {
                        href: "#",
                        class: "onboarding__dismiss",
                        onclick: move |evt| {
                            evt.prevent_default();
                            save_dismissed(STORAGE_KEY, true);
                            dismissed.set(true);
                            if let Some(ref handler) = on_dismiss {
                                handler.call(());
                            }
                        },
                        "Skip setup"
                    }
                }
            }
        }
    }
}
