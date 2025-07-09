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




/// Filters valid slot infos based on completeness and returns:
/// - all flattened slotInfos
/// - selected slotInfos
/// - selected UID->SlotInfo map
/// - skipped slots grouped by slot number (if all SlotUIDs are skipped for that slot)
pub fn filter_valid_slot_infos<T: RewardStats + Clone + std::fmt::Debug>(
    slot_infos: &HashMap<String, HashMap<String, T>>,
) -> (
    Vec<T>, // all_infos
    Vec<T>, // selected_infos
    HashMap<String, T>, // selected_infos_map
    HashMap<String, Vec<(String, T, Vec<&'static str>)>> // skipped_slots_by_slot
) {
    let mut all_infos = Vec::new();
    let mut selected_infos = Vec::new();
    let mut selected_uid_set: HashSet<String> = HashSet::new();
    let mut skipped_by_slot: HashMap<String, Vec<(String, T, Vec<&'static str>)>> = HashMap::new();

    for (slot, slot_map) in slot_infos.iter() {
        let mut all_slot_uids_skipped = true;

        for (slot_uid, info) in slot_map.iter() {
            all_infos.push(info.clone());
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
                selected_uid_set.insert(slot_uid.clone());
                all_slot_uids_skipped = false;
            } else {
                skipped_by_slot.entry(slot.clone()).or_default().push((slot_uid.clone(), info.clone(), reasons));
            }
        }

        // If at least one was selected, remove the skipped records for this slot
        if !all_slot_uids_skipped {
            skipped_by_slot.remove(slot);
        }
    }

    let selected_infos_map = selected_infos.iter()
        .map(|si| (si.get_uid().to_string(), si.clone()))
        .collect::<HashMap<String, T>>();

    let skipped_count = skipped_by_slot.values().map(|v| v.len()).sum::<usize>();
    println!(
        "SlotInfo completeness filter: total_slots={}, selected_uids={}, skipped_slots={}, skipped_uids={}",
        slot_infos.len(),
        selected_infos.len(),
        skipped_by_slot.len(),
        skipped_count
    );

    (all_infos, selected_infos, selected_infos_map, skipped_by_slot)
}
