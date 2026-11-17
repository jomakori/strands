//! Constellation view (T10): tags as nodes, judged links as edges.
//!
//! Nodes are plain buttons laid out on a circle with percentage offsets, so the
//! graph scales inside its box at 360px without horizontal scrolling; the edges
//! are an SVG layer underneath. Selecting a node (tap, or Enter/Space on the
//! focused button) shows every rationale that touches it. A node with no judged
//! edge says so instead of pretending to be connected.

use std::collections::{BTreeMap, HashSet};

use dioxus::prelude::*;
use strands_api_types::{LinkSummary, TagSummary};
use uuid::Uuid;

use crate::api;
use crate::state::{self, RetryNotice, Skeleton};

/// One node in the graph, whether or not it has an edge.
struct Node {
    id: Uuid,
    name: String,
    /// Percentage position inside the square plot, centre of the node.
    x: f64,
    y: f64,
}

/// Edges keyed by node id, so selecting a node is a map lookup.
type EdgeIndex = BTreeMap<Uuid, Vec<usize>>;

fn layout(tags: &[TagSummary], links: &[LinkSummary]) -> Vec<Node> {
    // Every tag is a node: a tag with no judged link still has to render, and
    // say that nothing links to it yet.
    let mut ids: Vec<(Uuid, String)> = tags.iter().map(|tag| (tag.id, tag.name.clone())).collect();

    // An edge can name a tag the grid did not return (a stale link whose tag was
    // deleted after judging); include it so no edge dangles.
    let known: HashSet<Uuid> = ids.iter().map(|(id, _)| *id).collect();
    for link in links {
        if !known.contains(&link.a_tag_id) {
            ids.push((link.a_tag_id, link.a_tag_name.clone()));
        }
        if !known.contains(&link.b_tag_id) {
            ids.push((link.b_tag_id, link.b_tag_name.clone()));
        }
    }

    let count = ids.len().max(1) as f64;
    ids.into_iter()
        .enumerate()
        .map(|(index, (id, name))| {
            let angle = std::f64::consts::TAU * index as f64 / count - std::f64::consts::FRAC_PI_2;
            Node {
                id,
                name,
                x: 50.0 + 38.0 * angle.cos(),
                y: 50.0 + 38.0 * angle.sin(),
            }
        })
        .collect()
}

fn edge_index(links: &[LinkSummary], nodes: &[Node]) -> EdgeIndex {
    let positions: BTreeMap<Uuid, usize> = nodes
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();

    let mut index = EdgeIndex::new();
    for (link_index, link) in links.iter().enumerate() {
        for id in [link.a_tag_id, link.b_tag_id] {
            if positions.contains_key(&id) {
                index.entry(id).or_default().push(link_index);
            }
        }
    }
    index
}

/// Thicker, brighter lines for stronger links; 0 still draws, so a judged
/// but unrelated pair is visible rather than invisible.
fn stroke_width(score: u8) -> f64 {
    0.35 + f64::from(score.min(100)) / 100.0 * 1.15
}

fn stroke_opacity(score: u8) -> f64 {
    0.25 + f64::from(score.min(100)) / 200.0
}

#[component]
pub fn Constellation() -> Element {
    let mut refresh = use_signal(|| 0_u32);
    let selected = use_signal(|| None::<Uuid>);
    let mut judging = use_signal(|| false);
    let mut message = use_signal(|| None::<String>);

    let data = use_resource(move || async move {
        let _ = refresh();
        let tags = api::list_tags().await?;
        let links = api::list_links().await?;
        Ok::<_, api::ApiError>((tags, links))
    });

    let loading = data.read().is_none();
    let failure = match &*data.read() {
        Some(Err(error)) => Some(state::describe(error)),
        _ => None,
    };
    let (tags, links) = match &*data.read() {
        Some(Ok((tags, links))) => (tags.clone(), links.clone()),
        _ => (Vec::new(), Vec::new()),
    };

    let nodes = layout(&tags, &links);
    let edges = edge_index(&links, &nodes);

    let judge = move |_| {
        spawn(async move {
            if *judging.peek() {
                return;
            }
            judging.set(true);
            match api::judge_links().await {
                Ok(report) => {
                    message.set(Some(format!(
                        "Judged {} pair{}: {} new, {} updated.",
                        report.judged,
                        if report.judged == 1 { "" } else { "s" },
                        report.created,
                        report.updated
                    )));
                    refresh += 1;
                }
                Err(error) => message.set(Some(format!("Could not judge links: {error}"))),
            }
            judging.set(false);
        });
    };

    let selected_links: Vec<LinkSummary> = selected
        .read()
        .and_then(|id| edges.get(&id).cloned())
        .map(|indexes| indexes.iter().map(|index| links[*index].clone()).collect())
        .unwrap_or_default();

    let selected_name = selected.read().and_then(|id| {
        nodes
            .iter()
            .find(|node| node.id == id)
            .map(|node| node.name.clone())
    });

    rsx! {
        main { class: "constellation",
            header { class: "constellation-head",
                h1 { "Constellation" }
                p { class: "sub", "Tags that keep showing up together, and why." }
            }

            if let Some(message) = failure {
                RetryNotice {
                    message: format!("Could not load the constellation: {message}"),
                    onretry: move |_| refresh += 1,
                }
            } else if loading {
                Skeleton { lines: 4, label: "Loading the constellation…" }
            } else if nodes.is_empty() {
                // Same empty-state shape as the grid and detail views: a quiet
                // line of prose, no fake data.
                p { class: "sub", "data-state": "empty",
                    "No tags yet, so there is nothing to link."
                }
            } else if links.is_empty() {
                p { class: "sub", "data-state": "empty",
                    "Nothing is linked yet. Judge links to see which tags share a thread or a place."
                }
                button {
                    class: "fchip",
                    disabled: *judging.read(),
                    onclick: judge,
                    if *judging.read() { "Judging…" } else { "Judge links" }
                }
            } else {
                div { class: "constellation-plot",
                    svg {
                        class: "constellation-edges",
                        view_box: "0 0 100 100",
                        preserve_aspect_ratio: "none",
                        for (index, link) in links.iter().enumerate() {
                            if let (Some(a), Some(b)) = (
                                nodes.iter().find(|node| node.id == link.a_tag_id),
                                nodes.iter().find(|node| node.id == link.b_tag_id),
                            ) {
                                line {
                                    key: "{index}",
                                    x1: "{a.x}",
                                    y1: "{a.y}",
                                    x2: "{b.x}",
                                    y2: "{b.y}",
                                    "stroke-width": "{stroke_width(link.score)}",
                                    "stroke-opacity": "{stroke_opacity(link.score)}",
                                }
                            }
                        }
                    }
                    for node in nodes.iter() {
                        button {
                            key: "{node.id}",
                            class: if selected.read().as_ref() == Some(&node.id) { "constellation-node is-selected" } else { "constellation-node" },
                            style: "left: {node.x}%; top: {node.y}%;",
                            aria_label: "Tag {node.name}",
                            aria_pressed: selected.read().as_ref() == Some(&node.id),
                            onclick: {
                                let id = node.id;
                                move |_| {
                                    let mut selected = selected;
                                    if selected.read().as_ref() == Some(&id) {
                                        selected.set(None);
                                    } else {
                                        selected.set(Some(id));
                                    }
                                }
                            },
                            span { class: "constellation-node-name", "{node.name}" }
                            if !edges.contains_key(&node.id) {
                                span { class: "constellation-node-empty", "nothing linked yet" }
                            }
                        }
                    }
                }

                div { class: "constellation-actions",
                    button {
                        class: "fchip",
                        disabled: *judging.read(),
                        onclick: judge,
                        if *judging.read() { "Judging…" } else { "Judge links" }
                    }
                    if let Some(text) = message.read().as_ref() {
                        p { class: "sub", "{text}" }
                    }
                }

                section { class: "constellation-rationale", aria_live: "polite",
                    match selected_name {
                        Some(name) if selected_links.is_empty() => rsx! {
                            p { class: "sub", "data-state": "empty",
                                "{name} has no judged links yet, so no rationale to show."
                            }
                        },
                        Some(name) => rsx! {
                            h2 { "{name}" }
                            ul { class: "constellation-links",
                                for (index, link) in selected_links.iter().enumerate() {
                                    li { key: "{index}",
                                        div { class: "constellation-link-head",
                                            span { class: "constellation-link-score", "{link.score}" }
                                            span { class: "constellation-link-pair",
                                                "{link.a_tag_name} ↔ {link.b_tag_name}"
                                            }
                                        }
                                        p { class: "sub", "{link.rationale}" }
                                    }
                                }
                            }
                        },
                        None => rsx! {
                            p { class: "sub", "Select a tag to see why it is linked." }
                        },
                    }
                }
            }
        }
    }
}
