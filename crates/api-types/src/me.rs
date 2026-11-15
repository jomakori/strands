//! Profile DTO: `GET /api/v1/me`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The signed-in user, as the SPA sees them. `avatar_url` is absent for
/// accounts whose provider sends no picture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub avatar_url: Option<String>,
}
