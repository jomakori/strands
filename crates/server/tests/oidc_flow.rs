//! The Dex OIDC client against a real socket (ticket T20c).
//!
//! The lane that can actually break here is the wire protocol: a discovery
//! document whose `issuer` disagrees with ours, a token endpoint that wants a
//! secret we no longer have, a userinfo answer that arrives with the wrong
//! bearer. A mock of our own trait would encode our belief about those answers,
//! so these tests serve the documented shapes from a localhost socket and drive
//! the client that ships — reqwest, form encoding, JSON decoding and all.
//!
//! Nothing here leaves the machine: the issuer is `http://127.0.0.1:<port>`,
//! which is exactly what `OIDC_ISSUER` would be pointed at in a local run. The
//! HTTP client is reqwest itself, so no client stubbing is needed to keep the
//! suite off the network.

use std::sync::{Arc, Mutex};

use strands_server::auth::AuthError;
use strands_server::auth::identity::{
    ConsentRequest, Endpoints, IdentityProvider, OidcIdentityProvider,
};
use strands_server::config::AuthConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One request the stub was handed, for assertions about *what we sent*.
#[derive(Debug, Clone)]
struct Recorded {
    method: String,
    path: String,
    body: String,
    authorization: Option<String>,
}

/// A localhost issuer: discovery, token and userinfo, canned.
struct Stub {
    origin: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Stub {
    /// Start a stub whose discovery document names itself. `doc_issuer_suffix`
    /// is appended to the origin so a test can serve a document that lies about
    /// which issuer it describes.
    async fn start(doc_issuer_suffix: &'static str) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));

        let shared = Arc::clone(&requests);
        let document_issuer = format!("{origin}{doc_issuer_suffix}");
        let advertised = origin.clone();

        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let Some(request) = read_request(&mut socket).await else {
                    continue;
                };
                shared.lock().unwrap().push(request.clone());

                let (status, content_type, body) = match request.path.as_str() {
                    "/.well-known/openid-configuration" => (
                        200,
                        "application/json",
                        format!(
                            r#"{{"issuer":"{document_issuer}",
                                "authorization_endpoint":"{advertised}/auth",
                                "token_endpoint":"{advertised}/token",
                                "userinfo_endpoint":"{advertised}/userinfo",
                                "code_challenge_methods_supported":["S256","plain"]}}"#
                        ),
                    ),
                    // A code the issuer refuses, as it would for a replay or a
                    // verifier that does not hash to the challenge.
                    "/token" if request.body.contains("code=replayed") => (
                        400,
                        "application/json",
                        r#"{"error":"invalid_grant"}"#.to_owned(),
                    ),
                    "/token" => (
                        200,
                        "application/json",
                        r#"{"access_token":"at-1","token_type":"Bearer",
                            "id_token":"header.payload.signature"}"#
                            .to_owned(),
                    ),
                    "/userinfo" => match request.authorization.as_deref() {
                        Some("Bearer at-1") => (
                            200,
                            "application/json",
                            r#"{"sub":"dex-user-1","email":"alice@example.com",
                                "email_verified":true,"name":"Alice Example",
                                "picture":"https://example.com/alice.png"}"#
                                .to_owned(),
                        ),
                        _ => (
                            401,
                            "application/json",
                            r#"{"error":"invalid_token"}"#.to_owned(),
                        ),
                    },
                    _ => (404, "text/plain", String::new()),
                };

                write_response(&mut socket, status, content_type, &body).await;
            }
        });

        Self { origin, requests }
    }

    fn config(&self) -> AuthConfig {
        AuthConfig {
            issuer: self.origin.clone(),
            client_id: "strands".into(),
            public_base_url: "https://strands.example".into(),
            cookie_secure: true,
            session_ttl_days: 30,
        }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    fn count(&self, path: &str) -> usize {
        self.requests()
            .iter()
            .filter(|request| request.path == path)
            .count()
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Recorded> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let path = request_line.next()?.to_owned();

    let mut content_length = 0usize;
    let mut authorization = None;
    for line in lines {
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().ok()?;
        } else if line.len() > 14 && line[..14].eq_ignore_ascii_case("authorization:") {
            authorization = Some(line[14..].trim().to_owned());
        }
    }

    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }

    Some(Recorded {
        method,
        path,
        body: String::from_utf8_lossy(&body).to_string(),
        authorization,
    })
}

async fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        if status == 200 { "OK" } else { "Error" },
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

fn authorization_url(provider: &OidcIdentityProvider) -> String {
    provider.consent_url(&ConsentRequest {
        state: "state-1",
        nonce: "nonce-1",
        code_challenge: "challenge-1",
        redirect_uri: "https://strands.example/auth/google/callback",
    })
}

/// The whole flow, one pass: discover, authorize, exchange with the verifier and
/// no secret, then read the email from userinfo.
#[tokio::test]
async fn discovery_then_exchange_reads_identity_from_userinfo() {
    let stub = Stub::start("").await;
    let config = stub.config();

    let provider = OidcIdentityProvider::discover(&config)
        .await
        .expect("discovery against a conforming document");

    assert_eq!(
        provider.endpoints(),
        &Endpoints {
            authorization: format!("{}/auth", stub.origin),
            token: format!("{}/token", stub.origin),
            userinfo: format!("{}/userinfo", stub.origin),
        }
    );

    // The authorize request: PKCE S256, state, nonce — and nothing secret.
    let url = authorization_url(&provider);
    assert!(url.starts_with(&format!("{}/auth?", stub.origin)), "{url}");
    assert!(url.contains("code_challenge=challenge-1"), "{url}");
    assert!(url.contains("code_challenge_method=S256"), "{url}");
    assert!(url.contains("state=state-1"), "{url}");
    assert!(url.contains("nonce=nonce-1"), "{url}");
    assert!(!url.contains("client_secret"), "{url}");

    let identity = provider
        .exchange_code("the-code", "the-verifier", &config.redirect_uri())
        .await
        .expect("exchange against the stub");

    // Identity came from /userinfo, not from the id_token the stub also sent.
    assert_eq!(identity.subject, "dex-user-1");
    assert_eq!(identity.email, "alice@example.com");
    assert_eq!(identity.name, "Alice Example");
    assert_eq!(
        identity.avatar_url.as_deref(),
        Some("https://example.com/alice.png")
    );

    let requests = stub.requests();

    let token = requests
        .iter()
        .find(|request| request.path == "/token")
        .expect("the token endpoint was called");
    assert_eq!(token.method, "POST");
    assert!(
        token.body.contains("grant_type=authorization_code"),
        "{}",
        token.body
    );
    assert!(token.body.contains("code=the-code"), "{}", token.body);
    assert!(
        token.body.contains("code_verifier=the-verifier"),
        "{}",
        token.body
    );
    assert!(token.body.contains("client_id=strands"), "{}", token.body);
    assert!(
        token
            .body
            .contains("redirect_uri=https%3A%2F%2Fstrands.example%2Fauth%2Fgoogle%2Fcallback"),
        "the exchange replays the redirect the browser used: {}",
        token.body
    );
    // The ticket's core assertion: a public client sends no secret.
    assert!(
        !token.body.contains("client_secret") && !token.body.contains("secret"),
        "no secret in the token request: {}",
        token.body
    );

    let userinfo = requests
        .iter()
        .find(|request| request.path == "/userinfo")
        .expect("userinfo was called");
    assert_eq!(userinfo.authorization.as_deref(), Some("Bearer at-1"));
}

/// Discovery happens once, at boot, and the endpoint list is then in memory: a
/// second login must not re-fetch the document.
#[tokio::test]
async fn discovery_is_fetched_once_and_then_cached_in_memory() {
    let stub = Stub::start("").await;
    let provider = OidcIdentityProvider::discover(&stub.config())
        .await
        .unwrap();
    let config = stub.config();

    for code in ["first", "second"] {
        provider
            .exchange_code(code, "verifier", &config.redirect_uri())
            .await
            .unwrap();
    }

    assert_eq!(
        stub.count("/.well-known/openid-configuration"),
        1,
        "the endpoint list is resolved once per process"
    );
    assert_eq!(stub.count("/token"), 2);
}

/// A rejected code is the caller's error, and it does not become a 500.
#[tokio::test]
async fn a_rejected_code_is_an_invalid_code_error() {
    let stub = Stub::start("").await;
    let provider = OidcIdentityProvider::discover(&stub.config())
        .await
        .unwrap();

    let error = provider
        .exchange_code(
            "replayed",
            "verifier",
            "https://strands.example/auth/google/callback",
        )
        .await
        .unwrap_err();

    assert!(matches!(error, AuthError::InvalidCode), "{error}");
    assert_eq!(stub.count("/userinfo"), 0, "nothing to look up");
}

/// A document that names a different issuer is refused at discovery, so a
/// misdirected fetch cannot silently point the whole flow somewhere else.
#[tokio::test]
async fn a_discovery_document_naming_another_issuer_is_refused() {
    let stub = Stub::start("/elsewhere").await;

    let error = OidcIdentityProvider::discover(&stub.config())
        .await
        .err()
        .expect("a mismatched issuer must not be accepted");

    assert!(error.to_string().contains("issuer"), "{error}");
}
