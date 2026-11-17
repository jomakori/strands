//! The SPA bundle, served by the API's own router (ticket T20a).
//!
//! One deployable (spec §6): the service that answers `/api` is the service
//! that answers the browser. That keeps the session cookie first-party, needs
//! no second web tier, and means a deploy is one image — there is nowhere for
//! the bundle and the API to disagree about which release is live.
//!
//! Two rules decide every request that no route claimed:
//!
//! * `/api` and `/auth` belong to the server. A miss under either stays a JSON
//!   404 — the SPA fallback must never swallow it, or a typo'd API call comes
//!   back as `index.html` with a 200 and the client parses HTML as JSON.
//! * Everything else is a file if one exists, and the shell (`index.html`) if
//!   one does not — that is what makes `/tags/<uuid>` survive a reload, since
//!   the client router owns those paths and the server has no route for them.

use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, HeaderValue};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};

use crate::app::AppState;
use crate::error::AppError;

/// Where the bundle lives, overridable so the image, a local `cargo run`, and
/// the deploy can each point at their own checkout of `dx build`'s output.
pub const ENV: &str = "STRANDS_ASSETS_DIR";
/// `dx build` writes `target/dx/…/release/web/public`; the image copies that
/// to `dist`, which is what an unconfigured `cargo run` looks for.
pub const DEFAULT_DIR: &str = "dist";

/// Prefixes the server owns. A miss under one of these is never the SPA.
const SERVER_PREFIXES: [&str; 2] = ["/api", "/auth"];

/// A year, for a filename that carries its own content hash: that URL can
/// never mean a different byte sequence, so revalidating it is pure latency.
const IMMUTABLE: &str = "public, max-age=31536000, immutable";
/// Not hashed (hand-named CSS, icons): cacheable, but only briefly, because a
/// new build reuses the name.
const SHORT_LIVED: &str = "public, max-age=3600";
/// The shell names the hashed bundles by URL. Storing it would pin a client to
/// an asset the next deploy deleted, so it is never cached.
const NO_STORE: &str = "no-store";

/// The web bundle on disk.
#[derive(Clone, Debug)]
pub struct Assets {
    dir: PathBuf,
}

/// A bundle path that cannot be served, reported at boot.
#[derive(Debug, thiserror::Error)]
pub enum AssetsError {
    #[error("{ENV}={path} does not exist or is not a directory")]
    Missing { path: PathBuf },
    #[error("{ENV}={path} holds no index.html")]
    NoIndex { path: PathBuf },
}

impl Assets {
    /// A bundle at `dir`. Existence is the caller's problem — [`resolve`] is
    /// the one that reports it.
    ///
    /// [`resolve`]: Assets::resolve
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The bundle the process should serve, if any.
    ///
    /// An explicit [`ENV`] that points nowhere is a packaging mistake — the
    /// image was built without the bundle, or mounted it at the wrong path —
    /// so it fails at boot rather than answering 404s that look like routing
    /// bugs. An unset [`ENV`] with no default directory is a plain dev run
    /// (`cargo run` from the repo, no `dx build` yet): the API is still worth
    /// having, so the SPA is simply absent.
    pub fn resolve() -> Result<Option<Self>, AssetsError> {
        let configured = std::env::var(ENV)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        let dir = configured
            .as_deref()
            .map_or_else(|| PathBuf::from(DEFAULT_DIR), PathBuf::from);

        if !dir.is_dir() {
            return match configured {
                Some(_) => Err(AssetsError::Missing { path: dir }),
                None => Ok(None),
            };
        }
        if !dir.join("index.html").is_file() {
            return Err(AssetsError::NoIndex { path: dir });
        }
        Ok(Some(Self::new(dir)))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file the path names, or `None` when there is no such file.
    ///
    /// Delegated to [`ServeDir`] rather than opened by hand: it resolves
    /// percent-encoded paths, refuses traversal, and sets the content type,
    /// which are exactly the three things a hand-rolled version gets wrong.
    async fn file(&self, method: Method, path: &str) -> Option<Response> {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .ok()?;
        let response = ServeDir::new(&self.dir)
            .append_index_html_on_directories(false)
            .oneshot(request)
            .await
            .ok()?;

        // Anything but 200 (`404`, `400` for a path ServeDir rejects) is
        // "no such file"; the caller decides what that means. The body type is
        // ServeDir's own, so it is normalised to axum's at the boundary.
        (response.status() == StatusCode::OK)
            .then(|| with_cache(response, cache_control_for(path)).map(Body::new))
    }

    /// The shell, for a client-side route.
    async fn shell(&self, method: Method) -> Option<Response> {
        let request = Request::builder()
            .method(method)
            .uri("/")
            .body(Body::empty())
            .ok()?;
        let response = ServeFile::new(self.dir.join("index.html"))
            .oneshot(request)
            .await
            .ok()?;
        (response.status() == StatusCode::OK).then(|| with_cache(response, NO_STORE).map(Body::new))
    }
}

/// Fallback for every path no route claimed.
pub async fn fallback(State(state): State<AppState>, request: Request) -> Response {
    let path = request.uri().path().to_owned();

    if owns_path(&path) {
        return AppError::NotFound.into_response();
    }

    // Reads only: a POST to a client-side route is a mistake worth a 404, and
    // answering it with the shell would look like a successful write.
    let method = request.method().clone();
    if method != Method::GET && method != Method::HEAD {
        return AppError::NotFound.into_response();
    }

    let Some(assets) = state.assets.as_ref() else {
        return AppError::NotFound.into_response();
    };

    if let Some(response) = assets.file(method.clone(), &path).await {
        return response;
    }

    // A miss that names a file stays a 404: answering `index.html` to a `.js`
    // or `.wasm` request ships HTML under a script content type, and the
    // browser fails somewhere far less legible than the real cause.
    if names_a_file(&path) {
        return AppError::NotFound.into_response();
    }

    match assets.shell(method).await {
        Some(response) => response,
        None => AppError::NotFound.into_response(),
    }
}

/// Whether `path` belongs to the server's own surface (`/api`, `/auth`).
fn owns_path(path: &str) -> bool {
    SERVER_PREFIXES
        .iter()
        .any(|prefix| path == *prefix || path.starts_with(&format!("{prefix}/")))
}

/// Whether the last segment looks like a filename rather than a route.
fn names_a_file(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .is_some_and(|segment| segment.contains('.'))
}

/// Whether the filename carries a content hash (`index-9f8e7d6c5b4a.js`), and
/// so may be cached forever. `dx` hashes the bundle's real output; assets the
/// app ships by hand keep their name and are treated as mutable.
fn cache_control_for(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or_default();
    // No extension means no file: the request is a client-side route, which
    // is answered with the shell, which is never cached.
    if name.is_empty() || !name.contains('.') || name.ends_with(".html") {
        return NO_STORE;
    }
    let hashed = name
        .split(|character: char| !character.is_ascii_hexdigit())
        .any(|token| token.len() >= 8);
    if hashed { IMMUTABLE } else { SHORT_LIVED }
}

fn with_cache<B>(mut response: Response<B>, cache: &'static str) -> Response<B> {
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static(cache));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_is_never_cached() {
        assert_eq!(cache_control_for("/index.html"), NO_STORE);
        assert_eq!(cache_control_for("/"), NO_STORE);
        assert_eq!(cache_control_for("/assets"), NO_STORE);
    }

    #[test]
    fn hashed_output_is_cached_forever_and_hand_named_files_are_not() {
        assert_eq!(
            cache_control_for("/assets/index-9f8e7d6c5b4a.js"),
            IMMUTABLE
        );
        assert_eq!(cache_control_for("/assets/strands_bg.wasm"), SHORT_LIVED);
        assert_eq!(cache_control_for("/assets/main.css"), SHORT_LIVED);
        assert_eq!(cache_control_for("/favicon.ico"), SHORT_LIVED);
        // The names `dx build --release` actually writes: the hash follows a
        // prefix, so it is a hex run rather than the whole stem.
        assert_eq!(
            cache_control_for("/assets/strands-web-dxhb6615d78f0b1b984.js"),
            IMMUTABLE
        );
        assert_eq!(
            cache_control_for("/assets/strands-web_bg-dxhec2dd4ebbf673399.wasm"),
            IMMUTABLE
        );
    }

    #[test]
    fn the_servers_own_prefixes_are_not_client_routes() {
        for path in ["/api", "/api/v1/tags", "/api/v1/nope", "/auth/google"] {
            assert!(owns_path(path), "{path}");
        }
        for path in ["/", "/apix", "/tags/1", "/authentic"] {
            assert!(!owns_path(path), "{path}");
        }
    }

    #[test]
    fn a_last_segment_with_a_dot_is_a_file_request() {
        assert!(names_a_file("/assets/app.js"));
        assert!(names_a_file("/favicon.ico"));
        assert!(!names_a_file("/tags/1234"));
        assert!(!names_a_file("/"));
    }
}
