use crate::CommitBoostSlotInfos;
use crate::log_source::common::{is_relay_proxy,get_slot_start_time_utc};
use crate::log_source::types::{Bid, CommitBoostRequest, CommitBoostSlotInfo};
use chrono::{DateTime, Utc, SecondsFormat};
use ethers::types::U256;
use log::debug;
use once_cell::sync::Lazy;
use regex::Regex;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use url::Url;

// Compile once, reuse everywhere
static TS_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}T[0-9:\.]+Z").expect("valid ts regex"));
static KV_START_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?:^|\s)[A-Za-z_][A-Za-z0-9_]*=").expect("valid kv-start regex"));

pub fn process_lines<S: AsRef<str>>(line: S, slot_infos: &mut CommitBoostSlotInfos) {
    if let Some(entry) = parse_text_line(line.as_ref()) {
        process_json(&entry, slot_infos);
    }
}

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

fn process_json(log_entry: &CommitBoostLogEntry, slot_infos: &mut CommitBoostSlotInfos) {
    let span = &log_entry.span;
    let slot = span.slot.unwrap_or_default().to_string();
    let parent_hash = span.parent_hash.clone().unwrap_or_else(|| "unknown".to_string());
    let slot_uid = format!("{}_{}", slot, parent_hash);

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    // merging happens if slot_uid already exists
    let slot_info = slot_info_map
        .entry(slot_uid.clone())
        .and_modify(|existing| existing.merge_fields_from_text_entry(log_entry))
        .or_insert_with(|| {
            debug!("[INIT] Creating CommitBoostSlotInfo for slot_uid: {}", slot_uid);
            CommitBoostSlotInfo::from_text_entry(log_entry, slot_uid.clone(), slot.clone())
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

                // Resolve only if header's block_hash is one of the previously seen BLINDED hashes.
                if slot_info.selected_req_id.is_none()
                    && !bid.block_hash.is_empty()
                    && slot_info.pending_blinded_block_hashes.contains(&bid.block_hash)
                {
                    debug!(
                        "[RESOLVE] Header matched blinded block {}; setting selected_req_id={}",
                        bid.block_hash, req_id
                    );
                    slot_info.selected_req_id = Some(req_id);
                    // Safe: guaranteed member of pending_blinded_block_hashes
                    slot_info.block_hash = bid.block_hash.clone();
                }
            }
        }

        "/eth/v1/builder/blinded_blocks" => {
            if log_entry.message == "received unblinded block" {
                let block_hash = span.block_hash.clone().unwrap_or_default();

                // Always track blinded block hashes (authoritative on-chain candidates)
                if !block_hash.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&block_hash) {
                    slot_info.pending_blinded_block_hashes.push(block_hash.clone());
                }

                // Try to match to an existing request/bid with SAME block_hash
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
                        || slot_info.block_hash != block_hash
                    {
                        debug!(
                            "[SUBMIT] Selected best matching req_id={} with highest bid for blinded block_hash={}",
                            best_req_id, block_hash
                        );
                        slot_info.selected_req_id = Some(best_req_id.clone());
                        // Safe: member of pending_blinded_block_hashes (enforced above)
                        slot_info.block_hash = block_hash.clone();
                        slot_info.block_number =
                            format!("{}", span.block_number.unwrap_or_default());
                    }
                } else {
                    // keep it recorded only; resolution may happen later via header
                    debug!(
                        "[DEFER] Blinded block {} recorded for slot_uid={}",
                        block_hash, slot_uid
                    );
                }
            }
        }

        _ => {}
    }
}

// === Text-only helpers on CommitBoostSlotInfo (distinct names to avoid conflicts) ===
impl CommitBoostSlotInfo {
    /// Never set `block_hash` here; only via blinded matches. Capture `block_number` opportunistically.
    pub fn merge_fields_from_text_entry(&mut self, log_entry: &CommitBoostLogEntry) {
        if self.block_number.is_empty() {
            if let Some(num) = log_entry.span.block_number {
                if num != 0 {
                    self.block_number = num.to_string();
                }
            }
        }
    }

    pub fn from_text_entry(
        log_entry: &CommitBoostLogEntry,
        slot_uid: String,
        slot: String,
    ) -> Self {
        let mut info = CommitBoostSlotInfo::new(slot_uid, slot);
        info.merge_fields_from_text_entry(log_entry);
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
    let mut slots: Vec<_> = slot_infos.keys().cloned().collect();
    slots.sort();

    // ---------- Pass 1: resolve each UID (late match + STRICT fallback from blinded set) ----------
    for slot in &slots {
        if let Some(slot_map) = slot_infos.get_mut(slot) {
            let mut slot_uids: Vec<_> = slot_map.keys().cloned().collect();
            slot_uids.sort();

            for slot_uid in slot_uids {
                if let Some(slot_info) = slot_map.get_mut(&slot_uid) {
                    // Late match using pending hashes (kept from process_json)
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
                                // Safe: from pending_blinded_block_hashes
                                slot_info.block_hash = blinded_block_hash.clone();
                                break;
                            }
                        }
                    }

                    // STRICT fallback: only choose a bid whose hash is among blinded hashes
                    if slot_info.selected_req_id.is_none()
                        && !slot_info.pending_blinded_block_hashes.is_empty()
                    {
                        let allowed: BTreeSet<&str> = slot_info
                            .pending_blinded_block_hashes
                            .iter()
                            .map(|s| s.as_str())
                            .collect();

                        let mut best_allowed: Option<(String, String, Decimal)> = None;
                        for (req_id, req) in &slot_info.requests {
                            for bid in &req.bids {
                                if !bid.block_hash.is_empty()
                                    && allowed.contains(bid.block_hash.as_str())
                                    && bid.bid_value > Decimal::ZERO
                                {
                                    match best_allowed {
                                        Some((_, _, ref cur)) if bid.bid_value <= *cur => {}
                                        _ => best_allowed = Some((req_id.clone(), bid.block_hash.clone(), bid.bid_value)),
                                    }
                                }
                            }
                        }

                        if let Some((best_req_id, block_hash, _)) = best_allowed {
                            debug!(
                                "[AUTO-MATCH/STRICT] Selected req_id={} block_hash={} (from blinded set)",
                                best_req_id, block_hash
                            );
                            slot_info.selected_req_id = Some(best_req_id);
                            slot_info.block_hash = block_hash;
                        }
                    }

                    // If some earlier step set a non-blinded hash, purge it
                    if !slot_info.block_hash.is_empty()
                        && !slot_info.pending_blinded_block_hashes.contains(&slot_info.block_hash)
                    {
                        debug!(
                            "[SANITY] Clearing non-blinded block_hash={} in slot_uid={}",
                            slot_info.block_hash, slot_uid
                        );
                        slot_info.block_hash.clear();
                    }
                }
            }
        }
    }

    // ---------- Pass 2: PER-UID, PER-REQUEST reconciliation ----------
    for slot in slots {
        let Some(slot_map) = slot_infos.get_mut(&slot) else { continue; };

        // Stamp RFC3339 slot-start time once per UID
        let slot_i64 = slot.parse::<i64>().unwrap_or_default();
        let slot_start_dt = get_slot_start_time_utc(slot_i64);
        let slot_start_rfc3339 = slot_start_dt.to_rfc3339_opts(SecondsFormat::Millis, true);
        for (_uid, info) in slot_map.iter_mut() {
            if info.time.is_empty() {
                info.time = slot_start_rfc3339.clone();
            }
        }

        // Reconcile each UID independently, scoped to its selected request
        for (_uid, info) in slot_map.iter_mut() {
            // 0) Pick request to evaluate
            let selected_req = if let Some(rid) = info.selected_req_id.as_ref() {
                info.requests.get(rid)
            } else {
                // Fallback: best request by highest rproxy bid restricted to blinded hashes (if any)
                let allowed: Option<BTreeSet<&str>> = if info.pending_blinded_block_hashes.is_empty() {
                    None
                } else {
                    Some(info.pending_blinded_block_hashes.iter().map(|s| s.as_str()).collect())
                };

                let mut best: Option<(&String, &CommitBoostRequest, Decimal)> = None;
                for (rid, req) in &info.requests {
                    let mut best_in_req = Decimal::ZERO;
                    for b in &req.bids {
                        if b.bid_value <= Decimal::ZERO { continue; }
                        if !is_relay_proxy(&b.relay) { continue; }
                        if let Some(ref allow) = allowed {
                            if !allow.contains(b.block_hash.as_str()) { continue; }
                        }
                        if b.bid_value > best_in_req {
                            best_in_req = b.bid_value;
                        }
                    }
                    if best_in_req > Decimal::ZERO {
                        match best {
                            Some((_, _, cur)) if best_in_req <= cur => {}
                            _ => best = Some((rid, req, best_in_req)),
                        }
                    }
                }
                if let Some((rid, _req, _)) = best {
                    info.selected_req_id = Some(rid.clone());
                    info.requests.get(rid)
                } else {
                    None
                }
            };

            // If we still don't have a request → zero metrics
            let Some(req) = selected_req else {
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
                continue;
            };

            // 1) Use only this request’s bids (>0)
            let mut req_bids: Vec<&Bid> = req.bids.iter().filter(|b| b.bid_value > Decimal::ZERO).collect();
            if req_bids.is_empty() {
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
                continue;
            }

            // 2) Enforce header<->blinded match within THIS UID
            let allowed: BTreeSet<&str> = info.pending_blinded_block_hashes.iter().map(|s| s.as_str()).collect();

            if !info.block_hash.is_empty() && !allowed.contains(info.block_hash.as_str()) {
                debug!("[SANITY-REQ] Clearing non-blinded block_hash={} (slot={})", info.block_hash, slot);
                info.block_hash.clear();
            }

            if info.block_hash.is_empty() {
                // pick best hash inside this request that is in the blinded set
                let mut best_for_allowed: Option<(String, Decimal)> = None;
                for b in &req_bids {
                    if b.block_hash.is_empty() || !allowed.contains(b.block_hash.as_str()) { continue; }
                    match best_for_allowed {
                        Some((_, ref cur)) if b.bid_value <= *cur => {}
                        _ => best_for_allowed = Some((b.block_hash.clone(), b.bid_value)),
                    }
                }
                if let Some((hash, _)) = best_for_allowed {
                    info.block_hash = hash;
                } else {
                    // no header<->blinded match → zero metrics
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
                    continue;
                }
            }

            // Keep chosen hash in pending (invariant)
            if !info.block_hash.is_empty() && !info.pending_blinded_block_hashes.contains(&info.block_hash) {
                info.pending_blinded_block_hashes.push(info.block_hash.clone());
            }

            // 3) Compute request-top & tie sets (REQUEST-SCOPED)
            let req_top_value = req_bids.iter().map(|b| b.bid_value).max().unwrap_or(Decimal::ZERO);

            let mut nonproxy_at_top_hosts: BTreeSet<String> = BTreeSet::new();
            for b in &req_bids {
                if b.bid_value == req_top_value && !is_relay_proxy(&b.relay) {
                    nonproxy_at_top_hosts.insert(host_from(&b.relay));
                }
            }
            let req_is_loss = !nonproxy_at_top_hosts.is_empty();

            // Winner for the chosen hash (deterministic)
            let mut candidates_for_hash: Vec<&Bid> = req_bids.iter().copied().filter(|b| b.block_hash == info.block_hash).collect();
            if candidates_for_hash.is_empty() {
                // safety: treat as no header match
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
                continue;
            }

            candidates_for_hash.sort_by(|a, b| {
                let o = b.bid_value.cmp(&a.bid_value);
                if o != std::cmp::Ordering::Equal { return o; }
                host_from(&a.relay).cmp(&host_from(&b.relay))
            });
            let winner = candidates_for_hash[0];
            let onchain_val = winner.bid_value;

            // Best non-proxy below the chosen value (REQUEST-SCOPED)
            let second_best_nonproxy = req_bids
                .iter()
                .copied()
                .filter(|b| !is_relay_proxy(&b.relay) && b.bid_value < onchain_val)
                .max_by(|a, b| a.bid_value.cmp(&b.bid_value));

            let second_val = second_best_nonproxy.map(|b| b.bid_value).unwrap_or(Decimal::ZERO);
            let second_host = second_best_nonproxy.map(|b| host_from(&b.relay)).unwrap_or_default();

            // 4) Fill fields
            info.onchain_bid_value = onchain_val;
            info.is_winning_bid_highest = onchain_val == req_top_value;

            if req_is_loss {
                // tie at top with non-proxy inside THIS request
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = true;
                info.equal_to_proxy_bidders = nonproxy_at_top_hosts.iter().cloned().collect::<Vec<_>>().join(", ");

                // show all top hosts in this request
                let mut all_top: BTreeSet<String> = BTreeSet::new();
                for b in &req_bids {
                    if b.bid_value == req_top_value {
                        all_top.insert(host_from(&b.relay));
                    }
                }
                info.onchain_bid_delivered_relay = all_top.into_iter().collect::<Vec<_>>().join(", ");

                // zero uplift & fees on loss/tie
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.fee_per_block = dec!(0.0);
            } else {
                // proxy win inside THIS request
                info.is_proxy_win = true;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();

                // Delivered host for chosen hash @ winner value (lexicographically smallest)
                let mut hosts_for_val: Vec<String> = candidates_for_hash
                    .iter()
                    .filter(|b| b.bid_value == onchain_val)
                    .map(|b| host_from(&b.relay))
                    .collect();
                hosts_for_val.sort();
                info.onchain_bid_delivered_relay =
                    hosts_for_val.get(0).cloned().unwrap_or_else(|| host_from(&winner.relay));

                info.second_highest_bid_value = second_val;
                info.second_higher_bid_delivered_relay = second_host;

                // uplift & fees (request-scoped)
                let mut uplift = onchain_val - second_val;
                if uplift.is_sign_negative() { uplift = Decimal::ZERO; }

                if uplift.is_zero() || onchain_val.is_zero() {
                    info.el_reward_increase_wei = U256::zero();
                    info.el_reward_increase_eth = Decimal::ZERO;
                    info.el_reward_increase_percent_precise = Decimal::ZERO;
                    info.el_reward_increase_percentage = 0;
                    info.fee_per_block = dec!(0.0);
                } else {
                    let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                    let uplift_wei_dec = (uplift * wei_multiplier).round();
                    let uplift_wei = U256::from_dec_str(&uplift_wei_dec.to_string()).unwrap_or_else(|_| U256::zero());
                    let pct_precise = (uplift / onchain_val) * Decimal::from(100);

                    info.el_reward_increase_wei = uplift_wei;
                    info.el_reward_increase_eth = uplift;
                    info.el_reward_increase_percent_precise = pct_precise;
                    info.el_reward_increase_percentage = pct_precise.round().to_u64().unwrap_or(0);

                    info.fee_per_block = if pct_precise <= dec!(1) {
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
                }
            }

            // Final invariant: any set block_hash must be in the pending (blinded) list
            if !info.block_hash.is_empty()
                && !info.pending_blinded_block_hashes.contains(&info.block_hash)
            {
                debug!("[SANITY-FINAL] Clearing non-blinded block_hash={} (slot={})", info.block_hash, slot);
                info.block_hash.clear();
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
    pub version: Option<String>, // from getHeader log
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
