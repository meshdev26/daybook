//! File Share capability — compiled only with the `file_share` feature.
//!
//! The only document send in the codebase is post-payment product delivery,
//! tied to an order's seller and invoice. There is no upload, browse or
//! share flow for arbitrary files, so activating this type would mean
//! inventing a product rather than reusing one. It stays a notice-only stub.

use super::{Descriptor, InboundHandler};

pub const DESCRIPTOR: Descriptor = Descriptor {
    bot_type: "file_share",
    feature: "file_share",
    label: "File Share",
    inbound: InboundHandler::VerticalStub,
};
