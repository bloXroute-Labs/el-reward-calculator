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
pub fn filter_valid_slot_infos<T>(
    slot_infos: &HashMap<String, HashMap<String, T>>,
) -> (
    Vec<T>,                  // all_infos
    Vec<T>,                  // selected_infos
    HashMap<String, T>,      // selected_infos_map
    Vec<(T, Vec<&'static str>)>, // skipped_with_reasons
)
where
    T: RewardStats + Clone,
{
    let all_infos: Vec<T> = slot_infos
        .iter()
        .flat_map(|(_, inner)| inner.values())
        .cloned()
        .collect();

    let mut selected_infos = Vec::new();
    let mut skipped_with_reasons = Vec::new();

    for info in &all_infos {
        let mut reasons = Vec::new();

        if info.get_uid().is_empty() {
            reasons.push("UID empty");
        }
        if info.get_block_hash().is_empty() {
            reasons.push("BlockHash empty");
        }
        if info.get_onchain_bid_value() <= Decimal::ZERO {
            reasons.push("Bid is zero or negative");
        }

        if reasons.is_empty() {
            selected_infos.push(info.clone());
        } else {
            skipped_with_reasons.push((info.clone(), reasons));
        }
    }

    let selected_infos_map = selected_infos
        .iter()
        .map(|si| (si.get_uid().to_string(), si.clone()))
        .collect::<HashMap<String, T>>();

    println!(
        "SlotInfo completeness filter: total={}, selected={}, skipped={}",
        all_infos.len(),
        selected_infos.len(),
        skipped_with_reasons.len()
    );

    (all_infos, selected_infos, selected_infos_map, skipped_with_reasons)
}
