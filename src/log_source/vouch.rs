use crate::log_source::types::{LogEntryVouch,SlotInfo };
use crate::SlotInfos;
use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use crate::Bid;
use log::debug;
use ethers::types::U256;
use ethers::utils::parse_ether;
use ethers::utils::format_units;
use crate::log_source::common::{is_relay_proxy, parse_url};
use url::Url;
use serde::Serialize;

#[derive(Serialize)]
struct RemovedSlotInfo {
    slot: String,
    pod_name: String,
}

pub fn parse_file_content<R: std::io::Read>(reader : R , slot_infos :&mut SlotInfos){
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<LogEntryVouch>(Value::Object(map)) {
                    Ok(log_entry) => {
                        // Process the valid log entry
                        process_json_first_pass(&log_entry, slot_infos);
                        // process_slots(slot_infos);
                    }
                    Err(e) => {
                        eprintln!("Failed to parse log entry: {}. Skipping.", e);
                    }
                }
            }
            Ok(Value::Null) => {
                eprintln!("Encountered Null value. Skipping.");
            }
            Ok(Value::Bool(_)) => {
                eprintln!("Encountered Boolean value. Skipping.");
            }
            Ok(Value::Number(_)) => {
                eprintln!("Encountered Number value. Skipping.");
            }
            Ok(Value::String(_)) => {
                eprintln!("Encountered String value. Skipping.");
            }
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<LogEntryVouch>(item) {
                        Ok(log_entry) => {
                            // Process the valid log entry
                            process_json_first_pass(&log_entry, slot_infos);
                        }
                        Err(e) => {
                            eprintln!("Failed to parse log entry: {}. Skipping.", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to parse JSON entry: {}. Skipping.", e);
                // Optionally, log the error or take other actions
            }
        }
    }
    calculate_uplift_second_pass_and_store_removed(slot_infos, "./removed_slots.json")
}
fn process_json_first_pass(log_entry: &LogEntryVouch, slot_infos: &mut SlotInfos) {
    let slot = log_entry.slot.to_string();
    let slot_uid = slot.clone(); // no req_id in vouch logs, so use slot as slot_uid
    debug!("First pass - collecting: slot={}, slot_uid={}", slot, slot_uid);

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);
    let slot_info = slot_info_map.entry(slot_uid.clone()).or_insert_with(|| {
        debug!("Creating new SlotInfo for slot_uid: {}", slot_uid);
        SlotInfo::new_with_slot_uid_and_slot(slot_uid.clone(), slot.clone())
    });

    let date = DateTime::parse_from_rfc3339(&log_entry.time)
        .expect(&format!(
            "failed to parse timestamp for slot-{}, timestamp-{}",
            slot, log_entry.time
        ))
        .with_timezone(&Utc);

    let mut bid: Bid = Default::default();
    bid.timestamp = date.timestamp();
    bid.slot = slot.clone();

    bid.relay = Url::parse(&log_entry.provider)
        .ok()
        .and_then(|u| u.domain().map(str::to_string))
        .unwrap_or_else(|| log_entry.provider.clone());

    let wei = U256::from_dec_str(&log_entry.score).unwrap_or(U256::zero());
    bid.bid_value = format_units(wei, "ether")
        .unwrap_or_else(|_| "0.0".into())
        .parse::<f64>()
        .unwrap_or(0.0);

    debug!("Collected bid: slot={}, relay={}, value={}", slot, bid.relay, bid.bid_value);

    slot_info.info.bids.push(bid.clone());

    if log_entry.selected && is_relay_proxy(&bid.relay) {
        slot_info.is_proxy_win = true;
        slot_info.is_payload_received = true;
        slot_info.onchain_bid_value = bid.bid_value;
        slot_info.onchain_bid_delivered_relay = parse_url(&bid);
        slot_info.block_number = "".to_string(); // Not available in vouch logs
        slot_info.info.block_hash = "".to_string(); // Not available in vouch logs
    }
}

pub fn calculate_uplift_second_pass_and_store_removed(
    slot_infos: &mut SlotInfos,
    removed_file_path: &str,
) {
    let mut removed_slots: Vec<RemovedSlotInfo> = Vec::new();
    let mut slots_to_remove = Vec::new();

    for (slot, slot_info_map) in slot_infos.iter_mut() {
        let mut slot_uids_to_remove = vec![];

        for (slot_uid, slot_info) in slot_info_map.iter() {
            let has_proxy_bid = slot_info
                .info
                .bids
                .iter()
                .any(|b| is_relay_proxy(&b.relay));

            if !has_proxy_bid {
                removed_slots.push(RemovedSlotInfo {
                    slot: slot.clone(),
                    pod_name:String::new(),
                });
                slot_uids_to_remove.push(slot_uid.clone());
            }
        }

        for uid in slot_uids_to_remove {
            slot_info_map.remove(&uid);
        }

        if slot_info_map.is_empty() {
            slots_to_remove.push(slot.clone());
        }
    }

    for slot in slots_to_remove {
        slot_infos.remove(&slot);
    }

    // Now compute uplift for remaining proxy-winning slots
    for (_slot, slot_info_map) in slot_infos.iter_mut() {
        for (_slot_uid, slot_info) in slot_info_map.iter_mut() {
            if slot_info.is_proxy_win && slot_info.onchain_bid_value > 0.0 {
                slot_info.info.bids.sort_by(|a, b| b.bid_value.total_cmp(&a.bid_value));

                let highest_bid = slot_info.info.bids.first();
                if let Some(highest) = highest_bid {
                    slot_info.is_winning_bid_highest =
                        slot_info.onchain_bid_value == highest.bid_value;

                    let second_best_bid = slot_info
                        .info
                        .bids
                        .iter()
                        .skip(1)
                        .find(|b| !is_relay_proxy(&b.relay));

                    slot_info.second_highest_bid_value =
                        second_best_bid.map_or(0.0, |b| b.bid_value);
                    slot_info.second_higher_bid_delivered_relay =
                        second_best_bid.map_or_else(String::new, parse_url);

                    if slot_info.second_highest_bid_value > 0.0 {
                        let el_reward_increase =
                            slot_info.onchain_bid_value - slot_info.second_highest_bid_value;

                        let el_reward_increase_wei =
                            parse_ether(&el_reward_increase.to_string()).unwrap_or(U256::zero());
                        let el_reward_increase_eth = format_units(el_reward_increase_wei, "ether")
                            .unwrap_or_else(|_| "0.0".into())
                            .parse::<f64>()
                            .unwrap_or(0.0);

                        let el_reward_increase_percent_precise =
                            (el_reward_increase / slot_info.onchain_bid_value) * 100.0;
                        let el_reward_increase_percentage =
                            el_reward_increase_percent_precise.round() as u64;

                        slot_info.el_reward_increase_wei = el_reward_increase_wei;
                        slot_info.el_reward_increase_eth =  el_reward_increase_eth;
                        slot_info.el_reward_increase_percentage = el_reward_increase_percentage;
                        slot_info.el_reward_increase_percent_precise =
                            el_reward_increase_percent_precise;
                    }
                }
            }
        }
    }

    // Write removed slots to file
    // if let Ok(json) = serde_json::to_string_pretty(&removed_slots) {
    //     let mut file =
    //         File::create(removed_file_path).expect("Unable to create removed_slots.json file");
    //     file.write_all(json.as_bytes())
    //         .expect("Failed to write removed slot info to file");
    //     debug!("Removed slot info written to {}", removed_file_path);
    // } else {
    //     eprintln!("Failed to serialize removed slot info.");
    // }
}
