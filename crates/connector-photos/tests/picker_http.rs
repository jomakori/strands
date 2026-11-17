//! The Picker client against a real socket (ticket T14).
//!
//! These tests exist because the interesting failures in this lane are protocol
//! failures, not logic failures: a `mediaItemsSet` that never flips, a session
//! Google has forgotten, a `baseUrl` whose 60 minutes are up. A hand-written
//! mock would encode our *belief* about those answers; a socket serving the
//! documented shapes exercises the client that ships — reqwest, JSON decoding,
//! status mapping and all.
//!
//! The server here is deliberately dumb: no routing framework, no TLS, canned
//! replies keyed by path prefix. It proves the client's HTTP contract, and
//! nothing about Google.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use strands_connector_photos::{GooglePicker, PhotosError, PickerApi};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One canned HTTP answer.
#[derive(Clone)]
struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Reply {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json",
            body: body.into().into_bytes(),
        }
    }

    fn image(bytes: &[u8]) -> Self {
        Self {
            status: 200,
            content_type: "image/jpeg",
            body: bytes.to_vec(),
        }
    }

    fn status(status: u16) -> Self {
        Self {
            status,
            content_type: "text/plain",
            body: Vec::new(),
        }
    }
}

type Table = Arc<Mutex<Vec<(String, VecDeque<Reply>)>>>;

/// A localhost stand-in whose replies can be extended once its address is known
/// (the item fixtures have to name the server in their `baseUrl`).
struct Fake {
    origin: String,
    table: Table,
}

impl Fake {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let table: Table = Arc::new(Mutex::new(Vec::new()));

        let serving = Arc::clone(&table);
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buffer = [0u8; 4096];
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buffer[..read]).to_string();
                let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
                let method = parts.next().unwrap_or("GET").to_owned();
                let target = parts.next().unwrap_or("/").to_owned();
                let path = target.split('?').next().unwrap_or("/").to_owned();

                let reply = {
                    let mut routes = serving.lock().expect("routes");
                    // Longest matching prefix wins, so `/sessions/s-1` cannot be
                    // shadowed by `/sessions`.
                    let mut best: Option<usize> = None;
                    for (index, (prefix, _)) in routes.iter().enumerate() {
                        if format!("{method} {path}").starts_with(prefix.as_str())
                            && best.is_none_or(|current| {
                                routes[index].0.len() > routes[current].0.len()
                            })
                        {
                            best = Some(index);
                        }
                    }
                    best.and_then(|index| {
                        let queue = &mut routes[index].1;
                        // The last reply repeats: that is how a poll that starts
                        // pending and ends ready is expressed.
                        if queue.len() > 1 {
                            queue.pop_front()
                        } else {
                            queue.front().cloned()
                        }
                    })
                }
                .unwrap_or_else(|| Reply::status(404));

                let response = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    reply.status,
                    reason(reply.status),
                    reply.content_type,
                    reply.body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.write_all(&reply.body).await;
                let _ = socket.flush().await;
            }
        });

        Self { origin, table }
    }

    fn base(&self) -> String {
        format!("{}/v1", self.origin)
    }

    fn reply(&self, method_path: &str, replies: Vec<Reply>) {
        self.table
            .lock()
            .expect("routes")
            .push((method_path.to_owned(), replies.into()));
    }

    fn picker(&self) -> GooglePicker {
        GooglePicker::with_api_base(self.base()).expect("build the picker")
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        410 => "Gone",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

fn session_body(media_items_set: bool) -> String {
    format!(
        r#"{{"id":"s-1","pickerUri":"https://photos.example/pick/s-1","expireTime":"2030-01-01T00:00:00Z","mediaItemsSet":{media_items_set}}}"#
    )
}

fn item(id: &str, base_url: &str, caption: Option<&str>) -> String {
    let caption = match caption {
        Some(text) => format!(r#","caption":"{text}""#),
        None => String::new(),
    };
    format!(
        r#"{{"id":"{id}","createTime":"2026-03-01T10:00:00Z"{caption},"mediaFile":{{"baseUrl":"{base_url}","mimeType":"image/jpeg","mediaFileMetadata":{{"width":1024,"height":768}}}}}}"#
    )
}

#[tokio::test]
async fn a_pick_round_trip_reads_the_items_and_their_bytes() {
    let fake = Fake::start().await;
    let first = format!("{}/preview/a", fake.origin);
    let second = format!("{}/preview/b", fake.origin);

    fake.reply("POST /v1/sessions", vec![Reply::json(session_body(false))]);
    // Pending first, ready after: the poll has to see both.
    fake.reply(
        "GET /v1/sessions/s-1",
        vec![
            Reply::json(session_body(false)),
            Reply::json(session_body(true)),
        ],
    );
    fake.reply(
        "GET /v1/mediaItems",
        vec![Reply::json(format!(
            r#"{{"mediaItems":[{}]}}"#,
            [
                item("item-a", &first, Some("Sunrise over the bay")),
                item("item-b", &second, None),
            ]
            .join(",")
        ))],
    );
    fake.reply("GET /preview/a", vec![Reply::image(b"jpeg-a")]);
    fake.reply("GET /preview/b", vec![Reply::image(b"jpeg-b")]);

    let picker = fake.picker();
    let session = picker.create_session("token").await.expect("create");
    assert_eq!(session.id, "s-1");
    assert_eq!(session.picker_uri, "https://photos.example/pick/s-1");

    let pending = picker.session_status("token", "s-1").await.expect("poll");
    assert!(!pending.media_items_set, "the user has not picked yet");

    let ready = picker.session_status("token", "s-1").await.expect("poll");
    assert!(ready.media_items_set, "the user has finished picking");

    let items = picker.list_items("token", "s-1").await.expect("list");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].id, "item-a");
    assert_eq!(items[0].caption.as_deref(), Some("Sunrise over the bay"));
    assert_eq!(items[0].width, 1024);
    assert_eq!(items[1].caption, None);

    let thumbnail = picker.thumbnail(&items[0].base_url).await.expect("bytes");
    assert_eq!(thumbnail.bytes, b"jpeg-a");
    assert_eq!(thumbnail.content_type, "image/jpeg");

    let thumbnail = picker.thumbnail(&items[1].base_url).await.expect("bytes");
    assert_eq!(thumbnail.bytes, b"jpeg-b");
}

#[tokio::test]
async fn a_session_google_has_forgotten_is_expired_not_refused() {
    let fake = Fake::start().await;
    // Google drops sessions it no longer retains; the handle is dead.
    fake.reply("GET /v1/sessions/s-1", vec![Reply::status(404)]);

    let error = fake
        .picker()
        .session_status("token", "s-1")
        .await
        .expect_err("a forgotten session is not a status");
    assert!(
        matches!(error, PhotosError::Expired),
        "a forgotten session must read as expired, got {error:?}"
    );
}

#[tokio::test]
async fn a_rejected_token_is_refused_not_retried() {
    let fake = Fake::start().await;
    fake.reply("POST /v1/sessions", vec![Reply::status(401)]);

    let error = fake
        .picker()
        .create_session("stale")
        .await
        .expect_err("a rejected token cannot open a session");
    assert!(
        matches!(error, PhotosError::Refused(_)),
        "401 must surface as refused, got {error:?}"
    );
}

#[tokio::test]
async fn a_lapsed_download_url_is_expired() {
    let fake = Fake::start().await;
    // Past the 60-minute window Google answers 403 to the same URL.
    fake.reply("GET /preview/a", vec![Reply::status(403)]);

    let error = fake
        .picker()
        .thumbnail(&format!("{}/preview/a", fake.origin))
        .await
        .expect_err("a lapsed url cannot be downloaded");
    assert!(
        matches!(error, PhotosError::Expired),
        "a lapsed baseUrl must read as expired, got {error:?}"
    );
}

#[tokio::test]
async fn an_oversized_preview_is_refused_by_the_cap() {
    let fake = Fake::start().await;
    let oversized = vec![0u8; strands_connector_photos::MAX_THUMBNAIL_BYTES + 1];
    fake.reply("GET /preview/a", vec![Reply::image(&oversized)]);

    let error = fake
        .picker()
        .thumbnail(&format!("{}/preview/a", fake.origin))
        .await
        .expect_err("a preview past the cap must not be stored");
    assert!(
        matches!(error, PhotosError::ThumbnailTooLarge),
        "an oversized preview must be refused, got {error:?}"
    );
}
