//! The web client's only door to data (architecture §6: every read goes through one module).
//!
//! [`home_rows`] maps the server's `GET /api/v1/tags` projection — plus the thread previews
//! read from `GET /api/v1/tags/{id}/threads` — into the grid's view model: one [`Tag`] per
//! row the server returned, its count as the server counted it, up to three previews. Nothing
//! here invents a row; an empty answer renders the empty grid, not a sample one.
//!
//! The types live here, not in `strands-core`: this is the client-side view model. The wire
//! types arrive with the client and get mapped into these.
//!
//! Rows are owned rather than `&'static` because quick capture (T12) inserts a note into the
//! grid before the server answers, then rewrites it with the server's row.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use strands_api_types::{CategorySummary, TagSummary, ThreadSummary};
use uuid::Uuid;

/// The five fixed categories (spec §4). Order here is the chip order on the home surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Travel,
    Tech,
    Food,
    Reading,
    Ideas,
}

impl Category {
    /// Every category, in chip order.
    pub const ALL: [Category; 5] = [
        Category::Travel,
        Category::Tech,
        Category::Food,
        Category::Reading,
        Category::Ideas,
    ];

    /// Stable key, used for `data-` attributes and (later) filter query state.
    pub fn key(self) -> &'static str {
        match self {
            Category::Travel => "travel",
            Category::Tech => "tech",
            Category::Food => "food",
            Category::Reading => "reading",
            Category::Ideas => "ideas",
        }
    }

    /// Human label, shown in the chip and as the card eyebrow.
    pub fn label(self) -> &'static str {
        match self {
            Category::Travel => "Travel",
            Category::Tech => "Tech",
            Category::Food => "Food",
            Category::Reading => "Reading",
            Category::Ideas => "Ideas",
        }
    }

    /// The category a server row names, matched on the label the server stores. Case- and
    /// whitespace-insensitive because the name crosses a network boundary, where casing rules
    /// are the writer's, not ours.
    pub fn from_label(label: &str) -> Option<Self> {
        let wanted = label.trim();
        Self::ALL
            .into_iter()
            .find(|category| category.label().eq_ignore_ascii_case(wanted))
    }

    /// CSS custom-property holding this category's ink. Category hue is the only colour
    /// on the surface; everything else is ink-on-ink (spec §3 visual system).
    pub fn accent(self) -> &'static str {
        match self {
            Category::Travel => "var(--travel)",
            Category::Tech => "var(--tech)",
            Category::Food => "var(--food)",
            Category::Reading => "var(--reading)",
            Category::Ideas => "var(--ideas)",
        }
    }

    /// Inner markup of the 24×24 stroked glyph, copied from the design reference so the
    /// chip and the card eyebrow carry the same mark.
    pub fn glyph(self) -> &'static str {
        match self {
            Category::Travel => {
                r#"<circle cx="12" cy="12" r="9"/><path d="m15.5 8.5-2 5-5 2 2-5z"/>"#
            }
            Category::Tech => r#"<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 6l-3 12"/>"#,
            Category::Food => {
                r#"<path d="M4 12h16a8 8 0 0 1-16 0zM9 8c0-1.5 1-1.5 1-3M14 8c0-1.5 1-1.5 1-3"/>"#
            }
            Category::Reading => {
                r#"<path d="M4 5.5C6 4.5 9 4.5 12 6c3-1.5 6-1.5 8-.5V19c-2-1-5-1-8 .5-3-1.5-6-1.5-8-.5zM12 6v13.5"/>"#
            }
            Category::Ideas => {
                r#"<circle cx="6" cy="7" r="2"/><circle cx="18" cy="6" r="1.6"/><circle cx="12" cy="17" r="2.6"/><path d="m7.6 8.6 3 6.2M16.9 7.5l-3.5 7.4"/>"#
            }
        }
    }
}

/// One preview line on a card: a thread and how long ago it last moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPreview {
    /// Thread id. A capture that has not been answered yet carries a `local-…` id, which
    /// is what reconciliation replaces.
    pub id: String,
    pub title: String,
    /// Age in minutes at render time; the view formats it (see [`relative`]).
    pub age_minutes: u32,
}

impl ThreadPreview {
    /// A preview of a just-captured note: no server round trip yet.
    pub fn captured(id: String, title: String) -> Self {
        Self {
            id,
            title,
            age_minutes: 0,
        }
    }
}

/// A tag as the home grid needs it: identity, category, how many threads it holds, and the
/// few most recent previews. A card shows at least one preview, or its count when it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub id: String,
    pub name: String,
    pub category: Category,
    /// Total threads, not just the previews carried here.
    pub thread_count: u32,
    pub threads: Vec<ThreadPreview>,
}

/// Header totals for the current view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub tags: usize,
    pub threads: u32,
}

/// How many previews a card carries (design: three lines of the most recent threads).
pub const PREVIEWS: usize = 3;

/// Every grid row the server returned, most-recently-active first (spec §7.1 grid ordering).
///
/// `threads` is keyed by tag id and holds only what the server answered for that tag; a tag
/// whose preview read failed keeps its server count and renders without previews, which is a
/// thinner card, not a wrong one. `now` is the render clock, because the server sends instants
/// and the card shows ages.
pub fn home_rows(
    tags: &[TagSummary],
    categories: &[CategorySummary],
    threads: &HashMap<Uuid, Vec<ThreadSummary>>,
    now: DateTime<Utc>,
) -> Vec<Tag> {
    let by_id: HashMap<Uuid, Category> = categories
        .iter()
        .filter_map(|category| {
            category_at(category.position, &category.name).map(|mapped| (category.id, mapped))
        })
        .collect();

    let mut rows: Vec<Tag> = tags
        .iter()
        .filter_map(|summary| {
            // `by_id` is built from the categories the same server just returned, so a tag's
            // category is always found. The filter only covers a tag pointing at a category id
            // that is not in the caller's own list — data no chip can draw.
            by_id.get(&summary.category_id).map(|category| {
                tag_row(
                    summary,
                    *category,
                    threads.get(&summary.id).map(Vec::as_slice),
                    now,
                )
            })
        })
        .collect();
    rows.sort_by_key(latest_activity);
    rows
}

/// Which of the five categories a server row names: by seed position first (the order the
/// server lays them down), then by name, for a server whose positions have drifted.
pub fn category_at(position: i32, name: &str) -> Option<Category> {
    usize::try_from(position)
        .ok()
        .and_then(|index| Category::ALL.get(index).copied())
        .or_else(|| Category::from_label(name))
}

/// One grid card from one server row: identity and count from the summary, previews from the
/// tag's thread list, newest first.
pub fn tag_row(
    summary: &TagSummary,
    category: Category,
    threads: Option<&[ThreadSummary]>,
    now: DateTime<Utc>,
) -> Tag {
    let mut rows: Vec<&ThreadSummary> = threads.unwrap_or_default().iter().collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));

    Tag {
        id: summary.id.to_string(),
        name: summary.name.clone(),
        category,
        thread_count: u32::try_from(summary.thread_count).unwrap_or(u32::MAX),
        threads: rows
            .into_iter()
            .take(PREVIEWS)
            .map(|thread| preview(thread, now))
            .collect(),
    }
}

/// Minutes between a server instant and the render clock, floored at zero so a clock that
/// runs ahead of the server never renders a negative age.
pub fn age_minutes(at: DateTime<Utc>, now: DateTime<Utc>) -> u32 {
    (now - at).num_minutes().clamp(0, i64::from(u32::MAX)) as u32
}

/// The render clock. The server sends instants and the cards show ages, so the browser supplies
/// "now" — the one clock both ends of this app can read.
pub fn now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(js_sys::Date::now() as i64).unwrap_or_default()
}

/// One preview line from one server thread row.
pub fn preview(thread: &ThreadSummary, now: DateTime<Utc>) -> ThreadPreview {
    ThreadPreview {
        id: thread.id.to_string(),
        title: thread.title.clone(),
        age_minutes: age_minutes(thread.updated_at, now),
    }
}

/// Look up one tag by id for the detail view. `None` when the server's rows do not hold that
/// id — the view renders a "not found" state rather than unwrapping.
pub fn find_tag(tags: &[Tag], tag_id: &str) -> Option<Tag> {
    tags.iter().find(|tag| tag.id == tag_id).cloned()
}

/// Breadcrumb segments for one thread in a tag's path stack: category, tag, thread title.
pub fn thread_path(tag: &Tag, thread: &ThreadPreview) -> Vec<String> {
    vec![
        tag.category.label().to_string(),
        tag.name.to_string(),
        thread.title.to_string(),
    ]
}

/// Narrow the grid to the active categories. An empty selection means "everything", which is
/// the home surface's resting state — chips are additive, never a mode.
pub fn visible(tags: &[Tag], active: &[Category]) -> Vec<Tag> {
    tags.iter()
        .filter(|tag| active.is_empty() || active.contains(&tag.category))
        .cloned()
        .collect()
}

/// Totals for the counts header: how many tags and threads the view currently holds.
pub fn counts(tags: &[Tag]) -> Counts {
    Counts {
        tags: tags.len(),
        threads: tags.iter().map(|tag| tag.thread_count).sum(),
    }
}

/// One captured note placed in the grid before the server has answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    /// Thread id the grid uses until the server names one.
    pub local_id: String,
    pub tag_name: String,
    pub category: Category,
    pub title: String,
}

/// Put a capture at the top of the grid: prepend its preview to the tag's card, or add the
/// card when the tag is new. The note is visible on the next render, without a round trip.
pub fn insert_capture(tags: &mut Vec<Tag>, capture: &Capture) {
    let preview = ThreadPreview::captured(capture.local_id.clone(), capture.title.clone());

    match tags
        .iter_mut()
        .find(|tag| tag.name.eq_ignore_ascii_case(&capture.tag_name))
    {
        Some(tag) => {
            tag.threads.insert(0, preview);
            tag.thread_count += 1;
        }
        None => tags.push(Tag {
            // A brand-new tag has no server id yet; reconciliation fills it in.
            id: format!("local-tag-{}", capture.local_id),
            name: capture.tag_name.clone(),
            category: capture.category,
            thread_count: 1,
            threads: vec![preview],
        }),
    }
}

/// One strand the server has already accepted, ready to be placed in the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Strand {
    /// The server's tag id, already created when the tag was new.
    pub tag_id: String,
    pub tag_name: String,
    pub category: Category,
    pub thread_id: String,
    /// The title the server derived from the body.
    pub title: String,
}

/// Place a confirmed strand at the top of the grid: the server has already written it, so there
/// is nothing to reconcile and nothing to roll back — the row carries the real ids and the card
/// is the one it belongs to, whether or not that tag had a card before.
pub fn add_strand(tags: &mut Vec<Tag>, strand: &Strand) {
    let preview = ThreadPreview {
        id: strand.thread_id.clone(),
        title: strand.title.clone(),
        age_minutes: 0,
    };

    let at = tags
        .iter()
        .position(|tag| tag.id == strand.tag_id)
        .or_else(|| {
            tags.iter()
                .position(|tag| tag.name.eq_ignore_ascii_case(&strand.tag_name))
        });

    let Some(index) = at else {
        tags.insert(
            0,
            Tag {
                id: strand.tag_id.clone(),
                name: strand.tag_name.clone(),
                category: strand.category,
                thread_count: 1,
                threads: vec![preview],
            },
        );
        return;
    };

    let mut tag = tags.remove(index);
    // A card capture left behind carries a `local-tag-` id; the server's answer replaces it.
    tag.id = strand.tag_id.clone();
    tag.name = strand.tag_name.clone();
    tag.category = strand.category;
    tag.thread_count += 1;
    tag.threads.insert(0, preview);
    tag.threads.truncate(PREVIEWS);
    // Most-recently-active first is the server's own ordering, so the moved card is where a
    // reload would put it.
    tags.insert(0, tag);
}

/// Replace the optimistic row with the server's answer: the real thread id, the title the
/// server derived from the body, and the tag the note actually landed in. The card the note
/// was optimistically filed under adopts the server's tag id, so the grid keeps one card per
/// tag instead of showing the optimistic one beside the real one.
pub fn reconcile_capture(
    tags: &mut Vec<Tag>,
    local_id: &str,
    tag_id: &str,
    thread_id: &str,
    title: &str,
) {
    let Some(source) = tags
        .iter()
        .position(|tag| tag.threads.iter().any(|thread| thread.id == local_id))
    else {
        return;
    };
    let row = tags[source]
        .threads
        .iter()
        .position(|thread| thread.id == local_id)
        .expect("the card was found holding this row");
    let mut preview = tags[source].threads.remove(row);
    preview.id = thread_id.to_owned();
    preview.title = title.to_owned();

    // A card for the server's tag may already exist (a second note into the same tag), in
    // which case the row moves there rather than duplicating the card.
    let target = match tags.iter().position(|tag| tag.id == tag_id) {
        Some(index) => index,
        None => {
            tags[source].id = tag_id.to_owned();
            source
        }
    };

    tags[target].threads.insert(0, preview);
    tags[target].thread_count += 1;

    if target != source {
        tags[source].thread_count = tags[source].thread_count.saturating_sub(1);
        let orphaned = tags[source].thread_count == 0 && tags[source].id.starts_with("local-tag-");
        if orphaned {
            tags.remove(source);
        }
    }
}

/// Drop an optimistic row whose write failed, and the card the capture created for it.
pub fn rollback_capture(tags: &mut Vec<Tag>, local_id: &str) {
    let Some(tag) = tags
        .iter_mut()
        .find(|tag| tag.threads.iter().any(|thread| thread.id == local_id))
    else {
        return;
    };

    tag.threads.retain(|thread| thread.id != local_id);
    tag.thread_count = tag.thread_count.saturating_sub(1);
    let orphaned = tag.thread_count == 0 && tag.id.starts_with("local-tag-");
    let tag_id = tag.id.clone();
    if orphaned {
        tags.retain(|kept| kept.id != tag_id);
    }
}

/// Archive one thread: drop its row from the tag it is filed under and hand back the title,
/// so the view can say what happened. `None` when that tag no longer holds the row.
pub fn archive_thread(tags: &mut [Tag], tag_id: &str, thread_id: &str) -> Option<String> {
    let tag = tags.iter_mut().find(|tag| tag.id == tag_id)?;
    let row = tag
        .threads
        .iter()
        .position(|thread| thread.id == thread_id)?;
    let thread = tag.threads.remove(row);
    tag.thread_count = tag.thread_count.saturating_sub(1);
    Some(thread.title)
}

/// Minute age of a tag's most recent thread; `u32::MAX` when a tag has no previews so it
/// sorts last rather than first.
fn latest_activity(tag: &Tag) -> u32 {
    tag.threads
        .iter()
        .map(|thread| thread.age_minutes)
        .min()
        .unwrap_or(u32::MAX)
}

/// Pips filled for a card: a five-step read of how much the tag holds. Derived from thread
/// count rather than stored, so it stays honest when the real counts arrive.
pub fn weight(thread_count: u32) -> u8 {
    match thread_count {
        0 => 1,
        1..=3 => 2,
        4..=6 => 3,
        7..=10 => 4,
        _ => 5,
    }
}

/// How many pips a card draws.
pub const PIPS: u8 = 5;

/// `--glow` for a card: the mockup brightens busier tags. 0.0–1.0.
pub fn glow(thread_count: u32) -> f32 {
    (thread_count as f32 / 14.0).clamp(0.15, 1.0)
}

/// Compact relative age: `now`, `12m`, `3h`, `2d`, `5w`.
pub fn relative(age_minutes: u32) -> String {
    match age_minutes {
        0..=0 => "now".to_string(),
        1..=59 => format!("{age_minutes}m"),
        _ if age_minutes < 24 * 60 => format!("{}h", age_minutes / 60),
        _ if age_minutes < 7 * 24 * 60 => format!("{}d", age_minutes / (24 * 60)),
        _ => format!("{}w", age_minutes / (7 * 24 * 60)),
    }
}

/// `1 thread` / `7 threads`.
pub fn thread_plural(n: u32) -> String {
    if n == 1 {
        "1 thread".to_string()
    } else {
        format!("{n} threads")
    }
}

/// `1 tag` / `16 tags`.
pub fn tag_plural(n: usize) -> String {
    if n == 1 {
        "1 tag".to_string()
    } else {
        format!("{n} tags")
    }
}
