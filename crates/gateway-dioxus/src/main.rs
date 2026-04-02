#![allow(dead_code, unused_mut, unused_variables)]
#![allow(
    clippy::clone_on_copy,
    clippy::collapsible_if,
    clippy::collapsible_str_replace,
    clippy::empty_line_after_doc_comments,
    clippy::let_underscore_future,
    clippy::manual_map,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::single_match,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::unnecessary_map_or,
    clippy::useless_format
)]

use dioxus::prelude::*;

mod api;
mod chat;
mod components;
mod config;
pub mod i18n;
mod pages;
mod route;
mod utils;

use api::ws::get_token;
use components::auth_gate::AuthGate;
use i18n::{detect_locale, t};
use route::Route;

const MAIN_CSS: Asset = asset!("/assets/main.css");
const SAVFOX_FAVICON: Asset = asset!("/assets/savfox.ico");

fn main() {
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut authenticated = use_signal(|| get_token().is_some());
    let locale = use_context_provider(|| Signal::new(detect_locale()));
    let title = t(locale(), "app.title");

    rsx! {
        document::Stylesheet { href: MAIN_CSS }
        document::Link {
            rel: "icon",
            href: SAVFOX_FAVICON,
            r#type: "image/x-icon",
        }
        document::Title { "{title}" }
        if authenticated() {
            Router::<Route> {}
        } else {
            AuthGate { on_authenticated: move |_| authenticated.set(true) }
        }
    }
}
