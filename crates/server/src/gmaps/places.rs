//! Place enrichment: turning a saved row (`name` + optional address) into a
//! located place.
//!
//! Behind a trait for one reason: the importer must not need Google to be
//! testable, and it must never invent coordinates when Google is not there.
//! [`UnconfiguredResolver`] is the honest production default when no API key is
//! set — it resolves nothing, so every row lands `unresolved` and says so,
//! instead of the import failing or the rows vanishing.

use serde::Deserialize;

use crate::llm::BoxFuture;

/// What we know about a row before looking it up.
#[derive(Debug, Clone, Copy)]
pub struct PlaceQuery<'a> {
    pub name: &'a str,
    pub address: &'a str,
}

impl PlaceQuery<'_> {
    /// A lookup needs an address. Takeout's four-column CSV has none, so those
    /// rows are stored unresolved without ever calling the provider.
    pub fn is_resolvable(&self) -> bool {
        !self.address.trim().is_empty() && !self.name.trim().is_empty()
    }
}

/// A place the provider found.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPlace {
    pub name: String,
    pub address: Option<String>,
    pub lat: f64,
    pub lng: f64,
    pub place_id: Option<String>,
    pub primary_type: Option<String>,
    pub types: Vec<String>,
    pub metro: Option<String>,
    pub locality: Option<String>,
    pub country: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlaceError {
    #[error("the place lookup failed: {0}")]
    Transport(String),
    #[error("the place lookup was rejected: {0}")]
    Upstream(String),
}

/// Looks a place up. `Ok(None)` means "asked, found nothing" — distinct from an
/// error, and both leave the row unresolved.
pub trait PlaceResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        query: PlaceQuery<'a>,
    ) -> BoxFuture<'a, Result<Option<ResolvedPlace>, PlaceError>>;
}

/// The resolver used when no Places API key is configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredResolver;

impl PlaceResolver for UnconfiguredResolver {
    fn resolve<'a>(
        &'a self,
        _query: PlaceQuery<'a>,
    ) -> BoxFuture<'a, Result<Option<ResolvedPlace>, PlaceError>> {
        Box::pin(async { Ok(None) })
    }
}

/// Google Places API (New) text search.
#[derive(Debug, Clone)]
pub struct PlacesApiResolver {
    client: reqwest::Client,
    endpoint: String,
    api_key: String,
}

impl PlacesApiResolver {
    /// Build from the environment, or `None` when `GOOGLE_MAPS_API_KEY` is
    /// unset — the caller then keeps [`UnconfiguredResolver`].
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("GOOGLE_MAPS_API_KEY")
            .ok()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty())?;
        Some(Self::new(
            api_key,
            std::env::var("GOOGLE_PLACES_ENDPOINT")
                .unwrap_or_else(|_| "https://places.googleapis.com/v1/places:searchText".into()),
        ))
    }

    pub fn new(api_key: String, endpoint: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint,
            api_key,
        }
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    #[serde(default)]
    places: Vec<ApiPlace>,
}

#[derive(Deserialize)]
struct ApiPlace {
    #[serde(default)]
    id: Option<String>,
    #[serde(default, rename = "displayName")]
    display_name: Option<DisplayName>,
    #[serde(default, rename = "formattedAddress")]
    formatted_address: Option<String>,
    #[serde(default)]
    location: Option<LatLng>,
    #[serde(default, rename = "primaryType")]
    primary_type: Option<String>,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default, rename = "addressComponents")]
    address_components: Vec<AddressComponent>,
}

#[derive(Deserialize)]
struct DisplayName {
    #[serde(default)]
    text: Option<String>,
}

#[derive(Deserialize)]
struct LatLng {
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize)]
struct AddressComponent {
    #[serde(default, rename = "longText")]
    long_text: Option<String>,
    #[serde(default)]
    types: Vec<String>,
}

impl PlaceResolver for PlacesApiResolver {
    fn resolve<'a>(
        &'a self,
        query: PlaceQuery<'a>,
    ) -> BoxFuture<'a, Result<Option<ResolvedPlace>, PlaceError>> {
        Box::pin(async move {
            if !query.is_resolvable() {
                return Ok(None);
            }

            let body = serde_json::json!({
                "textQuery": format!("{}, {}", query.name.trim(), query.address.trim()),
                "maxResultCount": 1,
            });

            let response = self
                .client
                .post(&self.endpoint)
                .header("X-Goog-Api-Key", &self.api_key)
                .header(
                    "X-Goog-FieldMask",
                    "places.id,places.displayName,places.formattedAddress,places.location,\
                     places.primaryType,places.types,places.addressComponents",
                )
                .json(&body)
                .send()
                .await
                .map_err(|err| PlaceError::Transport(err.to_string()))?;

            let status = response.status();
            if !status.is_success() {
                let detail = response.text().await.unwrap_or_default();
                return Err(PlaceError::Upstream(format!("{status}: {detail}")));
            }

            let parsed: SearchResponse = response
                .json()
                .await
                .map_err(|err| PlaceError::Transport(err.to_string()))?;

            Ok(parsed.places.into_iter().next().and_then(into_resolved))
        })
    }
}

/// A result without coordinates is not a resolved place; dropping it here keeps
/// `NaN` (and a bogus 0,0) out of the database entirely.
fn into_resolved(place: ApiPlace) -> Option<ResolvedPlace> {
    let location = place.location?;
    let component = |wanted: &str| {
        place
            .address_components
            .iter()
            .find(|component| component.types.iter().any(|kind| kind == wanted))
            .and_then(|component| component.long_text.clone())
    };

    Some(ResolvedPlace {
        name: place
            .display_name
            .and_then(|name| name.text)
            .unwrap_or_default(),
        address: place.formatted_address,
        lat: location.latitude,
        lng: location.longitude,
        place_id: place.id,
        primary_type: place.primary_type,
        types: place.types,
        metro: component("administrative_area_level_2"),
        locality: component("locality"),
        country: component("country"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_address_is_not_resolvable() {
        assert!(
            !PlaceQuery {
                name: "Blue Bottle",
                address: "  ",
            }
            .is_resolvable()
        );
        assert!(
            PlaceQuery {
                name: "Blue Bottle",
                address: "1 Ferry Building, San Francisco",
            }
            .is_resolvable()
        );
    }

    #[tokio::test]
    async fn the_unconfigured_resolver_finds_nothing_and_does_not_invent() {
        let resolver = UnconfiguredResolver;
        let resolved = resolver
            .resolve(PlaceQuery {
                name: "Blue Bottle",
                address: "1 Ferry Building, San Francisco",
            })
            .await
            .expect("no transport involved");
        assert_eq!(resolved, None);
    }
}
