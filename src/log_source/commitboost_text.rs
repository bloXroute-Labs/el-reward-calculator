//! commitboost_text.rs — Parse Commit-Boost **text logs** (systemd/syslog style) and
//! populate CommitBoostSlotInfos using the same selection logic as the JSON parser.
//
//! Examples supported:
//! Jun 10 04:43:01 commit-boost-pbs[1355148]: 2025-06-10T04:43:01.958430Z  INFO : received unblinded block method=/eth/v1/builder/blinded_blocks req_id=... slot=11892213 block_hash=0xd72a... block_number=22671851 parent_hash=0x3b22...
//! Jun 10 00:20:59 host commit-boost-pbs[1645437]: 2025-06-10T00:20:59.382215Z DEBUG : received new header relay_id="renzo_primev_bloxroute_regulated" latency=282.03516ms version="electra" value_eth="0.033989248002005575" block_hash=0x7c9a... method=/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey} req_id=97cc... slot=11890903 parent_hash=0x63e5... validator=0xb3f9...
//
//! Add in Cargo.toml:
//! regex = "1"
//! once_cell = "1"

use crate::CommitBoostSlotInfos;
use crate::log_source::common::is_relay_proxy;
use crate::log_source::types::{Bid, CommitBoostRequest, CommitBoostSlotInfo, SlotTrait};
use chrono::{DateTime, Utc};
use ethers::types::U256;
use log::debug;
use once_cell::sync::Lazy;
use regex::Regex;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, BufReader};
use url::Url;

// Compile once, reuse everywhere
static TS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}T[0-9:\.]+Z").expect("valid ts regex"));
static KV_START_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:^|\s)[A-Za-z_][A-Za-z0-9_]*=").expect("valid kv-start regex"));


/// Public entrypoint used by `main.rs`: process **a single line** (matches your call site).
pub fn process_lines<S: AsRef<str>>(line: S, slot_infos: &mut CommitBoostSlotInfos) {
    if let Some(entry) = parse_text_line(line.as_ref()) {
        process_json(&entry, slot_infos);
    }
}

// ========================== Text line -> CommitBoostLogEntry ==========================

fn parse_text_line(line: &str) -> Option<CommitBoostLogEntry> {
    // 0) Find embedded RFC3339 timestamp (ignore syslog prefix before it)
    let ts_m = TS_RE.find(line)?;
    let timestamp = &line[ts_m.start()..ts_m.end()];
    let mut rest = line[ts_m.end()..].trim_start();

    // 1) Level token (INFO/DEBUG/ERROR...)
    let (level, rest_after_level) = split_once_ws(rest)?;
    rest = rest_after_level.trim_start();

    // 2) Optional ":" right after level
    if let Some(':') = rest.chars().next() {
        rest = rest[1..].trim_start();
    }

    // 3) Split message vs. k/v tail (first token that looks like "<key>=" starts KVs)
    let kv_start_idx = KV_START_RE.find(rest).map(|m| m.start()).unwrap_or(rest.len());
    let message = rest[..kv_start_idx].trim();
    let kv_tail = rest[kv_start_idx..].trim_start();

    let kv = parse_kv_pairs(kv_tail);

    let span = Span {
        req_id: kv.get("req_id").cloned(),
        slot: kv.get("slot").and_then(|s| s.parse::<i64>().ok()),
        name: String::new(), // not present in text logs
        method: kv.get("method").cloned().unwrap_or_default(),
        parent_hash: kv.get("parent_hash").cloned(),
        block_hash: kv.get("block_hash").cloned(),
        block_number: kv.get("block_number").and_then(|s| s.parse::<u64>().ok()),
        validator: kv.get("validator").cloned(),
    };

    let fields = FlatFields {
        latency: kv.get("latency").cloned(),
        value_eth: kv.get("value_eth").cloned(),
        block_hash: kv.get("block_hash").cloned(),
        relay_id: kv.get("relay_id").cloned(),
        version: kv.get("version").cloned(),
    };

    Some(CommitBoostLogEntry {
        timestamp: timestamp.to_string(),
        level: level.to_string(),
        message: message.to_string(),
        span,
        fields,
    })
}

/// Split a string at the first ASCII whitespace boundary, returning (head, tail).
fn split_once_ws(s: &str) -> Option<(&str, &str)> {
    let mut it = s.char_indices();
    while let Some((i, ch)) = it.next() {
        if ch.is_ascii_whitespace() {
            // consume the whole whitespace run
            let mut j = i + ch.len_utf8();
            for (k, c2) in it.by_ref() {
                if !c2.is_ascii_whitespace() {
                    j = k;
                    break;
                }
            }
            return Some((&s[..i], &s[j..]));
        }
    }
    None
}

/// Parse key/value pairs from a tail like:
/// `relay_id="renzo..." latency=282.0ms method=/eth/v1/... block_hash=0x...`
/// Supports quoted values with spaces; basic `\"` unescape.
fn parse_kv_pairs(s: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let b = s.as_bytes();
    let mut i = 0usize;

    fn is_key_byte(c: u8) -> bool {
        matches!(c, b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'0'..=b'9')
    }
    fn is_ws(c: u8) -> bool {
        c == b' ' || c == b'\t'
    }

    while i < b.len() {
        // skip whitespace
        while i < b.len() && is_ws(b[i]) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        // parse key
        let start_key = i;
        while i < b.len() && is_key_byte(b[i]) {
            i += 1;
        }
        if i == start_key || i >= b.len() || b[i] != b'=' {
            // not a key=..., skip to next space
            while i < b.len() && !is_ws(b[i]) {
                i += 1;
            }
            continue;
        }
        let key = &s[start_key..i];
        i += 1; // skip '='

        // parse value
        if i < b.len() && b[i] == b'"' {
            // quoted string
            i += 1; // skip opening quote
            let mut out = String::new();
            while i < b.len() {
                match b[i] {
                    b'\\' if i + 1 < b.len() => {
                        match b[i + 1] {
                            b'"' => {
                                out.push('"');
                                i += 2;
                            }
                            b'\\' => {
                                out.push('\\');
                                i += 2;
                            }
                            other => {
                                out.push(other as char);
                                i += 2;
                            }
                        }
                    }
                    b'"' => {
                        i += 1; // consume closing quote
                        break;
                    }
                    c => {
                        out.push(c as char);
                        i += 1;
                    }
                }
            }
            map.insert(key.to_string(), out);
        } else {
            // unquoted: read to next space
            let val_start = i;
            while i < b.len() && !is_ws(b[i]) {
                i += 1;
            }
            let val = &s[val_start..i];
            map.insert(key.to_string(), val.to_string());
        }
    }

    map
}

// ========================== Core processing (mirrors JSON version) ==========================

fn process_json(log_entry: &CommitBoostLogEntry, slot_infos: &mut CommitBoostSlotInfos) {
    let span = &log_entry.span;
    let slot = span.slot.unwrap_or_default().to_string();
    let parent_hash = span.parent_hash.clone().unwrap_or_else(|| "unknown".to_string());
    let slot_uid = format!("{}_{}", slot, parent_hash);

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    // Ensure merging happens if slot_uid already exists
    let slot_info = slot_info_map
        .entry(slot_uid.clone())
        .and_modify(|existing| merge_fields_into_slotinfo(existing, log_entry))
        .or_insert_with(|| {
            debug!("[INIT] Creating CommitBoostSlotInfo for slot_uid: {}", slot_uid);
            new_slot_info_from_log_entry(log_entry, slot_uid.clone(), slot.clone())
        });

    match span.method.as_str() {
        "/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey}" => {
            if log_entry.message == "received new header" {
                let req_id = span
                    .req_id
                    .clone()
                    .unwrap_or_else(|| "unknown_reqid".to_string());

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

                    if slot_info.selected_req_id.is_none()
                        || slot_info.get_block_hash() != block_hash
                    {
                        debug!(
                            "[SUBMIT] Selected best matching req_id={} with highest bid for block_hash={}",
                            best_req_id, block_hash
                        );
                        slot_info.selected_req_id = Some(best_req_id.clone());
                        slot_info.block_hash = block_hash;
                        slot_info.block_number =
                            format!("{}", span.block_number.unwrap_or_default());
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

// ===== Free helper functions (avoid duplicate inherent impls) =====

fn merge_fields_into_slotinfo(info: &mut CommitBoostSlotInfo, log_entry: &CommitBoostLogEntry) {
    if info.block_hash.is_empty() {
        if let Some(bh) = &log_entry.fields.block_hash {
            info.block_hash = bh.clone();
        }
    }

    if info.block_number.is_empty() {
        if let Some(num) = log_entry.span.block_number {
            if num != 0 {
                info.block_number = num.to_string();
            }
        }
    }
}

fn new_slot_info_from_log_entry(
    log_entry: &CommitBoostLogEntry,
    slot_uid: String,
    slot: String,
) -> CommitBoostSlotInfo {
    let mut info = CommitBoostSlotInfo::new(slot_uid, slot);
    merge_fields_into_slotinfo(&mut info, log_entry);
    info
}

// ========================== Per-slot reconciliation (same rules as JSON) ==========================

fn host_from(relay: &str) -> String {
    Url::parse(relay)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| relay.to_string())
}

/// If you call the JSON version elsewhere, you can omit this and reuse that.
/// Keeping a local copy is fine because it's in a different module namespace.
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
                    if slot_info.selected_req_id.is_none()
                        && !slot_info.pending_blinded_block_hashes.is_empty()
                    {
                        for blinded_block_hash in &slot_info.pending_blinded_block_hashes {
                            let mut matched: Vec<(&String, &CommitBoostRequest)> = slot_info
                                .requests
                                .iter()
                                .filter(|(_, req)| {
                                    req.bids
                                        .iter()
                                        .any(|b| &b.block_hash == blinded_block_hash)
                                })
                                .collect();

                            if !matched.is_empty() {
                                // sort: highest value for that hash, then req_id
                                matched.sort_by(|(aid, a), (bid, b)| {
                                    let a_max = a
                                        .bids
                                        .iter()
                                        .filter(|x| &x.block_hash == blinded_block_hash)
                                        .map(|x| x.bid_value)
                                        .fold(Decimal::ZERO, Decimal::max);
                                    let b_max = b
                                        .bids
                                        .iter()
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
                                        _ => {
                                            best_bid = Some((
                                                req_id.clone(),
                                                bid.block_hash.clone(),
                                                bid.bid_value,
                                            ))
                                        }
                                    }
                                }
                            }
                        }
                        if let Some((best_req_id, block_hash, _)) = best_bid {
                            debug!(
                                "[AUTO-MATCH] Selected best bid req_id={} block_hash={} (fallback)",
                                best_req_id, block_hash
                            );
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

        #[derive(Clone)]
        struct BidView<'a> {
            relay: &'a str,
            host: String,
            block_hash: &'a str,
            value: Decimal,
        }

        // Gather ALL bids across ALL UIDs/requests in this slot
        let mut all_bids: Vec<BidView> = Vec::new();
        for (_uid, info) in slot_map.iter() {
            for (_rid, req) in &info.requests {
                for b in &req.bids {
                    if b.block_hash.is_empty() || b.bid_value <= Decimal::ZERO {
                        continue;
                    }
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
        let slot_top_value = all_bids
            .iter()
            .map(|v| v.value)
            .fold(Decimal::ZERO, Decimal::max);

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
        let best_nonproxy_host = best_nonproxy_hosts_at_best
            .iter()
            .next()
            .cloned()
            .unwrap_or_default();

        // 1) If any non-proxy equals rproxy at the top value anywhere in the slot -> LOSS
        let slot_is_loss = !nonproxy_at_top_hosts.is_empty();

        // 2) If rproxy wins (no non-proxy at top), choose rproxy candidate deterministically
        let (chosen_hash, chosen_proxy_host, uplift, uplift_wei, pct_precise, pct_rounded, fee_per_block) =
            if !slot_is_loss && !rproxy_at_top.is_empty() {
                let uplift = {
                    let d = slot_top_value - best_nonproxy_val;
                    if d.is_sign_negative() { Decimal::ZERO } else { d }
                };
                let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                let uplift_wei_dec = (uplift * wei_multiplier).round();
                let uplift_wei =
                    U256::from_dec_str(&uplift_wei_dec.to_string()).unwrap_or_else(|_| U256::zero());
                let pct_precise = if slot_top_value > Decimal::ZERO {
                    (uplift / slot_top_value) * Decimal::from(100)
                } else {
                    Decimal::ZERO
                };
                let pct_rounded = pct_precise.round().to_u64().unwrap_or(0);

                let mut rproxy_sorted = rproxy_at_top.clone();
                rproxy_sorted.sort_by(|a, b| {
                    let o = a.block_hash.cmp(b.block_hash);
                    if o != std::cmp::Ordering::Equal { return o; }
                    a.host.cmp(&b.host)
                });
                let chosen = rproxy_sorted[0];

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

        // LOSS details
        let eq_nonproxy_join = nonproxy_at_top_hosts.iter().cloned().collect::<Vec<_>>().join(", ");

        // All top hosts (proxy + non-proxy)
        let mut top_hosts_all: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if v.value == slot_top_value {
                top_hosts_all.insert(v.host.clone());
            }
        }
        let top_hosts_join = top_hosts_all.iter().cloned().collect::<Vec<_>>().join(", ");

        // Write the SAME per-slot result to every UID in this slot
        for (_uid, info) in slot_map.iter_mut() {
            info.onchain_bid_value = slot_top_value;
            info.is_winning_bid_highest = true;

            if slot_is_loss {
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = true;
                info.equal_to_proxy_bidders = eq_nonproxy_join.clone();
                info.onchain_bid_delivered_relay = top_hosts_join.clone();

                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.fee_per_block = dec!(0.0);
            } else {
                info.is_proxy_win = true;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();

                if !chosen_hash.is_empty() {
                    info.block_hash = chosen_hash.clone();
                }
                info.onchain_bid_delivered_relay = chosen_proxy_host.clone();

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

// ========================== Local types (module-private) ==========================

#[derive(Debug, Default, Serialize, Deserialize)]
#[allow(dead_code)]
struct CommitBoostLogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub span: Span,

    #[serde(flatten)]
    pub fields: FlatFields, // flattened fields like value_eth, block_hash, etc.
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct FlatFields {
    pub latency: Option<String>,
    pub value_eth: Option<String>,
    pub block_hash: Option<String>,
    pub relay_id: Option<String>,
    pub version: Option<String>, // from getHeader log
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct Span {
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
