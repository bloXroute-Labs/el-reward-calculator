
use csv::WriterBuilder;
use std::io::Write;
use crate::{ CommitBoostSlotInfos, SlotInfos, };
use crate::log_source::types::{SlotInfo,Bid, SlotInfoWithoutBids, SlotTrait};
use serde_json::{self};
use std::fs::File;
use log::debug;
use  std::io::Result as IoResult;
use url::Url;

pub fn is_relay_proxy(relay: &str) -> bool {
    relay.contains("relay-proxy") || relay.contains("Relay Proxy") || relay.contains("rproxy") || relay.contains("rpoxy") // handle typo
}


pub fn write_csv(slot_infos: &SlotInfos, folder_path: &str, date_str: &str, time_str: &str) -> IoResult<()> {
    let file_path = format!("{}/slot_infos_{}_{}.csv", folder_path, date_str, time_str);
    let file = File::create(&file_path)?;
    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(file);

    // For each slot, select one record per rules.
    for (slot, slot_info_with_uid) in slot_infos {
        debug!("Slot: {}, records: {:?}", slot, slot_info_with_uid);

        // Choose the record as follows:
        // If any record has is_proxy_win true, select the one with highest el_reward_increase_eth.
        // Otherwise, choose the first record.
        let chosen = if let Some(proxy_wins) = {
            let wins: Vec<&SlotInfo> = slot_info_with_uid.values().filter(|si| si.is_proxy_win).collect();
            if !wins.is_empty() { Some(wins) } else { None }
        } {
            proxy_wins.into_iter().max_by(|a, b| {
                a.el_reward_increase_eth
                    .partial_cmp(&b.el_reward_increase_eth)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        } else {
            slot_info_with_uid.values().next()
        };

        if let Some(slot_info) = chosen {
            let record = SlotInfoWithoutBids {
                slot_uid: &slot_info.slot_uid,
                slot: &slot_info.slot,
                block_number: &slot_info.block_number,
                header_start_ms_into_slot: slot_info.info.header_start_ms_into_slot,
                payload_start_ms_into_slot: slot_info.info.payload_start_ms_into_slot,
                block_hash: &slot_info.info.block_hash,
                is_proxy_win: slot_info.is_proxy_win,
                is_winning_bid_highest: slot_info.is_winning_bid_highest,
                el_reward_increase_eth: slot_info.el_reward_increase_eth,
                el_reward_increase_wei: slot_info.el_reward_increase_wei.clone(),
                onchain_bid_value: slot_info.onchain_bid_value,
                onchain_bid_delivered_relay: slot_info.onchain_bid_delivered_relay.clone(),
                second_highest_bid_value: slot_info.second_highest_bid_value,
                second_higher_bid_delivered_relay: slot_info.second_higher_bid_delivered_relay.clone(),
                is_payload_received: slot_info.is_payload_received,
                el_reward_increase_percentage: slot_info.el_reward_increase_percentage,
                el_reward_increase_percent_precise: slot_info.el_reward_increase_percent_precise,
                equal_to_proxy_bidders: slot_info.equal_to_proxy_bidders.clone(),
                is_equal_to_proxy_bid: slot_info.is_equal_to_proxy_bid,
                fee_per_block: slot_info.fee_per_block,
            };
            wtr.serialize(record)?;
        }
    }
    wtr.flush()?;
    Ok(())
}

pub fn write_json(slot_infos: &SlotInfos, folder_path: &str, date_str: &str, time_str: &str) -> IoResult<()> {
    let file_path = format!("{}/slot_infos_{}_{}.json", folder_path, date_str, time_str);
    let mut file = File::create(&file_path)?;
    let json_data = serde_json::to_string_pretty(&slot_infos)?;
    file.write_all(json_data.as_bytes())?;
    Ok(())
}


pub fn write_csv_commitboost(slot_infos: &CommitBoostSlotInfos, folder_path: &str, date_str: &str, time_str: &str) -> IoResult<()> {
    let file_path = format!("{}/slot_infos_{}_{}.csv", folder_path, date_str, time_str);
    let file = File::create(&file_path)?;
    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(file);

    for (_slot, slot_info_with_uid) in slot_infos {
        let chosen = slot_info_with_uid.values().filter(|si| si.is_proxy_win).max_by(|a, b| a.el_reward_increase_eth.partial_cmp(&b.el_reward_increase_eth).unwrap_or(std::cmp::Ordering::Equal))
            .or_else(|| slot_info_with_uid.values().next());

        if let Some(slot_info) = chosen {
            let record = SlotInfoWithoutBids {
                slot_uid: slot_info.get_uid(),
                slot: slot_info.get_slot(),
                block_number: slot_info.get_block_number(),
                header_start_ms_into_slot: slot_info.get_header_start(),
                payload_start_ms_into_slot: slot_info.get_payload_start(),
                block_hash: slot_info.get_block_hash(),
                is_proxy_win: slot_info.is_proxy_win(),
                is_winning_bid_highest: slot_info.is_winning_bid_highest(),
                el_reward_increase_eth: slot_info.get_el_reward_eth(),
                el_reward_increase_wei: slot_info.get_el_reward_wei(),
                onchain_bid_value: slot_info.get_onchain_bid_value(),
                onchain_bid_delivered_relay: slot_info.get_onchain_bid_delivered_relay().to_string(),
                second_highest_bid_value: slot_info.get_second_highest_bid_value(),
                second_higher_bid_delivered_relay: slot_info.get_second_higher_bid_delivered_relay().to_string(),
                is_payload_received: slot_info.is_payload_received(),
                el_reward_increase_percentage: slot_info.get_el_reward_percentage(),
                el_reward_increase_percent_precise: slot_info.get_el_reward_precise(),
                equal_to_proxy_bidders: slot_info.get_equal_to_proxy_bidders().to_string(),
                is_equal_to_proxy_bid: slot_info.is_equal_to_proxy_bid(),
                fee_per_block: slot_info.get_fee_per_block(),
            };
            wtr.serialize(record)?;
        }
    }
    wtr.flush()?;
    Ok(())
}


pub fn write_json_commitboost(slot_infos: &CommitBoostSlotInfos, folder_path: &str, date_str: &str, time_str: &str) -> IoResult<()> {
    let file_path = format!("{}/slot_infos_{}_{}.json", folder_path, date_str, time_str);
    let mut file = File::create(&file_path)?;
    let json_data = serde_json::to_string_pretty(&slot_infos)?;
    file.write_all(json_data.as_bytes())?;
    Ok(())
}

pub fn write_summary_report_commitboost(slot_infos: &CommitBoostSlotInfos, folder_path: &str, date_str: &str, time_str: &str) -> std::io::Result<()> {
    let mut total_slots = 0;
    let mut slots_won_by_rproxy = 0;
    let mut total_eth = 0.0f64;
    let mut reward_improvement_eth = 0.0f64;

    for slot_info_with_uid in slot_infos.values() {
        for slot_info in slot_info_with_uid.values() {
            total_slots += 1;

            if slot_info.is_proxy_win {
                slots_won_by_rproxy += 1;
                total_eth += slot_info.onchain_bid_value;
                reward_improvement_eth += slot_info.el_reward_increase_eth;
            }
        }
    }

    let improvement_percentage = if total_eth > 0.0 {
        (reward_improvement_eth / total_eth) * 100.0
    } else {
        0.0
    };

    let summary_path = format!("{}/summary_{}_{}.txt", folder_path, date_str, time_str);
    let mut summary_file = std::fs::File::create(&summary_path)?;

    use std::io::Write;
    writeln!(summary_file, "Total Slots           : {}", total_slots)?;
    writeln!(summary_file, "Slots won by Rproxy   : {}", slots_won_by_rproxy)?;
    writeln!(summary_file, "total                 : {:.18} ETH", total_eth)?;
    writeln!(summary_file, "EL reward improvement : {:.18} ETH", reward_improvement_eth)?;
    writeln!(
        summary_file,
        "Improvement percentage: ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth,
        improvement_percentage
    )?;

    // log to terminal
    println!("Total Slots           : {}", total_slots);
    println!("Slots won by Rproxy   : {}", slots_won_by_rproxy);
    println!("total                 : {:.18} ETH", total_eth);
    println!("EL reward improvement : {:.18} ETH", reward_improvement_eth);
    println!(
        "Improvement percentage: ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth,
        improvement_percentage
    );

    Ok(())
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
pub fn write_summary_report(slot_infos: &SlotInfos, folder_path: &str, date_str: &str, time_str: &str) -> std::io::Result<()> {
    let mut total_slots = 0;
    let mut slots_won_by_rproxy = 0;
    let mut total_eth = 0.0f64;
    let mut reward_improvement_eth = 0.0f64;

    for slot_info_with_uid in slot_infos.values() {
        for slot_info in slot_info_with_uid.values() {
            total_slots += 1;

            if slot_info.is_proxy_win {
                slots_won_by_rproxy += 1;
                total_eth += slot_info.onchain_bid_value;
                reward_improvement_eth += slot_info.el_reward_increase_eth;
            }
        }
    }

    let improvement_percentage = if total_eth > 0.0 {
        (reward_improvement_eth / total_eth) * 100.0
    } else {
        0.0
    };

    let summary_path = format!("{}/summary_{}_{}.txt", folder_path, date_str, time_str);
    let mut summary_file = std::fs::File::create(&summary_path)?;

    use std::io::Write;
    writeln!(summary_file, "Total Slots           : {}", total_slots)?;
    writeln!(summary_file, "Slots won by Rproxy   : {}", slots_won_by_rproxy)?;
    writeln!(summary_file, "total                 : {:.18} ETH", total_eth)?;
    writeln!(summary_file, "EL reward improvement : {:.18} ETH", reward_improvement_eth)?;
    writeln!(
        summary_file,
        "Improvement percentage: ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth,
        improvement_percentage
    )?;

    // log to terminal
    println!("Total Slots           : {}", total_slots);
    println!("Slots won by Rproxy   : {}", slots_won_by_rproxy);
    println!("total                 : {:.18} ETH", total_eth);
    println!("EL reward improvement : {:.18} ETH", reward_improvement_eth);
    println!(
        "Improvement percentage: ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth,
        improvement_percentage
    );

    Ok(())
}
