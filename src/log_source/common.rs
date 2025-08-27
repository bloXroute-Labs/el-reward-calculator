use crate::log_source::types::Bid;
use url::Url;
use rust_decimal::Decimal;
use std::{collections::{HashMap, HashSet}};
use crate::log_source::stats_writer::RewardStats;
// add/replace these imports near the top
use chrono::{DateTime, Utc, TimeZone };


// Ethereum mainnet beacon genesis & slot timing
const BEACON_GENESIS_TIME: i64 = 1_606_824_023; // 2020-12-01T12:00:23Z
const SECONDS_PER_SLOT: i64 = 12;

pub fn is_relay_proxy(relay: &str) -> bool {
    let relay_lower = relay.to_lowercase();
    let patterns = [
        "relay-proxy",
        "relay proxy",
        "proxy",
        "rproxy",
        "rpoxy", // handle typo
        "18.156.4.232",
        "3.79.24.65",
        "3.216.87.59",
        "54.82.110.47",
    ];

    patterns.iter().any(|p| relay_lower.contains(p))
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
/// - all_infos_map: UID -> SlotInfo for all entries
/// - selected_infos: Vec of SlotInfo passing validation
/// - selected_infos_map: UID -> SlotInfo passing validation
/// - skipped_slots_by_slot: Slot -> Vec<(SlotUID, SlotInfo, Reasons)>
pub fn filter_valid_slot_infos<T: RewardStats + Clone + std::fmt::Debug>(
    slot_infos: &HashMap<String, HashMap<String, T>>,
    logsource: &str,
) -> (
    HashMap<String, HashMap<String, T>>,                // all_infos_map
    Vec<T>,                                             // selected_infos
    HashMap<String, T>,                                 // selected_infos_map
    HashMap<String, Vec<(String, T, Vec<&'static str>)>> // skipped_slots_by_slot
) {
    let mut all_infos_map: HashMap<String, HashMap<String, T>> = HashMap::new();
    let mut selected_infos = Vec::new();
    let mut selected_uid_set: HashSet<String> = HashSet::new();
    let mut skipped_by_slot: HashMap<String, Vec<(String, T, Vec<&'static str>)>> = HashMap::new();

    for (slot, slot_map) in slot_infos {
        let mut all_slot_uids_skipped = true;
        all_infos_map.insert(slot.clone(), slot_map.clone());  // <-- keep full structure

        for (slot_uid, info) in slot_map {
            let mut reasons = Vec::new();
            if logsource != "vouch" {
                if info.get_uid().is_empty() {
                    reasons.push("UID empty");
                }
                if info.get_block_hash().is_empty() {
                    reasons.push("BlockHash empty");
                }
                if info.get_onchain_bid_value() <= Decimal::ZERO {
                    reasons.push("Bid is zero or negative");
                }
            }

            if reasons.is_empty() {
                selected_infos.push(info.clone());
                selected_uid_set.insert(slot_uid.clone());
                all_slot_uids_skipped = false;
            } else {
                skipped_by_slot
                    .entry(slot.clone())
                    .or_default()
                    .push((slot_uid.clone(), info.clone(), reasons));
            }
        }

        if !all_slot_uids_skipped {
            skipped_by_slot.remove(slot);
        }
    }

    let selected_infos_map = selected_infos
        .iter()
        .map(|si| (si.get_uid().to_string(), si.clone()))
        .collect::<HashMap<String, T>>();

    let total_slot_count = slot_infos.len();
    let total_slot_uid_count = slot_infos.values().map(|m| m.len()).sum::<usize>();
    let selected_slot_uid_count = selected_infos.len();
    let skipped_slot_count = skipped_by_slot.len();
    let skipped_uid_count = skipped_by_slot.values().map(|v| v.len()).sum::<usize>();

    println!(
        "SlotInfo completeness filter: total_slots={}, all_infos_map.len() = {}, total_slot_uids={}, selected_slot_uids={}, skipped_slots={}, skipped_uids={}",
        total_slot_count,
        all_infos_map.len(),
        total_slot_uid_count,
        selected_slot_uid_count,
        skipped_slot_count,
        skipped_uid_count
    );

    (all_infos_map, selected_infos, selected_infos_map, skipped_by_slot)
}


///   return time.Unix(beaconGenesisTime + (slot * secondsPerSlot), 0).UTC()
#[inline]
pub fn get_slot_start_time_utc(
    slot: i64,
) -> DateTime<Utc> {
    Utc
        .timestamp_opt(BEACON_GENESIS_TIME + (slot * SECONDS_PER_SLOT), 0)
        .single()
        .unwrap_or_else(|| crate::Utc.timestamp_opt(0, 0).single().unwrap())
}
