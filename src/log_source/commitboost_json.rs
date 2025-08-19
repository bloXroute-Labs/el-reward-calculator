
use serde::{Deserialize, Serialize};
use crate::{ CommitBoostSlotInfos};
use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, Utc};
use crate::log_source::types::{Bid,CommitBoostRequest, CommitBoostSlotInfo, SlotTrait};
use ethers::types::U256;
use crate::log_source::common::is_relay_proxy;
use log::debug;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use url::Url;
use std::collections::{BTreeSet, HashMap};
use rust_decimal_macros::dec;

pub fn parse_file_content<R: std::io::Read>(reader: R, slot_infos: &mut CommitBoostSlotInfos) {
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<CommitBoostLogEntry>(Value::Object(map)) {
                    Ok(log_entry) => {
                        process_json(&log_entry, slot_infos);
                    }
                    Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                }
            }
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<CommitBoostLogEntry>(item) {
                        Ok(log_entry) => process_json(&log_entry, slot_infos),
                        Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                    }
                }
            }
            _ => eprintln!("Unsupported JSON entry encountered. Skipping."),
        }
    }
}

fn process_json(log_entry: &CommitBoostLogEntry, slot_infos: &mut CommitBoostSlotInfos) {
    let span = &log_entry.span;
    let slot = span.slot.unwrap_or_default().to_string();
    let parent_hash = span.parent_hash.clone().unwrap_or_else(|| "unknown".to_string());
    let slot_uid = format!("{}_{}", slot, parent_hash);

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    // Ensure merging happens if slot_uid already exists
    let slot_info = slot_info_map
        .entry(slot_uid.clone())
        .and_modify(|existing| existing.merge_fields_from_log_entry(log_entry))
        .or_insert_with(|| {
            debug!("[INIT] Creating CommitBoostSlotInfo for slot_uid: {}", slot_uid);
            CommitBoostSlotInfo::from_log_entry(log_entry, slot_uid.clone(), slot.clone())
        });

    match span.method.as_str() {
        "/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey}" => {
            if log_entry.message == "received new header" {
                let req_id = span.req_id.clone().unwrap_or_else(|| "unknown_reqid".to_string());

                let mut bid: Bid = Default::default();
                if let Ok(date) = DateTime::parse_from_rfc3339(&log_entry.timestamp) {
                    bid.timestamp = date.with_timezone(&Utc).timestamp();
                }

                bid.slot = slot.clone();
                bid.block_hash = log_entry.fields.block_hash.clone().unwrap_or_default();
                bid.bid_value = log_entry
                    .fields
                    .value_eth
                    .as_deref()
                    .unwrap_or("0.0")
                    .parse::<Decimal>()
                    .unwrap_or(Decimal::ZERO);
                bid.relay = log_entry.fields.relay_id.clone().unwrap_or_default();

                slot_info
                    .requests
                    .entry(req_id.clone())
                    .or_insert_with(Default::default)
                    .bids
                    .push(bid.clone());

                // Handle resolution of earlier unmatched blinded block
                if slot_info.selected_req_id.is_none()
                    && slot_info.pending_blinded_block_hashes.contains(&bid.block_hash)
                {
                    debug!(
                        "[RESOLVE] Found pending blinded block hash {} via header; setting selected_req_id={}",
                        bid.block_hash, req_id
                    );
                    slot_info.selected_req_id = Some(req_id);
                    slot_info.block_hash = bid.block_hash.clone();
                }
            }
        }

        "/eth/v1/builder/blinded_blocks" => {
            if log_entry.message == "received unblinded block" {
                let block_hash = span.block_hash.clone().unwrap_or_default();

                let mut matched_req_ids: Vec<(&String, &CommitBoostRequest)> = slot_info
                    .requests
                    .iter()
                    .filter(|(_, req)| req.bids.iter().any(|b| b.block_hash == block_hash))
                    .collect();

                if !matched_req_ids.is_empty() {
                    matched_req_ids.sort_by(|(aid, a), (bid, b)| {
                        let a_max = a
                            .bids
                            .iter()
                            .filter(|b| b.block_hash == block_hash)
                            .map(|b| b.bid_value)
                            .fold(Decimal::ZERO, Decimal::max);

                        let b_max = b
                            .bids
                            .iter()
                            .filter(|b| b.block_hash == block_hash)
                            .map(|b| b.bid_value)
                            .fold(Decimal::ZERO, Decimal::max);

                        b_max.cmp(&a_max).then_with(|| aid.cmp(bid))
                    });

                    let (best_req_id, _) = matched_req_ids[0];

                    if slot_info.selected_req_id.is_none() || slot_info.get_block_hash() != block_hash {
                        debug!(
                            "[SUBMIT] Selected best matching req_id={} with highest bid for block_hash={}",
                            best_req_id, block_hash
                        );
                        slot_info.selected_req_id = Some(best_req_id.clone());
                        slot_info.block_hash = block_hash;
                        slot_info.block_number = format!("{}", span.block_number.unwrap_or_default());
                    }
                } else {
                    debug!(
                        "[DEFER] No matching request yet for blinded block {}; storing for later in slot_uid={}",
                        block_hash, slot_uid
                    );
                    if !slot_info.pending_blinded_block_hashes.contains(&block_hash) {
                        slot_info.pending_blinded_block_hashes.push(block_hash);
                    }
                }
            }
        }

        _ => {}
    }
}

impl CommitBoostSlotInfo {
    pub fn merge_fields_from_log_entry(&mut self, log_entry: &CommitBoostLogEntry) {
        if self.block_hash.is_empty() {
            if let Some(bh) = &log_entry.fields.block_hash {
                self.block_hash = bh.clone();
            }
        }

        if self.block_number.is_empty() {
            if let Some(num) = log_entry.span.block_number {
                if num != 0 {
                    self.block_number = num.to_string();
                }
            }
        }

        // Optional: can extend to merge more fields later if needed
    }

    pub fn from_log_entry(log_entry: &CommitBoostLogEntry, slot_uid: String, slot: String) -> Self {
        let mut info = CommitBoostSlotInfo::new(slot_uid, slot);
        info.merge_fields_from_log_entry(log_entry);
        info
    }
}



// small helper (keep near your other helpers)
fn host_from(relay: &str) -> String {
    Url::parse(relay)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| relay.to_string())
}

pub fn post_process_all_slots(slot_infos: &mut CommitBoostSlotInfos) {
    // ---------- Pass 1: resolve each UID as you already do (late match + best fallback) ----------
    let mut slots: Vec<_> = slot_infos.keys().cloned().collect();
    slots.sort();

    for slot in &slots {
        if let Some(slot_map) = slot_infos.get_mut(slot) {
            let mut slot_uids: Vec<_> = slot_map.keys().cloned().collect();
            slot_uids.sort();

            for slot_uid in slot_uids {
                if let Some(slot_info) = slot_map.get_mut(&slot_uid) {
                    // Late match using pending hashes
                    if slot_info.selected_req_id.is_none() && !slot_info.pending_blinded_block_hashes.is_empty() {
                        for blinded_block_hash in &slot_info.pending_blinded_block_hashes {
                            let mut matched: Vec<(&String, &CommitBoostRequest)> = slot_info
                                .requests
                                .iter()
                                .filter(|(_, req)| req.bids.iter().any(|b| &b.block_hash == blinded_block_hash))
                                .collect();

                            if !matched.is_empty() {
                                // sort: highest value for that hash, then req_id
                                matched.sort_by(|(aid, a), (bid, b)| {
                                    let a_max = a.bids.iter()
                                        .filter(|x| &x.block_hash == blinded_block_hash)
                                        .map(|x| x.bid_value)
                                        .fold(Decimal::ZERO, Decimal::max);
                                    let b_max = b.bids.iter()
                                        .filter(|x| &x.block_hash == blinded_block_hash)
                                        .map(|x| x.bid_value)
                                        .fold(Decimal::ZERO, Decimal::max);
                                    b_max.cmp(&a_max).then_with(|| aid.cmp(bid))
                                });

                                let (best_req_id, _) = matched[0];
                                debug!(
                                    "[FINALIZE] Late match for blinded block hash {} -> req_id {}",
                                    blinded_block_hash, best_req_id
                                );
                                slot_info.selected_req_id = Some(best_req_id.clone());
                                slot_info.block_hash = blinded_block_hash.clone();
                                break;
                            }
                        }
                    }

                    // Fallback: best bid across all requests
                    if slot_info.selected_req_id.is_none() {
                        let mut best_bid: Option<(String, String, Decimal)> = None;
                        for (req_id, req) in &slot_info.requests {
                            for bid in &req.bids {
                                if !bid.block_hash.is_empty() && bid.bid_value > Decimal::ZERO {
                                    match &best_bid {
                                        Some((_, _, cur)) if bid.bid_value <= *cur => {}
                                        _ => best_bid = Some((req_id.clone(), bid.block_hash.clone(), bid.bid_value)),
                                    }
                                }
                            }
                        }
                        if let Some((best_req_id, block_hash, _)) = best_bid {
                            debug!("[AUTO-MATCH] Selected best bid req_id={} block_hash={} (fallback)", best_req_id, block_hash);
                            slot_info.selected_req_id = Some(best_req_id);
                            slot_info.block_hash = block_hash;
                        }
                    }
                }
            }
        }
    }

    // ---------- Pass 2: per-slot reconciliation with your rules ----------
    for slot in slots {
        let Some(slot_map) = slot_infos.get_mut(&slot) else { continue; };

        // Gather ALL bids across ALL UIDs/requests in this slot
        #[derive(Clone)]
        struct BidView<'a> {
            relay: &'a str,
            host: String,
            block_hash: &'a str,
            value: Decimal,
        }

        let mut all_bids: Vec<BidView> = Vec::new();
        for (_uid, info) in slot_map.iter() {
            for (_rid, req) in &info.requests {
                for b in &req.bids {
                    if b.block_hash.is_empty() { continue; }
                    if b.bid_value <= Decimal::ZERO { continue; }
                    all_bids.push(BidView {
                        relay: &b.relay,
                        host: host_from(&b.relay),
                        block_hash: &b.block_hash,
                        value: b.bid_value,
                    });
                }
            }
        }

        if all_bids.is_empty() {
            // Nothing to compute for this slot
            for (_uid, info) in slot_map.iter_mut() {
                info.onchain_bid_value = Decimal::ZERO;
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();
                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.onchain_bid_delivered_relay.clear();
                info.is_winning_bid_highest = false;
                info.fee_per_block = dec!(0.0);
            }
            continue;
        }

        // Slot-top value
        let slot_top_value = all_bids.iter().map(|v| v.value).fold(Decimal::ZERO, Decimal::max);

        // Partition by value
        let mut rproxy_at_top: Vec<&BidView> = Vec::new();
        let mut nonproxy_at_top_hosts: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if v.value == slot_top_value {
                if is_relay_proxy(v.relay) {
                    rproxy_at_top.push(v);
                } else {
                    nonproxy_at_top_hosts.insert(v.host.clone());
                }
            }
        }

        // Best non-proxy < top
        let mut best_nonproxy_val = Decimal::ZERO;
        let mut best_nonproxy_hosts_at_best: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if !is_relay_proxy(v.relay) && v.value < slot_top_value {
                match best_nonproxy_val.partial_cmp(&v.value) {
                    Some(std::cmp::Ordering::Less) => {
                        best_nonproxy_val = v.value;
                        best_nonproxy_hosts_at_best.clear();
                        best_nonproxy_hosts_at_best.insert(v.host.clone());
                    }
                    Some(std::cmp::Ordering::Equal) => {
                        best_nonproxy_hosts_at_best.insert(v.host.clone());
                    }
                    _ => {}
                }
            }
        }
        let best_nonproxy_host = best_nonproxy_hosts_at_best.iter().next().cloned().unwrap_or_default();

        // ======= Apply your rules =======
        // 1) If any non-proxy equals rproxy at the top value anywhere in the slot -> LOSS
        let slot_is_loss = !nonproxy_at_top_hosts.is_empty();

        // 2) If rproxy wins (no non-proxy at top), choose rproxy candidate with highest EL:
        //    EL = slot_top_value - best_nonproxy_val
        //    If tie (likely), pick lexicographically smallest (block_hash, host)
        let (chosen_hash, chosen_proxy_host, uplift, uplift_wei, pct_precise, pct_rounded, fee_per_block) =
            if !slot_is_loss && !rproxy_at_top.is_empty() {
                // compute uplift once (same for all rproxy tops, usually), but keep tie-breakers deterministic
                let uplift = {
                    let d = slot_top_value - best_nonproxy_val;
                    if d.is_sign_negative() { Decimal::ZERO } else { d }
                };
                let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                let uplift_wei_dec = (uplift * wei_multiplier).round();
                let uplift_wei = U256::from_dec_str(&uplift_wei_dec.to_string()).unwrap_or_else(|_| U256::zero());
                let pct_precise = if slot_top_value > Decimal::ZERO {
                    (uplift / slot_top_value) * Decimal::from(100)
                } else {
                    Decimal::ZERO
                };
                let pct_rounded = pct_precise.round().to_u64().unwrap_or(0);

                // choose rproxy winner deterministically
                let mut rproxy_sorted = rproxy_at_top.clone();
                rproxy_sorted.sort_by(|a, b| {
                    // primary: (same uplift); tie-break by block_hash then host
                    let o = a.block_hash.cmp(b.block_hash);
                    if o != std::cmp::Ordering::Equal { return o; }
                    a.host.cmp(&b.host)
                });
                let chosen = rproxy_sorted[0];
                // your fee rule (unchanged)
                let fee = if pct_precise <= dec!(1) {
                    dec!(0.0)
                } else if pct_precise <= dec!(5) {
                    if uplift >= dec!(0.0015) { dec!(0.0015) } else { dec!(0.0) }
                } else if pct_precise <= dec!(9) {
                    if uplift > dec!(0.003) { dec!(0.003) }
                    else if uplift > dec!(0.0015) { dec!(0.0015) }
                    else { dec!(0.0) }
                } else {
                    if uplift > dec!(0.005) { dec!(0.005) }
                    else if uplift > dec!(0.003) { dec!(0.003) }
                    else if uplift > dec!(0.0015) { dec!(0.0015) }
                    else { dec!(0.0) }
                };

                (chosen.block_hash.to_string(), chosen.host.clone(), uplift, uplift_wei, pct_precise, pct_rounded, fee)
            } else {
                (String::new(), String::new(), Decimal::ZERO, U256::zero(), Decimal::ZERO, 0u64, dec!(0.0))
            };

        // Build “equal_to_proxy_bidders” string deterministically for LOSS
        let eq_nonproxy_join = nonproxy_at_top_hosts.iter().cloned().collect::<Vec<_>>().join(", ");

        // For LOSS display, show all top hosts (proxy + non-proxy) at the top value
        let mut top_hosts_all: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if v.value == slot_top_value {
                top_hosts_all.insert(v.host.clone());
            }
        }
        let top_hosts_join = top_hosts_all.iter().cloned().collect::<Vec<_>>().join(", ");

        // ======= Write the SAME per-slot result to every UID in this slot =======
        for (_uid, info) in slot_map.iter_mut() {
            info.onchain_bid_value = slot_top_value;
            info.is_winning_bid_highest = true;

            if slot_is_loss {
                // proxy loss (tie at top anywhere)
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = true;
                info.equal_to_proxy_bidders = eq_nonproxy_join.clone();
                info.onchain_bid_delivered_relay = top_hosts_join.clone();

                // block hash: pick deterministic top hash by lex order among all at top (optional)
                // we keep whatever was resolved earlier unless you prefer enforcing chosen hash:
                // info.block_hash = (choose lex-smallest top hash here if you want)
                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.fee_per_block = dec!(0.0);
            } else {
                // proxy win
                info.is_proxy_win = true;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();

                // enforce deterministic winner hash + proxy host
                if !chosen_hash.is_empty() {
                    info.block_hash = chosen_hash.clone();
                }
                info.onchain_bid_delivered_relay = chosen_proxy_host.clone();

                // second best non-proxy (for EL calc display)
                info.second_highest_bid_value = best_nonproxy_val;
                info.second_higher_bid_delivered_relay = best_nonproxy_host.clone();

                info.el_reward_increase_eth = uplift;
                info.el_reward_increase_wei = uplift_wei;
                info.el_reward_increase_percent_precise = pct_precise;
                info.el_reward_increase_percentage = pct_rounded;
                info.fee_per_block = fee_per_block;
            }
        }
    }
}




#[derive(Debug, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct CommitBoostLogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub span: Span,

    #[serde(flatten)]
    pub fields: FlatFields, // flattened fields like value_eth, block_hash, etc.
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FlatFields {
    pub latency: Option<String>,
    pub value_eth: Option<String>,
    pub block_hash: Option<String>,
    pub relay_id: Option<String>,
    pub version: Option<String>,  // from getHeader log
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct Span {
    #[serde(rename = "req_id")]
    pub req_id: Option<String>,
    pub slot: Option<i64>,
    pub name: String,
    pub method: String,
    pub parent_hash: Option<String>,
    pub block_hash: Option<String>,
    pub block_number: Option<u64>,
    pub validator: Option<String>,
}
