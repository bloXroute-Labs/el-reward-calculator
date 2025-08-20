// src/lib.rs

// Expose your module tree
pub mod log_source;

// Re-export types at the crate root so existing `use crate::...` imports keep working
pub use chrono::Utc;
pub use log_source::types::{Bid, CommitBoostSlotInfo, SlotInfo, SlotTrait};

// ----------------- Type aliases expected at the crate root -----------------
// Adjust these aliases if your actual shapes differ.
// Most repos use: HashMap<slot, HashMap<slot_uid, Info>>
pub type SlotInfos = std::collections::HashMap<
    String,
    std::collections::HashMap<String, log_source::types::SlotInfo>
>;

pub type CommitBoostSlotInfos = std::collections::HashMap<
    String,
    std::collections::HashMap<String, log_source::types::CommitBoostSlotInfo>
>;

// ----------------- Compatibility shim for a quirky import ------------------
// Some modules have: `use crate::mevboost_json::serde_json::to_writer_pretty;`
// Provide that path without touching the file.
pub mod mevboost_json {
    pub use serde_json; // so `crate::mevboost_json::serde_json::to_writer_pretty` works
}

// (Optional) Convenience re-exports for commitboost_text API
pub use log_source::commitboost_text::{post_process_all_slots, process_lines};
