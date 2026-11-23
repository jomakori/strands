//! Acceptance tests for staging demo mode (ticket C3).
//!
//! These run the real binary, not the router: the ticket's promise is about the
//! process starting, so the assertions are about exit status, stderr and what a
//! browser would get back. The config-level cases (and the double-seed row
//! counts) live next to the code in `strands_server::demo::tests`.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_strands-server");
const STAGING_URL: &str = "https://staging-strands.maklab.net";

/// A `Command` with a clean environment: **no** `OIDC_*` variable can leak in
/// from the developer's shell or from CI, which is the point of the boot test.
fn clean_command() -> Command {
    let mut command = Command::new(BIN);
    command
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("RUST_LOG", "error")
        // The LLM client is the one other fail-at-boot setting; demo mode makes
        // no promises about it, so give it a value that parses.
        .env("LITELLM_BASE_URL", "http://127.0.0.1:1")
        .env("LITELLM_MASTER_KEY", "sk-demo-test");
    command
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a port");
    listener.local_addr().expect("local addr").port()
}

fn database_url_or_skip(test_name: &str) -> Option<String> {
    let url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty());
    let required = std::env::var("STRANDS_TEST_REQUIRE_DB").is_ok_and(|value| value == "1");

    match (url, required) {
        (Some(url), _) => Some(url),
        (None, true) => panic!("STRANDS_TEST_REQUIRE_DB is set but DATABASE_URL is not"),
        (None, false) => {
            eprintln!(
                "skipping {test_name}: DATABASE_URL is not set (no live Postgres on this host)"
            );
            None
        }
    }
}

/// The binary is spawned for the length of one test; if the test panics the
/// process must not outlive it.
struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The staging boot this ticket exists for: `STRANDS_DEMO_MODE=true`, zero
/// `OIDC_*` variables, and a server that answers `/api/v1/me` as one fixed
/// account to a client that never logged in.
#[tokio::test]
async fn demo_mode_boots_with_no_oidc_variables() {
    let Some(database_url) =
        database_url_or_skip("demo_mode::demo_mode_boots_with_no_oidc_variables")
    else {
        return;
    };

    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let log_path = std::env::temp_dir().join(format!("strands-demo-boot-{port}.log"));
    let log = std::fs::File::create(&log_path).expect("create the boot log");

    let child = clean_command()
        .env("DATABASE_URL", database_url)
        .env("PUBLIC_BASE_URL", STAGING_URL)
        .env("STRANDS_DEMO_MODE", "true")
        .env("STRANDS_BIND_ADDR", format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::from(log))
        .spawn()
        .expect("spawn strands-server");
    let mut server = Server(child);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("http client");

    let failed = |server: &mut Server| -> String {
        let status = server.0.try_wait().ok().flatten();
        let log = std::fs::read_to_string(&log_path).unwrap_or_default();
        format!("server did not become healthy (status: {status:?})\n--- stderr ---\n{log}")
    };

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match client.get(format!("{base}/healthz")).send().await {
            Ok(response) if response.status().is_success() => break,
            _ => {}
        }
        if Instant::now() > deadline {
            panic!("{}", failed(&mut server));
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // No cookie, no login: the demo account is what every request resolves to.
    let body = client
        .get(format!("{base}/api/v1/me"))
        .send()
        .await
        .expect("GET /api/v1/me")
        .error_for_status()
        .expect("/api/v1/me is 200 in demo mode")
        .text()
        .await
        .expect("read /api/v1/me");

    assert!(body.contains("demo@strands.test"), "/api/v1/me: {body}");
    assert!(body.contains("Demo account"), "/api/v1/me: {body}");

    // And it is the account the seed filled, not an empty one.
    let tags = client
        .get(format!("{base}/api/v1/tags"))
        .send()
        .await
        .expect("GET /api/v1/tags");
    assert!(
        tags.status().is_success(),
        "/api/v1/tags: {}",
        tags.status()
    );
    let tags = tags.text().await.expect("read /api/v1/tags");
    assert!(tags.contains("Kyoto in November"), "/api/v1/tags: {tags}");

    let _ = std::io::stdout().flush();
}

/// The guard: the demo switch on a production-shaped deployment is a refusal to
/// start, with a non-zero exit and a message that names the reason.
#[test]
fn production_guard_exits_non_zero_and_names_the_reason() {
    let output = clean_command()
        // Deliberately unreachable: the guard must fire before the process opens
        // a connection to what it thinks is production data.
        .env("DATABASE_URL", "postgres://127.0.0.1:1/production")
        .env("PUBLIC_BASE_URL", "https://strands.maklab.net")
        .env("STRANDS_DEMO_MODE", "1")
        .env("STRANDS_ENV", "production")
        .stdin(Stdio::null())
        .output()
        .expect("run strands-server");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "demo mode must not start in production\n{stderr}"
    );
    assert_eq!(
        output.status.code(),
        Some(1),
        "a deliberate refusal exits 1, not a panic: {stderr}"
    );
    assert!(stderr.contains("refusing to start"), "{stderr}");
    assert!(stderr.contains("STRANDS_DEMO_MODE"), "{stderr}");
    assert!(stderr.contains("STRANDS_ENV=production"), "{stderr}");
}

/// The other half of the guard: the same production host with no environment
/// marker is still refused, and the message says which host it saw.
#[test]
fn production_guard_refuses_a_production_host_without_a_marker() {
    let output = clean_command()
        .env("DATABASE_URL", "postgres://127.0.0.1:1/production")
        .env("PUBLIC_BASE_URL", "https://strands.maklab.net")
        .env("STRANDS_DEMO_MODE", "1")
        .stdin(Stdio::null())
        .output()
        .expect("run strands-server");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("refusing to start"), "{stderr}");
    assert!(stderr.contains("strands.maklab.net"), "{stderr}");
    assert!(stderr.contains("staging-strands.maklab.net"), "{stderr}");
}

/// Production behaviour is unchanged when the variable is absent: OIDC is still
/// required, and the failure is the pre-C3 one.
#[test]
fn absent_demo_mode_still_requires_oidc() {
    let output = clean_command()
        .env("DATABASE_URL", "postgres://127.0.0.1:1/production")
        .env("PUBLIC_BASE_URL", STAGING_URL)
        .stdin(Stdio::null())
        .output()
        .expect("run strands-server");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("OIDC_ISSUER and OIDC_CLIENT_ID must be set"),
        "{stderr}"
    );
    assert!(
        !stderr.contains("STRANDS_DEMO_MODE"),
        "demo mode must not be mentioned when it was never requested: {stderr}"
    );
}
