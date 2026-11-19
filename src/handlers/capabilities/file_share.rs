//! File Share capability — compiled only with the `file_share` feature.
//! Vertical handler deferred; see `movie_bot.rs` for the notice contract.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "file_share",
    feature: "file_share",
    label: "File Share",
    inbound: InboundHandler::VerticalStub,
};
