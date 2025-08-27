// src/lib.rs

pub mod log_source;

pub use chrono::Utc;
pub use log_source::types::{Bid, CommitBoostSlotInfo, SlotInfo, SlotTrait};

pub type SlotInfos = std::collections::HashMap<
    String,
    std::collections::HashMap<String, log_source::types::SlotInfo>
>;

pub type CommitBoostSlotInfos = std::collections::HashMap<
    String,
    std::collections::HashMap<String, log_source::types::CommitBoostSlotInfo>
>;

pub mod mevboost_json {
    pub use serde_json;
}

pub use log_source::commitboost_text::{post_process_all_slots, process_lines};
