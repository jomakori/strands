//! The web client's only door to data (architecture §6: every read goes through one module).
//!
//! [`load_home`] returns a seeded fixture shaped like the `GET /api/v1/tags` projection the
//! grid will consume: 16 tags across the five categories, each with its thread count and up
//! to three thread previews. T4 landed the typed client; swapping the source for a network
//! read is a change to this file alone — the views render [`Tag`] either way.
//!
//! The types live here, not in `strands-core`: this is the client-side view model. The wire
//! types arrive with the client and get mapped into these.
//!
//! Rows are owned rather than `&'static` because quick capture (T12) inserts a note into the
//! grid before the server answers, then rewrites it with the server's row.

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

/// Read the home grid. Returns tags most-recently-active first (spec §7.1 grid ordering).
///
/// Seeded until the grid reads the API; the signature already returns an owned `Vec` so the
/// swap to a network call does not churn the caller.
pub fn load_home() -> Vec<Tag> {
    let mut tags: Vec<Tag> = SEED
        .iter()
        .map(|(id, name, category, thread_count, threads)| Tag {
            id: (*id).to_owned(),
            name: (*name).to_owned(),
            category: *category,
            thread_count: *thread_count,
            threads: threads
                .iter()
                .map(|(title, age_minutes)| ThreadPreview {
                    id: format!("{id}-{}", title.len()),
                    title: (*title).to_owned(),
                    age_minutes: *age_minutes,
                })
                .collect(),
        })
        .collect();
    tags.sort_by_key(latest_activity);
    tags
}

/// Look up one tag by id for the detail view. `None` when the id does not match any seed —
/// the view renders a "not found" state rather than unwrapping.
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

const MINUTE: u32 = 1;
const HOUR: u32 = 60 * MINUTE;
const DAY: u32 = 24 * HOUR;

/// The seeded home surface: 16 tags, five categories, real thread titles from the design
/// reference so the layout is judged on realistic copy rather than lorem ipsum. Tuples keep
/// the fixture readable now that [`Tag`] owns its strings.
type Seed = (
    &'static str,
    &'static str,
    Category,
    u32,
    &'static [(&'static str, u32)],
);

const SEED: &[Seed] = &[
    (
        "t1",
        "Tokyo",
        Category::Travel,
        12,
        &[
            (
                "The 6 a.m. train is not what the guidebooks promise",
                3 * HOUR,
            ),
            ("Finding quiet in Shinjuku", 2 * DAY),
            ("Vending machines as infrastructure", 6 * DAY),
        ],
    ),
    (
        "t2",
        "Kyoto",
        Category::Travel,
        9,
        &[
            ("Walking the Philosopher's Path at golden hour", 9 * HOUR),
            ("Moss and maintenance", 4 * DAY),
        ],
    ),
    (
        "t3",
        "Berlin",
        Category::Travel,
        7,
        &[
            ("A night that starts at 2 a.m.", 22 * HOUR),
            ("The Spree at sunrise", 8 * DAY),
        ],
    ),
    (
        "t4",
        "Lisbon",
        Category::Travel,
        4,
        &[("Learning to stop on the hills", 3 * DAY)],
    ),
    (
        "t5",
        "language models",
        Category::Tech,
        14,
        &[
            ("Reasoning is not the same as understanding", 14 * MINUTE),
            ("Small models at the edge", 20 * HOUR),
            ("Evaluating what we can't easily measure", 5 * DAY),
        ],
    ),
    (
        "t6",
        "edge computing",
        Category::Tech,
        6,
        &[("The cost of a round trip", 2 * HOUR)],
    ),
    (
        "t7",
        "TypeScript",
        Category::Tech,
        5,
        &[("Types that say too much", 2 * DAY)],
    ),
    (
        "t8",
        "sourdough",
        Category::Food,
        8,
        &[
            ("Why my starter has a name", 45 * MINUTE),
            ("Oven spring at altitude", 3 * DAY),
        ],
    ),
    (
        "t9",
        "ramen",
        Category::Food,
        5,
        &[("The third bowl is the best one", 7 * HOUR)],
    ),
    (
        "t10",
        "night markets",
        Category::Food,
        3,
        &[("Order one thing at a time", 4 * DAY)],
    ),
    (
        "t11",
        "Moby-Dick",
        Category::Reading,
        11,
        &[
            ("The cetology chapters are a feature", 5 * HOUR),
            ("What the whale is for", 9 * DAY),
        ],
    ),
    (
        "t12",
        "essays",
        Category::Reading,
        6,
        &[("The essay is a first draft of attention", 11 * HOUR)],
    ),
    (
        "t13",
        "notebooks",
        Category::Reading,
        4,
        &[("Reading your old marginalia", 6 * DAY)],
    ),
    (
        "t14",
        "slow time",
        Category::Ideas,
        9,
        &[
            ("Doing one thing badly, on purpose", 30 * MINUTE),
            ("The week has a shape", 4 * DAY),
        ],
    ),
    (
        "t15",
        "questions",
        Category::Ideas,
        7,
        &[
            ("What would I do with a free Tuesday", 5 * HOUR),
            ("Asking better follow-ups", 6 * DAY),
        ],
    ),
    (
        "t16",
        "margin notes",
        Category::Ideas,
        2,
        &[("Reading with a pencil is a conversation", 12 * HOUR)],
    ),
];
