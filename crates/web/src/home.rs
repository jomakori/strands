//! The home surface: five category chips, a counts header, and the tag grid.
//!
//! The grid is the server's rows: [`install`] reads `GET /api/v1/tags`, the caller's categories,
//! and each tag's threads once per app load, and maps them into [`Tag`] (T25). Until that read
//! answers the surface shows a skeleton; an empty answer is an empty state, not a sample grid;
//! a failed read offers the same retry control as every other fetch-backed surface. A 401 never
//! reaches these states — the client hands it to the gate (T24) through `session::refused`.
//!
//! Filtering is component state only — pressing a chip recomputes the visible set and the
//! grid re-renders in place. No navigation, no reload, no server round trip (spec §7.1).
//!
//! The grid itself lives in a context signal (provided by `App`) so quick capture (T12) can
//! insert a note optimistically and have the grid show it on the way back.

use std::collections::HashMap;

use dioxus::prelude::*;
use strands_api_types::{CategorySummary, TagSummary, ThreadSummary};
use uuid::Uuid;

use crate::api::{self, ApiError};
use crate::data::{self, Category, Tag};
use crate::routes::Route;
use crate::state::{self, EmptyNotice, RetryNotice, Skeleton};

/// The grid every surface reads: the app provides it once, capture mutates it, home renders it.
#[derive(Clone, Copy)]
pub struct Grid(pub Signal<Vec<Tag>>);

/// Where the grid's one read stands. The rows live in [`Grid`]; this says whether they are
/// still being read, are in hand, or could not be read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Ready,
    Failed(String),
}

/// The grid's read, provided beside [`Grid`] so a route change does not re-ask the server and
/// an optimistic capture survives the trip back to `/`.
#[derive(Clone, Copy)]
pub struct HomeLoad {
    pub phase: Signal<Phase>,
}

/// Read the grid once, at app start, and hand every route the handle to it.
pub fn install(grid: Signal<Vec<Tag>>) -> HomeLoad {
    let load = HomeLoad {
        phase: use_signal(|| Phase::Loading),
    };
    use_hook(|| read(grid, load));
    load
}

/// Ask the server again after a failed read.
pub fn retry(grid: Signal<Vec<Tag>>, load: HomeLoad) {
    read(grid, load);
}

/// One read of the server's rows into the grid.
fn read(mut grid: Signal<Vec<Tag>>, load: HomeLoad) {
    let mut phase = load.phase;
    spawn(async move {
        phase.set(Phase::Loading);
        match fetch().await {
            Ok(rows) => {
                grid.set(rows);
                phase.set(Phase::Ready);
            }
            // The gate already took over: an error panel behind a sign-in screen would only
            // describe a request the visitor is about to be asked to make again.
            Err(ApiError::Status(401)) => {}
            Err(error) => phase.set(Phase::Failed(state::describe(&error))),
        }
    });
}

/// The three reads the grid needs. Categories come first because a row without a category has
/// no chip to sit under; the thread previews are advisory, so one failed preview read thins a
/// card instead of failing the surface.
async fn fetch() -> Result<Vec<Tag>, ApiError> {
    let categories: Vec<CategorySummary> = api::list_categories().await?;
    let tags: Vec<TagSummary> = api::list_tags().await?;

    let answers = futures::future::join_all(tags.iter().map(|tag| api::list_threads(tag.id))).await;
    let mut threads: HashMap<Uuid, Vec<ThreadSummary>> = HashMap::new();
    for (tag, answer) in tags.iter().zip(answers) {
        if let Ok(rows) = answer {
            threads.insert(tag.id, rows);
        }
    }

    Ok(data::home_rows(&tags, &categories, &threads, data::now()))
}

/// The whole home screen. Reads the grid through the context signal, so a note captured on
/// the capture screen is already in the list when the user comes back.
#[component]
pub fn HomeView() -> Element {
    let grid = use_context::<Grid>().0;
    let load = use_context::<HomeLoad>();
    let active = use_signal(Vec::<Category>::new);

    let phase = load.phase;
    let all = grid();
    let shown = data::visible(&all, &active());
    let counts = data::counts(&shown);
    let tags_label = data::tag_plural(counts.tags);
    let threads_label = data::thread_plural(counts.threads);
    let loaded = phase() == Phase::Ready;
    let empty_title = if all.is_empty() {
        "No tags yet"
    } else {
        "Nothing in these categories"
    };

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
                    h1 { class: "brand-word",
                        b { "Strands" }
                        span { class: "strand", "aria-hidden": "true",
                            svg { view_box: "0 0 100 6", preserve_aspect_ratio: "none",
                                path { d: "M0 3C10 .5 18 5.5 30 3S52 .5 64 3S88 5.5 100 3" }
                            }
                        }
                    }
                }
                Link {
                    class: "capture-cta",
                    to: Route::Capture {},
                    "data-testid": "capture-open",
                    "New note"
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
                            // The header counts the rows in hand, so it equals what the server
                            // listed — including zero. Until the read answers there are no rows
                            // to count, and the line stays silent rather than printing a number
                            // it would have to take back.
                            if loaded {
                                "{tags_label} · {threads_label}"
                            }
                        }
                    }
                    match phase() {
                        Phase::Loading => rsx! {
                            Skeleton { lines: 6, label: "Reading your tags".to_string() }
                        },
                        Phase::Failed(message) => rsx! {
                            RetryNotice {
                                message,
                                onretry: move |_| retry(grid, load),
                            }
                        },
                        Phase::Ready => rsx! {
                            if shown.is_empty() {
                                EmptyNotice {
                                    title: empty_title.to_string(),
                                    hint: "Capture a note and it lands in a tag here.".to_string(),
                                }
                            }
                            ul { class: "grid", id: "grid",
                                for tag in shown.iter() {
                                    li { class: "grid-item", key: "{tag.id}",
                                        TagCard { tag: tag.clone() }
                                    }
                                }
                            }
                        },
                    }
                    footer { class: "feed-foot",
                        Link { class: "back-link", id: "constellation-link", to: Route::Constellation {},
                            "Constellation"
                        }
                        Link { class: "back-link", id: "account-link", to: Route::Account {},
                            "Account"
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
            "data-tag-id": tag.id.clone(),
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
