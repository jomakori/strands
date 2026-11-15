//! The web client's only door to data (architecture §6: every read goes through one module).
//!
//! T4 lands the typed HTTP client; until it does, [`load_home`] returns a seeded fixture
//! shaped like the `GET /api/v1/tags` projection the grid will consume: 16 tags across the
//! five categories, each with its thread count and up to three thread previews. Swapping the
//! source is then a change to this file alone — the views render [`Tag`] either way.
//!
//! The types live here, not in `strands-core`: this is the client-side view model. The wire
//! types arrive with the client and get mapped into these.

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadPreview {
    pub title: &'static str,
    /// Age in minutes at render time; the view formats it (see [`relative`]).
    pub age_minutes: u32,
}

/// A tag as the home grid needs it: identity, category, how many threads it holds, and the
/// few most recent previews. A card shows at least one preview, or its count when it has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tag {
    pub id: &'static str,
    pub name: &'static str,
    pub category: Category,
    /// Total threads, not just the previews carried here.
    pub thread_count: u32,
    pub threads: &'static [ThreadPreview],
}

/// Header totals for the current view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    pub tags: usize,
    pub threads: u32,
}

/// Read the home grid. Returns tags most-recently-active first (spec §7.1 grid ordering).
///
/// Seeded until the typed client lands; the signature already returns an owned `Vec` so the
/// swap to a network call does not churn the caller.
pub fn load_home() -> Vec<Tag> {
    let mut tags = SEED.to_vec();
    tags.sort_by_key(latest_activity);
    tags
}

/// Look up one tag by id for the detail view. `None` when the id does not match any seed —
/// the view renders a "not found" state rather than unwrapping.
pub fn find_tag(tag_id: &str) -> Option<Tag> {
    SEED.iter().find(|tag| tag.id == tag_id).copied()
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
        .copied()
        .collect()
}

/// Totals for the counts header: how many tags and threads the view currently holds.
pub fn counts(tags: &[Tag]) -> Counts {
    Counts {
        tags: tags.len(),
        threads: tags.iter().map(|tag| tag.thread_count).sum(),
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
/// reference so the layout is judged on realistic copy rather than lorem ipsum.
const SEED: &[Tag] = &[
    Tag {
        id: "t1",
        name: "Tokyo",
        category: Category::Travel,
        thread_count: 12,
        threads: &[
            ThreadPreview {
                title: "The 6 a.m. train is not what the guidebooks promise",
                age_minutes: 3 * HOUR,
            },
            ThreadPreview {
                title: "Finding quiet in Shinjuku",
                age_minutes: 2 * DAY,
            },
            ThreadPreview {
                title: "Vending machines as infrastructure",
                age_minutes: 6 * DAY,
            },
        ],
    },
    Tag {
        id: "t2",
        name: "Kyoto",
        category: Category::Travel,
        thread_count: 9,
        threads: &[
            ThreadPreview {
                title: "Walking the Philosopher's Path at golden hour",
                age_minutes: 9 * HOUR,
            },
            ThreadPreview {
                title: "Moss and maintenance",
                age_minutes: 4 * DAY,
            },
        ],
    },
    Tag {
        id: "t3",
        name: "Berlin",
        category: Category::Travel,
        thread_count: 7,
        threads: &[
            ThreadPreview {
                title: "A night that starts at 2 a.m.",
                age_minutes: 22 * HOUR,
            },
            ThreadPreview {
                title: "The Spree at sunrise",
                age_minutes: 8 * DAY,
            },
        ],
    },
    Tag {
        id: "t4",
        name: "Lisbon",
        category: Category::Travel,
        thread_count: 4,
        threads: &[ThreadPreview {
            title: "Learning to stop on the hills",
            age_minutes: 3 * DAY,
        }],
    },
    Tag {
        id: "t5",
        name: "language models",
        category: Category::Tech,
        thread_count: 14,
        threads: &[
            ThreadPreview {
                title: "Reasoning is not the same as understanding",
                age_minutes: 14 * MINUTE,
            },
            ThreadPreview {
                title: "Small models at the edge",
                age_minutes: 20 * HOUR,
            },
            ThreadPreview {
                title: "Evaluating what we can't easily measure",
                age_minutes: 5 * DAY,
            },
        ],
    },
    Tag {
        id: "t6",
        name: "edge computing",
        category: Category::Tech,
        thread_count: 6,
        threads: &[ThreadPreview {
            title: "The cost of a round trip",
            age_minutes: 2 * HOUR,
        }],
    },
    Tag {
        id: "t7",
        name: "TypeScript",
        category: Category::Tech,
        thread_count: 5,
        threads: &[ThreadPreview {
            title: "Types that say too much",
            age_minutes: 2 * DAY,
        }],
    },
    Tag {
        id: "t8",
        name: "sourdough",
        category: Category::Food,
        thread_count: 8,
        threads: &[
            ThreadPreview {
                title: "Why my starter has a name",
                age_minutes: 45 * MINUTE,
            },
            ThreadPreview {
                title: "Oven spring at altitude",
                age_minutes: 3 * DAY,
            },
        ],
    },
    Tag {
        id: "t9",
        name: "ramen",
        category: Category::Food,
        thread_count: 5,
        threads: &[ThreadPreview {
            title: "The third bowl is the best one",
            age_minutes: 7 * HOUR,
        }],
    },
    Tag {
        id: "t10",
        name: "night markets",
        category: Category::Food,
        thread_count: 3,
        threads: &[ThreadPreview {
            title: "Order one thing at a time",
            age_minutes: 4 * DAY,
        }],
    },
    Tag {
        id: "t11",
        name: "Moby-Dick",
        category: Category::Reading,
        thread_count: 11,
        threads: &[
            ThreadPreview {
                title: "The cetology chapters are a feature",
                age_minutes: 5 * HOUR,
            },
            ThreadPreview {
                title: "What the whale is for",
                age_minutes: 9 * DAY,
            },
        ],
    },
    Tag {
        id: "t12",
        name: "essays",
        category: Category::Reading,
        thread_count: 6,
        threads: &[ThreadPreview {
            title: "The essay is a first draft of attention",
            age_minutes: 11 * HOUR,
        }],
    },
    Tag {
        id: "t13",
        name: "notebooks",
        category: Category::Reading,
        thread_count: 4,
        threads: &[ThreadPreview {
            title: "Reading your old marginalia",
            age_minutes: 6 * DAY,
        }],
    },
    Tag {
        id: "t14",
        name: "slow time",
        category: Category::Ideas,
        thread_count: 9,
        threads: &[
            ThreadPreview {
                title: "An hour that is not a unit of production",
                age_minutes: 30 * MINUTE,
            },
            ThreadPreview {
                title: "Notes from a month of unmeasured days",
                age_minutes: 2 * DAY,
            },
        ],
    },
    Tag {
        id: "t15",
        name: "rituals",
        category: Category::Ideas,
        thread_count: 7,
        threads: &[ThreadPreview {
            title: "Two coffees and a closed door",
            age_minutes: 16 * HOUR,
        }],
    },
    Tag {
        id: "t16",
        name: "second brain",
        category: Category::Ideas,
        thread_count: 13,
        threads: &[
            ThreadPreview {
                title: "Capture is the easy half",
                age_minutes: 25 * MINUTE,
            },
            ThreadPreview {
                title: "The note that refuses to link",
                age_minutes: DAY,
            },
        ],
    },
];
