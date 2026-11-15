//! The home surface: five category chips, a counts header, and the tag grid.
//!
//! Filtering is component state only — pressing a chip recomputes the visible set and the
//! grid re-renders in place. No navigation, no reload, no server round trip (spec §7.1).

use dioxus::prelude::*;

use crate::data::{self, Category, Tag};
use crate::routes::Route;

/// The whole home screen. Reads the grid through [`data::load_home`], so the seeded fixture
/// and the future typed client are interchangeable here.
#[component]
pub fn HomeView() -> Element {
    // Recomputed per render on purpose: 16 tags is cheaper than the bookkeeping a memo would
    // need, and it keeps the filter path a plain function call.
    let all = data::load_home();
    let active = use_signal(Vec::<Category>::new);

    let shown = data::visible(&all, &active());
    let counts = data::counts(&shown);
    let tags_label = data::tag_plural(counts.tags);
    let threads_label = data::thread_plural(counts.threads);

    rsx! {
        div { class: "app",
            header { class: "top-row",
                div { class: "brand",
                    svg {
                        class: "brand-mark",
                        width: "44",
                        height: "44",
                        view_box: "0 0 32 32",
                        "aria-hidden": "true",
                        rect { width: "32", height: "32", rx: "9", fill: "oklch(0.21 0.025 265)" }
                        g { transform: "translate(16 16) scale(.92) translate(-15.4 -14.1)",
                            rect {
                                x: "3.2",
                                y: "11.2",
                                width: "22",
                                height: "13",
                                rx: "6.5",
                                fill: "none",
                                stroke: "oklch(0.97 0.01 265)",
                                stroke_width: "2.4",
                            }
                            path {
                                d: "M20 11.2 25 6",
                                fill: "none",
                                stroke: "oklch(0.97 0.01 265)",
                                stroke_width: "2.4",
                                stroke_linecap: "round",
                            }
                            circle { cx: "10", cy: "17.7", r: "2.8", fill: "oklch(0.86 0.14 95)" }
                            circle { cx: "26", cy: "5.6", r: "2.8", fill: "oklch(0.86 0.14 95)" }
                        }
                    }
                    span { class: "brand-word",
                        b { "Strands" }
                        span { class: "strand", "aria-hidden": "true",
                            svg { view_box: "0 0 100 6", preserve_aspect_ratio: "none",
                                path { d: "M0 3C10 .5 18 5.5 30 3S52 .5 64 3S88 5.5 100 3" }
                            }
                        }
                    }
                }
            }
            main { class: "feed", "aria-label": "Strands feed",
                div { class: "feed-inner",
                    div { class: "feed-head",
                        div {
                            class: "filter-row",
                            role: "group",
                            "aria-label": "Filter tags by category",
                            for category in Category::ALL {
                                Chip { key: "{category.key()}", category, active }
                            }
                        }
                        p { class: "sub", id: "feed-count", "aria-live": "polite",
                            "{tags_label} · {threads_label}"
                        }
                    }
                    div { class: "grid", id: "grid",
                        for tag in shown.iter() {
                            TagCard { key: "{tag.id}", tag: *tag }
                        }
                    }
                }
            }
        }
    }
}

/// One category chip. Additive toggle: pressing it adds or removes that category from the
/// selection, and an empty selection shows everything.
#[component]
fn Chip(category: Category, active: Signal<Vec<Category>>) -> Element {
    let pressed = active.read().contains(&category);

    rsx! {
        button {
            class: "fchip",
            r#type: "button",
            "data-chip": "{category.key()}",
            style: "--cat: {category.accent()};",
            "aria-pressed": if pressed { "true" } else { "false" },
            onclick: move |_| toggle(active, category),
            CatGlyph { category, size: 18 }
            span { "{category.label()}" }
        }
    }
}

/// One tag in the grid: category eyebrow, name, up to three thread previews, and the count.
#[component]
fn TagCard(tag: Tag) -> Element {
    let weight = data::weight(tag.thread_count);
    let glow = data::glow(tag.thread_count);
    let thread_label = data::thread_plural(tag.thread_count);
    let latest = tag
        .threads
        .first()
        .map(|thread| data::relative(thread.age_minutes));

    rsx! {
        Link {
            class: "tag",
            style: "--cat: {tag.category.accent()}; --glow: {glow:.2};",
            "data-tag-id": tag.id,
            "data-cat": tag.category.key(),
            to: Route::TagDetail { tag_id: tag.id.to_string() },
            div { class: "cat-row",
                CatGlyph { category: tag.category, size: 18 }
                span { class: "cat-name", "{tag.category.label()}" }
                span { class: "pips", "aria-hidden": "true",
                    for step in 1..=data::PIPS {
                        i { class: if step <= weight { "on" } else { "" } }
                    }
                }
            }
            div { class: "tag-body",
                h2 { class: "name", "{tag.name}" }
                if tag.threads.is_empty() {
                    span { class: "peeks",
                        span { class: "peek-line",
                            span { class: "peek-title", "{thread_label} so far" }
                        }
                    }
                } else {
                    span { class: "peeks",
                        for thread in tag.threads.iter().take(3) {
                            span { class: "peek-line",
                                span { class: "peek-title", "{thread.title}" }
                                span { class: "peek-age", "{data::relative(thread.age_minutes)}" }
                            }
                        }
                    }
                }
            }
            div { class: "foot",
                "{thread_label}"
                if let Some(age) = latest {
                    span { class: "foot-age", " · last {age}" }
                }
            }
        }
    }
}

/// The 24×24 stroked category mark, shared by chips, card eyebrows and the detail card.
/// The inner markup is a static string from [`Category::glyph`], not user input.
#[component]
pub(crate) fn CatGlyph(category: Category, size: u8) -> Element {
    rsx! {
        svg {
            class: "glyph",
            width: "{size}",
            height: "{size}",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            stroke_width: "1.8",
            stroke_linecap: "round",
            stroke_linejoin: "round",
            "aria-hidden": "true",
            dangerous_inner_html: category.glyph(),
        }
    }
}

/// Add or remove a category from the selection. Pure client state — the grid re-renders,
/// the document does not reload.
fn toggle(mut active: Signal<Vec<Category>>, category: Category) {
    let mut selected = active.write();
    match selected.iter().position(|chosen| *chosen == category) {
        Some(index) => {
            selected.remove(index);
        }
        None => selected.push(category),
    }
}
