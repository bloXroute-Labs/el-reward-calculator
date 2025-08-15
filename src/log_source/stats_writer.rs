use csv::WriterBuilder;
use crate::SlotInfo;
use crate::log_source::types::{CommitBoostSlotInfo, SlotInfoWithoutBids};
use log::debug;
use ethers::types::U256;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{Write, Result as IoResult};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{self, to_string_pretty};

/// Common stats surface for both MEV-Boost and Commit-Boost slot info
pub trait RewardStats: Clone {
    fn get_uid(&self) -> &str;
    fn get_slot(&self) -> &str;
    fn get_block_number(&self) -> &str;
    fn get_block_hash(&self) -> &str;
    fn get_header_start(&self) -> i64;
    fn get_payload_start(&self) -> i64;
    fn get_onchain_bid_value(&self) -> Decimal;
    fn get_el_reward_eth(&self) -> Decimal;
    fn get_el_reward_wei(&self) -> U256;
    fn get_is_proxy_win(&self) -> bool;
    fn get_is_winning_bid_highest(&self) -> bool;
    fn get_second_highest_bid_value(&self) -> Decimal;
    fn get_second_higher_bid_delivered_relay(&self) -> &str;
    fn get_onchain_bid_delivered_relay(&self) -> &str;
    fn get_is_payload_received(&self) -> bool;
    fn get_el_reward_percentage(&self) -> u64;
    fn get_el_reward_precise(&self) -> Decimal;
    fn get_equal_to_proxy_bidders(&self) -> &str;
    fn is_equal_to_proxy_bid(&self) -> bool;
    fn get_fee_per_block(&self) -> Decimal;
}

impl RewardStats for SlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_block_hash(&self) -> &str { &self.info.block_hash }
    fn get_header_start(&self) -> i64 { self.info.header_start_ms_into_slot }
    fn get_payload_start(&self) -> i64 { self.info.payload_start_ms_into_slot }
    fn get_onchain_bid_value(&self) -> Decimal { self.onchain_bid_value }
    fn get_el_reward_eth(&self) -> Decimal { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn get_is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_second_highest_bid_value(&self) -> Decimal { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> Decimal { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> Decimal { self.fee_per_block }
}

impl RewardStats for CommitBoostSlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_block_hash(&self) -> &str { &self.block_hash }
    fn get_header_start(&self) -> i64 { 0 }
    fn get_payload_start(&self) -> i64 { 0 }
    fn get_onchain_bid_value(&self) -> Decimal { self.onchain_bid_value }
    fn get_el_reward_eth(&self) -> Decimal { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn get_is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_second_highest_bid_value(&self) -> Decimal { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> Decimal { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> Decimal { self.fee_per_block }
}

/// From a map of {slot -> {slot_uid -> T}}, select the *one* T per slot to keep.
/// Policy: prefer proxy wins with positive uplift; among ties, pick max uplift; tie-break by uid.
pub fn select_final_slot_infos_generic<T>(
    slot_infos: &HashMap<String, HashMap<String, T>>,
) -> HashMap<String, T>
where
    T: RewardStats + Clone + Debug,
{
    let mut final_selected: HashMap<String, T> = HashMap::new();

    for (slot, slot_info_with_uid) in slot_infos {
        let proxy_wins: Vec<_> = slot_info_with_uid
            .values()
            .filter(|si| {
                let value = si.get_el_reward_eth();
                let valid = si.get_is_proxy_win() && value > Decimal::ZERO;
                if si.get_is_proxy_win() {
                    debug!(
                        "[SELECT] slot_uid={} is_proxy_win=true el_reward_increase_eth={} valid={}",
                        si.get_uid(),
                        si.get_el_reward_eth(),
                        valid
                    );
                }
                valid
            })
            .collect();

        let chosen = if !proxy_wins.is_empty() {
            let selected = proxy_wins
                .into_iter()
                .max_by(|a, b| {
                    let cmp = a
                        .get_el_reward_eth()
                        .partial_cmp(&b.get_el_reward_eth())
                        .unwrap_or(Ordering::Equal);
                    if cmp == Ordering::Equal {
                        debug!(
                            "[TIE] Equal el_reward_increase_eth: {} == {}, tie-breaking with slot_uid",
                            a.get_el_reward_eth(),
                            b.get_el_reward_eth()
                        );
                        a.get_uid().cmp(b.get_uid())
                    } else {
                        cmp
                    }
                })
                .unwrap();

            debug!(
                "[CHOSEN] Selected proxy win slot_uid={}, el_reward_increase_eth={}, is_equal_to_proxy_bid={}",
                selected.get_uid(),
                selected.get_el_reward_eth(),
                false // expose via trait if needed
            );

            Some(selected)
        } else {
            let fallback = slot_info_with_uid.values().next();
            if let Some(f) = fallback {
                debug!(
                    "[CHOSEN] No proxy win, defaulting to first slot_uid={} with block_hash={}",
                    f.get_uid(),
                    f.get_block_hash()
                );
            } else {
                debug!("[CHOSEN] No slot_info entries found at all.");
            }
            fallback
        };

        if let Some(slot_info) = chosen {
            final_selected.insert(slot.clone(), slot_info.clone());
        }
    }

    final_selected
}

/// NEW: Collapse any flat map keyed by slot_uid into one record per *slot*.
/// Same policy as above: prefer proxy win with max uplift, tie by uid.
pub fn coalesce_by_slot_generic<T>(
    selected_infos: &HashMap<String, T>,
) -> HashMap<String, T>
where
    T: RewardStats + Clone + Debug,
{
    let mut per_slot: HashMap<String, T> = HashMap::new();

    for info in selected_infos.values() {
        let slot_key = info.get_slot().to_string();

        per_slot
            .entry(slot_key)
            .and_modify(|existing| {
                let existing_is_valid = existing.get_is_proxy_win() && existing.get_el_reward_eth() > Decimal::ZERO;
                let incoming_is_valid = info.get_is_proxy_win() && info.get_el_reward_eth() > Decimal::ZERO;

                let take_incoming = match (existing_is_valid, incoming_is_valid) {
                    (false, true) => true,
                    (true, false) => false,
                    (true, true) => match info.get_el_reward_eth().partial_cmp(&existing.get_el_reward_eth()).unwrap_or(Ordering::Equal) {
                        Ordering::Greater => true,
                        Ordering::Equal => info.get_uid() < existing.get_uid(),
                        Ordering::Less => false,
                    },
                    (false, false) => false,
                };

                if take_incoming {
                    *existing = info.clone();
                }
            })
            .or_insert_with(|| info.clone());
    }

    per_slot
}

/// CSV writer (as-is): writes whatever map you pass (slot or slot_uid keyed)
pub fn write_csv_generic<T: RewardStats>(
    slot_infos: &HashMap<String, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
) -> IoResult<()> {
    let file_path = format!("{}/slot_infos_{}_{}.csv", folder_path, date_str, time_str);
    let file = File::create(&file_path)?;
    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(file);

    for (_slot_key, slot_info) in slot_infos {
        let record = SlotInfoWithoutBids {
            slot_uid: slot_info.get_uid(),
            slot: slot_info.get_slot(),
            block_number: slot_info.get_block_number(),
            block_hash: slot_info.get_block_hash(),
            header_start_ms_into_slot: slot_info.get_header_start(),
            payload_start_ms_into_slot: slot_info.get_payload_start(),
            onchain_bid_value: slot_info.get_onchain_bid_value(),
            el_reward_increase_eth: slot_info.get_el_reward_eth(),
            el_reward_increase_wei: slot_info.get_el_reward_wei(),
            is_proxy_win: slot_info.get_is_proxy_win(),
            is_winning_bid_highest: slot_info.get_is_winning_bid_highest(),
            second_highest_bid_value: slot_info.get_second_highest_bid_value(),
            second_higher_bid_delivered_relay: slot_info.get_second_higher_bid_delivered_relay().to_string(),
            onchain_bid_delivered_relay: slot_info.get_onchain_bid_delivered_relay().to_string(),
            is_payload_received: slot_info.get_is_payload_received(),
            el_reward_increase_percentage: slot_info.get_el_reward_percentage(),
            el_reward_increase_percent_precise: slot_info.get_el_reward_precise(),
            equal_to_proxy_bidders: slot_info.get_equal_to_proxy_bidders().to_string(),
            is_equal_to_proxy_bid: slot_info.is_equal_to_proxy_bid(),
            fee_per_block: slot_info.get_fee_per_block(),
        };
        wtr.serialize(record)?;
    }

    wtr.flush()?;
    Ok(())
}

/// OPTIONAL: CSV writer that guarantees one row per *slot*
pub fn write_csv_per_slot_generic<T: RewardStats + Clone + Debug>(
    selected_infos: &HashMap<String, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
) -> IoResult<()> {
    let per_slot = coalesce_by_slot_generic(selected_infos);
    write_csv_generic(&per_slot, folder_path, date_str, time_str)
}

pub fn write_json_generic<K, T>(
    slot_infos: &HashMap<K, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
) -> IoResult<()>
where
    K: std::fmt::Display + Eq + std::hash::Hash + Serialize,
    T: Serialize,
{
    let file_path = format!("{}/slot_infos_{}_{}.json", folder_path, date_str, time_str);
    let mut file = File::create(&file_path)?;
    let json_data = serde_json::to_string_pretty(&slot_infos)?;
    file.write_all(json_data.as_bytes())?;
    Ok(())
}

/// Writes the summary and logs skipped infos to JSON.
/// **Fix:** collapse to *one* record per slot before computing totals.
pub fn write_summary_generic<T: RewardStats + std::fmt::Debug + Serialize>(
    selected_infos: &HashMap<String, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
    _all_infos: &[T],
    skipped_infos:  &HashMap<String, Vec<(String, T, Vec<&'static str>)>>,
) -> std::io::Result<()> {
    use std::io::Write;

    // Collapse to one record per slot (handles MEV-Boost JSON case where input is slot_uid keyed)
    let per_slot = coalesce_by_slot_generic(selected_infos);

    // For visibility: show both counts
    let total_slot_uids = selected_infos.len();
    let total_slots = per_slot.len();

    let mut slots_won_by_rproxy = 0usize;
    let mut total_eth_overall = Decimal::ZERO;
    let mut total_eth_rproxy = Decimal::ZERO;
    let mut reward_improvement_eth = Decimal::ZERO;
    let mut total_fee_per_block_eth = Decimal::ZERO;

    println!("Total slot_infos parsed_before (slot_uids): {}", total_slot_uids);

    let mut sorted_infos: Vec<_> = per_slot.values().collect();
    // Stable sort by uid (or change to .get_slot() if you prefer)
    sorted_infos.sort_by(|a, b| a.get_uid().cmp(&b.get_uid()));

    // Checksum across the selected per-slot entries
    let checksum: Decimal = sorted_infos.iter().map(|i| i.get_onchain_bid_value()).sum();
    println!("Total ETH Checksum: {:.18}", checksum);

    for info in sorted_infos {
        if info.get_onchain_bid_value() > Decimal::ZERO {
            total_eth_overall += info.get_onchain_bid_value();
        }

        if info.get_is_proxy_win() {
            slots_won_by_rproxy += 1;
            total_eth_rproxy += info.get_onchain_bid_value();
            reward_improvement_eth += info.get_el_reward_eth();
            total_fee_per_block_eth += info.get_fee_per_block();
        }
    }

    let improvement_percentage = if total_eth_rproxy > Decimal::ZERO {
        (reward_improvement_eth / total_eth_rproxy) * dec!(100.0)
    } else {
        Decimal::ZERO
    };

    let owed_to_blxr = reward_improvement_eth * dec!(0.5);

    let summary_path = format!("{}/summary_{}_{}.txt", folder_path, date_str, time_str);
    let mut file = File::create(&summary_path)?;

    // Summary (prints both counts; metrics are per-slot)
    writeln!(file, "Total Slot UIDs        : {}", total_slot_uids)?;
    writeln!(file, "Total Slots            : {}", total_slots)?;
    writeln!(file, "total eth overall      : {:.18} ETH", total_eth_overall)?;
    writeln!(file, "Slots won by Rproxy    : {}", slots_won_by_rproxy)?;
    writeln!(file, "total eth (Rproxy slots): {:.18} ETH", total_eth_rproxy)?;
    writeln!(file, "EL reward improvement  : {:.18} ETH", reward_improvement_eth)?;
    writeln!(
        file,
        "Improvement percentage : ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth_rproxy,
        improvement_percentage
    )?;
    writeln!(file, "50% Owed to BLXR       : {:.18} ETH", owed_to_blxr)?;
    writeln!(file, "Total fee per block    : {:.18} ETH", total_fee_per_block_eth)?;

    println!("Total Slot UIDs           : {}", total_slot_uids);
    println!("Total Slots               : {}", total_slots);
    println!("total eth overall         : {:.18} ETH", total_eth_overall);
    println!("Slots won by Rproxy       : {}", slots_won_by_rproxy);
    println!("total eth (Rproxy slots)  : {:.18} ETH", total_eth_rproxy);
    println!("EL reward improvement     : {:.18} ETH", reward_improvement_eth);
    println!(
        "Improvement percentage    : ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth, total_eth_rproxy, improvement_percentage
    );
    println!("50% Owed to BLXR         : {:.18} ETH", owed_to_blxr);
    println!("Total fee per block       : {:.18} ETH", total_fee_per_block_eth);

    // Skipped infos
    let skipped_dir = format!("{}/skipped", folder_path);
    fs::create_dir_all(&skipped_dir)?;
    let skipped_path = format!("{}/skipped_out_{}_{}.json", skipped_dir, date_str, time_str);
    let mut skipped_file = File::create(&skipped_path)?;
    log_skipped_infos(skipped_infos, &mut skipped_file)?;

    Ok(())
}

/// Logs skipped slot infos fully in JSON format
pub fn log_skipped_infos<T: std::fmt::Debug + Serialize>(
    skipped_infos: &HashMap<String, Vec<(String, T, Vec<&'static str>)>>,
    file: &mut File,
) -> std::io::Result<()> {
    for (slot, entries) in skipped_infos {
        for (slot_uid, info, reasons) in entries {
            let json_entry = serde_json::json!({
                "slot": slot,
                "slot_uid": slot_uid,
                "reasons": reasons,
                "info": info
            });
            writeln!(file, "{}", to_string_pretty(&json_entry).unwrap())?;
        }
    }
    Ok(())
}
