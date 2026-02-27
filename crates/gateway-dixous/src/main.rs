#![allow(dead_code, unused_mut, unused_variables)]

use dioxus::prelude::*;

mod api;
mod chat;
mod components;
mod config;
mod pages;
mod route;
mod utils;

use api::ws::get_token;
use components::auth_gate::AuthGate;
use route::Route;

const MAIN_CSS: Asset = asset!("/assets/main.css");

fn main() {
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

#[component]
fn App() -> Element {
    let mut authenticated = use_signal(|| get_token().is_some());

    rsx! {
        document::Stylesheet { href: MAIN_CSS }
        document::Title { "Savfox - Your AI assistant" }
        if authenticated() {
            Router::<Route> {}
        } else {
            AuthGate { on_authenticated: move |_| authenticated.set(true) }
        }
    }
}
