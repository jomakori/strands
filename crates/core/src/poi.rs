//! The fixed place vocabulary (ticket T21).
//!
//! Provider place types are open strings (Google has hundreds and adds more),
//! so they are not usable as tags directly: every novel type would invent a new
//! tag and the tag screen would fill with provider jargon. [`PoiTag`] is the
//! closed ten-value vocabulary every place is filed under. [`PoiTag::from_primary_type`]
//! is total: a type the table below has never seen lands in [`PoiTag::Other`],
//! never in a free-text tag and never nowhere.
//!
//! The metro tag is derived from the place's *locality* component, never by
//! parsing a formatted address: an address line is display text, and "Lisbon,
//! Portugal" inside one says nothing about which component is the city.

use serde::{Deserialize, Serialize};

/// Every place is filed under exactly one of these; the enum is the vocabulary,
/// so adding an eleventh value is a code change rather than a new string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum PoiTag {
    Cafe,
    Restaurant,
    Bar,
    Museum,
    Park,
    Shop,
    Hotel,
    Gym,
    Landmark,
    /// Named catch-all for every provider type outside the nine above.
    Other,
}

impl PoiTag {
    /// The ten values, catch-all last. A test pins the length.
    pub const ALL: [Self; 10] = [
        Self::Cafe,
        Self::Restaurant,
        Self::Bar,
        Self::Museum,
        Self::Park,
        Self::Shop,
        Self::Hotel,
        Self::Gym,
        Self::Landmark,
        Self::Other,
    ];

    /// Tag name as stored in `tags.name`, e.g. `"Cafe"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cafe => "Cafe",
            Self::Restaurant => "Restaurant",
            Self::Bar => "Bar",
            Self::Museum => "Museum",
            Self::Park => "Park",
            Self::Shop => "Shop",
            Self::Hotel => "Hotel",
            Self::Gym => "Gym",
            Self::Landmark => "Landmark",
            Self::Other => "Other",
        }
    }

    /// Total mapping from a Google `primaryType` (or Timeline `semanticType`).
    ///
    /// Matching folds case and treats `-`, space and `/` as `_`, because the
    /// same provider spells types `coffee_shop` in Places API New and `Cafe` in
    /// a Timeline export. Unknown, empty and `None` all fall to [`Self::Other`].
    pub fn from_primary_type(primary_type: Option<&str>) -> Self {
        let Some(raw) = primary_type else {
            return Self::Other;
        };
        let normalized = raw
            .trim()
            .to_ascii_lowercase()
            .replace([' ', '-', '/'], "_");
        match normalized.as_str() {
            // Coffee, sweets and the bakery counter.
            "cafe" | "coffee_shop" | "coffee_roastery" | "tea_house" | "bakery" | "pastry_shop"
            | "dessert_shop" | "ice_cream_shop" | "juice_shop" | "donut_shop" | "bagel_shop"
            | "cafeteria" => Self::Cafe,

            // Sit-down and takeaway food that is not a cafe or a bar.
            "restaurant"
            | "fast_food_restaurant"
            | "pizza_restaurant"
            | "hamburger_restaurant"
            | "sandwich_shop"
            | "sushi_restaurant"
            | "ramen_restaurant"
            | "noodle_shop"
            | "mexican_restaurant"
            | "chinese_restaurant"
            | "indian_restaurant"
            | "italian_restaurant"
            | "japanese_restaurant"
            | "korean_restaurant"
            | "thai_restaurant"
            | "vietnamese_restaurant"
            | "french_restaurant"
            | "greek_restaurant"
            | "spanish_restaurant"
            | "mediterranean_restaurant"
            | "middle_eastern_restaurant"
            | "lebanese_restaurant"
            | "turkish_restaurant"
            | "afghani_restaurant"
            | "african_restaurant"
            | "american_restaurant"
            | "brazilian_restaurant"
            | "seafood_restaurant"
            | "steak_house"
            | "barbecue_restaurant"
            | "breakfast_restaurant"
            | "brunch_restaurant"
            | "lunch_restaurant"
            | "dinner_restaurant"
            | "diner"
            | "buffet_restaurant"
            | "fine_dining_restaurant"
            | "vegan_restaurant"
            | "vegetarian_restaurant"
            | "meal_takeaway"
            | "meal_delivery"
            | "food_court"
            | "deli" => Self::Restaurant,

            // Drink-led venues, including the ones that also serve food.
            "bar" | "pub" | "wine_bar" | "cocktail_bar" | "sports_bar" | "irish_pub"
            | "brewery" | "distillery" | "night_club" | "jazz_club" | "karaoke"
            | "bar_and_grill" => Self::Bar,

            // Collections and institutions that hold things worth looking at.
            "museum" | "art_museum" | "history_museum" | "science_museum" | "art_gallery"
            | "cultural_center" | "planetarium" | "aquarium" => Self::Museum,

            // Open space: green, wild, or a place to sit outside.
            "park" | "national_park" | "state_park" | "city_park" | "garden"
            | "botanical_garden" | "hiking_area" | "campground" | "picnic_ground"
            | "playground" | "dog_park" | "beach" | "nature_preserve" | "wildlife_refuge"
            | "wildlife_park" | "zoo" | "golf_course" => Self::Park,

            // Retail, including markets and everyday errands.
            "shop"
            | "store"
            | "clothing_store"
            | "book_store"
            | "bookstore"
            | "supermarket"
            | "grocery_store"
            | "convenience_store"
            | "shopping_mall"
            | "market"
            | "farmers_market"
            | "jewelry_store"
            | "hardware_store"
            | "electronics_store"
            | "furniture_store"
            | "florist"
            | "liquor_store"
            | "pharmacy"
            | "drugstore"
            | "gift_shop"
            | "shoe_store"
            | "department_store"
            | "pet_store"
            | "sporting_goods_store"
            | "bicycle_store"
            | "home_goods_store"
            | "toy_store"
            | "cosmetics_store"
            | "butcher_shop"
            | "fish_market"
            | "produce_market"
            | "warehouse_store"
            | "discount_store"
            | "wholesale_store"
            | "cell_phone_store"
            | "mobile_phone_store"
            | "auto_parts_store"
            | "garden_center"
            | "art_supply_store"
            | "music_store"
            | "video_game_store"
            | "thrift_store"
            | "vintage_clothing_store"
            | "tea_store" => Self::Shop,

            // Somewhere to sleep.
            "lodging"
            | "hotel"
            | "motel"
            | "hostel"
            | "resort_hotel"
            | "bed_and_breakfast"
            | "guest_house"
            | "inn"
            | "extended_stay_hotel"
            | "camping_cabin"
            | "cottage"
            | "farm_stay"
            | "japanese_inn"
            | "ryokan"
            | "cabin" => Self::Hotel,

            // Fitness, sport and the classes around them.
            "gym"
            | "fitness_center"
            | "yoga_studio"
            | "pilates_studio"
            | "martial_arts_school"
            | "boxing_gym"
            | "climbing_gym"
            | "swimming_pool"
            | "sports_complex"
            | "sports_club"
            | "tennis_court"
            | "sports_activity_location"
            | "athletic_field"
            | "ice_skating_rink"
            | "skateboard_park"
            | "bowling_alley"
            | "spa" => Self::Gym,

            // Sights, worship and cultural venues that are not museums.
            "tourist_attraction"
            | "landmark"
            | "historical_landmark"
            | "historical_place"
            | "monument"
            | "church"
            | "mosque"
            | "synagogue"
            | "hindu_temple"
            | "buddhist_temple"
            | "place_of_worship"
            | "castle"
            | "ruins"
            | "bridge"
            | "observation_deck"
            | "stadium"
            | "arena"
            | "performing_arts_theater"
            | "movie_theater"
            | "concert_hall"
            | "library"
            | "city_hall"
            | "courthouse"
            | "university" => Self::Landmark,

            // Named catch-all: transit, services, offices, and types Google adds
            // after this table was written. Never dropped.
            _ => Self::Other,
        }
    }
}

impl std::fmt::Display for PoiTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Prefix that makes the city tag read as a location, not a venue type.
pub const METRO_TAG_PREFIX: &str = "Metro: ";

/// Tag for the place's city, built from its `locality` address component.
///
/// The argument is deliberately the component and not the place: a formatted
/// address is display text, and splitting it would guess which comma-separated
/// part is the city. No locality, no metro tag.
pub fn metro_tag(locality: Option<&str>) -> Option<String> {
    locality
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("{METRO_TAG_PREFIX}{value}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vocabulary_is_exactly_ten_values_and_round_trips() {
        assert_eq!(PoiTag::ALL.len(), 10);
        for tag in PoiTag::ALL {
            assert_eq!(PoiTag::from_primary_type(Some(tag.as_str())), tag);
            let json = serde_json::to_string(&tag).expect("serialize");
            let back: PoiTag = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(tag, back);
        }
    }

    #[test]
    fn provider_spellings_fold_onto_the_vocabulary() {
        assert_eq!(PoiTag::from_primary_type(Some("Cafe")), PoiTag::Cafe);
        assert_eq!(PoiTag::from_primary_type(Some("coffee_shop")), PoiTag::Cafe);
        assert_eq!(
            PoiTag::from_primary_type(Some(" Pizza-Restaurant ")),
            PoiTag::Restaurant
        );
        assert_eq!(PoiTag::from_primary_type(Some("market")), PoiTag::Shop);
        assert_eq!(
            PoiTag::from_primary_type(Some("national_park")),
            PoiTag::Park
        );
    }

    #[test]
    fn unknown_types_land_in_the_named_catch_all() {
        assert_eq!(
            PoiTag::from_primary_type(Some("quantum_teleport_hub")),
            PoiTag::Other
        );
        assert_eq!(PoiTag::from_primary_type(Some("")), PoiTag::Other);
        assert_eq!(PoiTag::from_primary_type(None), PoiTag::Other);
    }

    #[test]
    fn metro_comes_from_the_locality_component_only() {
        assert_eq!(metro_tag(Some("Lisbon")).as_deref(), Some("Metro: Lisbon"));
        assert_eq!(metro_tag(Some("  ")), None);
        assert_eq!(metro_tag(None), None);
    }
}
