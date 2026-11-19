use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub enum State {
    #[default]
    Idle,
    AwaitingPhoto,
    AwaitingAiPostConfirm {
        draft_id: i64,
    },
    AwaitingAiPostEdit {
        draft_id: i64,
    },
    CryptoInvoice {
        step: u8,
        amount: Option<String>,
        asset: Option<String>,
        description: Option<String>,
    },
    /// End-user moderation appeal (multibot foundation Step 4.4, L8). Carries
    /// ONLY the opaque decision reference and its scope — never the held
    /// message body, a username, or any other content. The next text message
    /// is treated as the buyer's short reason and then the state is cleared.
    AwaitingAppealReason {
        seller_id: i64,
        bot_id: i64,
        message_ref: String,
        decision_id: Option<i64>,
    },
}
