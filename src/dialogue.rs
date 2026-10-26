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
}
