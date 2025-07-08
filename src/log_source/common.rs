use crate::log_source::types::{Bid};
use url::Url;
use rust_decimal::Decimal;
use std::collections::{HashMap, HashSet};
use crate::log_source::stats_writer::RewardStats;

pub fn is_relay_proxy(relay: &str) -> bool {
    relay.contains("relay-proxy") || relay.contains("Relay Proxy") || relay.contains("rproxy") || relay.contains("rpoxy") // handle typo
}

pub fn parse_url(bid: &Bid) -> String {
    // Remove leading/trailing backslashes and quotes
    let trimmed = bid.relay.trim_matches('\\').trim_matches('"');
    match Url::parse(trimmed) {
        Ok(parsed_url) => parsed_url.host_str().unwrap_or("").to_string(),
        Err(e) => {
            eprintln!("Failed to parse URL from relay field '{}': {} : {}", trimmed, e,bid.relay.clone());
            trimmed.to_string()
        },
    }
}




/// Filters valid slot infos based on completeness and returns a map of UID -> SlotInfo
/// Also returns the original flattened list and number skipped (for diagnostics).
pub fn filter_valid_slot_infos<T>(slot_infos: &HashMap<String, HashMap<String, T>>)
    -> (Vec<T>,Vec<T>, HashMap<String, T>, usize)
where
    T: RewardStats + Clone,
{
    // Flatten everything
    let all_infos: Vec<T> = slot_infos
        .iter()
        .flat_map(|(_, inner)| inner.values())
        .cloned()
        .collect();

    // Select valid ones
    let selected_infos: Vec<T> = all_infos
        .iter()
        .filter(|si| {
            !si.get_block_hash().is_empty()
                && si.get_onchain_bid_value() > Decimal::ZERO
                && !si.get_uid().is_empty()
        })
        .cloned()
        .collect();

    // Build a map for downstream processing
    let selected_infos_map = selected_infos
        .iter()
        .map(|si| (si.get_uid().to_string(), si.clone()))
        .collect::<HashMap<String, T>>();

    let skipped = all_infos.len().saturating_sub(selected_infos.len());

    println!(
        "SlotInfo completeness filter: total={}, selected={}, skipped={}",
        all_infos.len(),
        selected_infos.len(),
        skipped
    );

    (all_infos, selected_infos, selected_infos_map, skipped)
}
