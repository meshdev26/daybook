//! Product delivery: the post-payment fulfillment DM.
//!
//! Single-sourced so the three grant sites (background granter in the bin,
//! `/paid`, and the claim API) send byte-identical deliveries. Delivery is
//! strictly a private message to the buyer — payloads are never rendered in
//! channels or group chats.

/// Deliver a purchased product to the buyer's private chat.
///
/// - `digital_link` / `digital_file`: sends `Delivery: {name}\n{delivery}` where
///   `delivery` is the link, text, or Telegram `file_id` stored on the
///   product. The payload is required: a product without one is a seller
///   configuration error and surfaces as `Err` (never an empty delivery).
/// - `physical`: sends the order receipt ("order received, seller will
///   ship"); nothing is shipped by the platform.
/// - any other kind is rejected: new kinds must be wired deliberately, never
///   silently delivered as the wrong thing.
///
/// Never fails the surrounding grant flow fatally: callers decide whether a
/// delivery error is retried; the grant itself is already durable.
// The extra two arguments are the outbound moderation scope (seller/bot);
// keeping them explicit makes the gate's tenant derivation auditable at every
// call site. Mirrors the crate-wide allowance in `handlers::commands`.
#[allow(clippy::too_many_arguments)]
pub async fn deliver(
    bot: &teloxide::prelude::Bot,
    user_id: i64,
    kind: &str,
    name: &str,
    delivery: Option<&str>,
    db: &crate::db::Db,
    seller_id: i64,
    bot_id: Option<i64>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use teloxide::prelude::Requester as _;
    let chat = teloxide::types::ChatId(user_id);
    // L10 outbound gate: every text DM is a NON-ContentSender, content-bearing
    // send. The policy decision lives in the central helper; the caller passes
    // the invoice's seller scope so seller rules apply alongside the floor.
    match kind {
        // A file_id payload re-sends the actual document; anything else
        // (http(s) link or plain text) goes out as a message.
        "digital_file" => {
            let payload = delivery.filter(|d| !d.trim().is_empty()).ok_or_else(|| {
                format!(
                    "product '{}' ({}) has no delivery payload configured",
                    name, kind
                )
            })?;
            if payload.starts_with("http://")
                || payload.starts_with("https://")
                || !payload
                    .trim_start_matches('-')
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric())
            {
                let text = format!("Delivery: {name}\n{payload}");
                crate::handlers::moderation::gate_outbound(db, seller_id, bot_id, &text)?;
                bot.send_message(chat, text).await?;
            } else {
                bot.send_document(
                    chat,
                    teloxide::types::InputFile::file_id(teloxide::types::FileId(
                        payload.to_string(),
                    )),
                )
                .await?;
            }
        }
        "digital_link" => {
            let payload = delivery.filter(|d| !d.trim().is_empty()).ok_or_else(|| {
                format!(
                    "product '{}' ({}) has no delivery payload configured",
                    name, kind
                )
            })?;
            let text = format!("Delivery: {name}\n{payload}");
            crate::handlers::moderation::gate_outbound(db, seller_id, bot_id, &text)?;
            bot.send_message(chat, text).await?;
        }
        "physical" => {
            let text = format!(
                "Order received for '{}'. The seller will ship it to you soon.\nYou can track it with /orders once the seller ships.",
                name
            );
            crate::handlers::moderation::gate_outbound(db, seller_id, bot_id, &text)?;
            bot.send_message(chat, text).await?;
        }
        other => {
            return Err(format!("unknown product kind '{}'", other).into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Regression: product delivery is a NON-`ContentSender`, content-bearing
    /// DM path. A seller-scoped floor term matching the delivery text must
    /// hold it BEFORE the raw-Bot send — the mock Telegram server sees zero
    /// requests and the error carries only the opaque `moderation_hold` marker.
    #[tokio::test]
    async fn delivery_is_held_before_transport_by_seller_term() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "result": {"message_id": 1, "date": 1_700_000_000,
                           "chat": {"id": 42, "type": "private", "first_name": "T"},
                           "text": "x"}
            })))
            .mount(&server)
            .await;

        let dir = tempfile::tempdir().unwrap();
        let db = crate::db::Db::new(dir.path().join("t.db").to_str().unwrap()).unwrap();
        assert!(db.add_blocked_word(55, "forbidden", "hide", true));
        let bot = teloxide::Bot::new("0:placeholder")
            .set_api_url(server.uri().parse::<reqwest::Url>().unwrap());

        let err = super::deliver(
            &bot,
            42,
            "digital_link",
            "forbidden item",
            Some("https://example.com/file"),
            &db,
            55,
            Some(3),
        )
        .await
        .expect_err("seller term must hold the delivery DM");
        assert!(err.to_string().contains("moderation_hold"), "got {err:?}");
        assert!(
            !err.to_string().contains("forbidden"),
            "no body in the error"
        );
        assert!(
            server.received_requests().await.unwrap().is_empty(),
            "no send may reach Telegram while held"
        );
    }
}
