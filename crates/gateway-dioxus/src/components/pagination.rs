use dioxus::prelude::*;

/// Simple pagination with page numbers.
#[component]
pub fn Pagination(
    current_page: usize,
    total_pages: usize,
    on_page: EventHandler<usize>,
) -> Element {
    if total_pages <= 1 {
        return rsx! {};
    }

    let has_prev = current_page > 1;
    let has_next = current_page < total_pages;

    // Show at most 5 page buttons around current
    let start = current_page.saturating_sub(2).max(1);
    let end = (start + 4).min(total_pages);
    let pages: Vec<usize> = (start..=end).collect();

    rsx! {
        nav { class: "pagination", aria_label: "Pagination",
            button {
                class: "pagination__btn",
                disabled: !has_prev,
                onclick: move |_| if has_prev { on_page.call(current_page - 1) },
                "Prev"
            }
            for page in pages {
                {
                    let is_active = page == current_page;
                    let cls = if is_active { "pagination__btn pagination__btn--active" } else { "pagination__btn" };
                    rsx! {
                        button {
                            key: "{page}",
                            class: "{cls}",
                            aria_current: if is_active { "page" } else { "" },
                            onclick: move |_| on_page.call(page),
                            "{page}"
                        }
                    }
                }
            }
            button {
                class: "pagination__btn",
                disabled: !has_next,
                onclick: move |_| if has_next { on_page.call(current_page + 1) },
                "Next"
            }
        }
    }
}
