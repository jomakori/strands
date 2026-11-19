//! Live pairing proof: the real transport, the real WhatsApp servers.
//!
//! Ignored by default because it opens a socket to WhatsApp and needs no
//! credentials — the phone scans the code, which is exactly why nothing here
//! can finish a link. What it proves is the part before the scan: the bridge
//! comes up and offers a user-actionable QR (and a pair code when a phone
//! number is configured), which is what the Settings row renders.
//!
//! Run with: cargo test --package strands-server --test whatsapp_pairing_live -- --ignored --nocapture

use std::time::Duration;

use strands_server::whatsapp::bridge::{LinkState, SessionFactory};
use strands_server::whatsapp::live::LiveFactory;
use uuid::Uuid;

#[tokio::test]
#[ignore = "opens a socket to WhatsApp; needs no credentials"]
async fn the_live_bridge_offers_a_scannable_code() {
    let factory = LiveFactory::from_env();
    let session = factory
        .open(Uuid::new_v4(), None)
        .await
        .expect("open the live WhatsApp client");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    loop {
        match session.state() {
            LinkState::AwaitingScan { qr, pair_code } => {
                let qr_ready = qr.map(|code| !code.value.is_empty()).unwrap_or(false);
                let pair_ready = pair_code
                    .map(|code| !code.value.is_empty())
                    .unwrap_or(false);
                if qr_ready || pair_ready {
                    eprintln!(
                        "live bridge ready: qr={qr_ready} pair_code={pair_ready} \
                         (scan it from WhatsApp → Linked devices)"
                    );
                    break;
                }
            }
            other => panic!("unexpected state before the scan: {other:?}"),
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the bridge did not produce a QR within 90s"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    session.unlink().await.expect("close the live client");
}
