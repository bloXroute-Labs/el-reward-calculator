use rust_decimal::Decimal;
use serde::Deserialize;
use serde::Serialize;
use serde::Serializer;
use ethers::types::U256;
use std::i64;
use std::collections::HashMap;

// commit boost
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CommitBoostSlotInfo {
    pub slot: String,
    pub slot_uid: String,
    pub block_number: String,

    /// All getHeader requests mapped by req_id
    pub requests: HashMap<String, CommitBoostRequest>,

    /// The selected request (identified via getPayload)
    pub selected_req_id: Option<String>,
    pub block_hash: String,
    /// These fields will be computed post processing:
    pub is_proxy_win: bool,
    pub is_winning_bid_highest: bool,
    #[serde(serialize_with = "u256_to_string")]
    pub el_reward_increase_wei: U256,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub el_reward_increase_eth: Decimal,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub onchain_bid_value: Decimal,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub second_highest_bid_value: Decimal,
    pub onchain_bid_delivered_relay: String,
    pub second_higher_bid_delivered_relay: String,
    pub is_payload_received: bool,
    pub el_reward_increase_percentage: u64,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub el_reward_increase_percent_precise: Decimal,
    pub equal_to_proxy_bidders: String,
    pub is_equal_to_proxy_bid: bool,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub fee_per_block: Decimal,
    pub pending_blinded_block_hashes: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct CommitBoostRequest {
    pub header_start_ms_into_slot: i64,
    pub payload_start_ms_into_slot: i64,
    pub block_hash: String,
    pub pubkey: String,
    pub parent_hash: String,
    pub block_number: String,
    pub bids: Vec<Bid>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd)]
pub struct BidSet {
   pub bids: Vec<Bid>,
   pub pubkey: String,
   pub parent_hash: String,
   pub block_number: String,
}

#[derive(Clone, Debug,Default,Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LogEntryVouch {
    pub level: String,
    pub service: String,
    #[serde(rename = "impl")]
    pub impl_field: String,
    pub slot: i64,
    pub provider: String,
    pub value: String,
    #[serde(rename = "value_delta")]
    pub value_delta: String,
    pub score: String,
    #[serde(rename = "score_delta")]
    pub score_delta: String,
    pub selected: bool,
    pub time: String,
    pub message: String,
}
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MEVBoostJSONLogEntry {
    pub level: String,
    pub method: String,
    pub msg: String,
    pub slot: String,
    #[serde(rename = "slotUID", default)]
    pub slot_uid: String,
    pub time: String,
    #[serde(default)]
    pub block_hash: String,
    #[serde(default)]
    pub parent_hash: String,
    #[serde(default)]
    pub ua: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub version: String,
    // Optional only needed for `getHeader` -> bid received
    #[serde(default)]
    pub block_number: Option<i64>,
    #[serde(default)]
    pub pubkey: Option<String>,
    #[serde(default)]
    pub tx_root: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Clone, Debug,Default,Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LogEntry {
    pub client: String,
    pub filename: String,
    pub hostname: String,
    pub log_format_version: String,
    pub logstamp: u64,
    pub message: Message,
    pub network: String,
    pub syslog_identifier: String,
}

#[derive(Clone, Debug,Default,Serialize, Deserialize)]
#[allow(non_snake_case, dead_code)]
pub struct Message {
    pub blockHash: String,
    pub level: String,
    pub method: String,
    pub msg: String,
    pub parentHash: String,
    pub slot: String,
    pub slotUID: String,
    pub time: String,
    #[serde(alias = "ua", alias = "userAgent")]
    pub ua: String,
    pub url: Option<String>,
    pub version: String,
    pub blockNumber: Option<u64>,
    pub pubkey: Option<String>,
    pub txRoot: Option<String>,
    pub value: Option<String>,
}

#[derive(Clone, Debug,Default,Serialize, Deserialize)]
pub struct SlotInfo {
    pub slot_uid: String,
    pub slot: String,
    pub block_number: String,
    pub info: RequestInfo,
    pub is_proxy_win: bool,
    pub is_winning_bid_highest: bool,
    #[serde(serialize_with = "u256_to_string")]
    pub el_reward_increase_wei: U256,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub el_reward_increase_eth: Decimal,
    pub onchain_bid_value: Decimal,
    pub second_highest_bid_value: Decimal,
    pub onchain_bid_delivered_relay: String,
    pub second_higher_bid_delivered_relay: String,
    pub is_payload_received: bool,
    pub el_reward_increase_percentage: u64,
    pub el_reward_increase_percent_precise: Decimal,
    pub equal_to_proxy_bidders: String,
    pub is_equal_to_proxy_bid: bool,
    pub fee_per_block: Decimal,
}

pub fn u256_to_string<S>(value: &U256, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd)]
pub struct RequestInfo {
    pub header_start_ms_into_slot: i64,
    pub bids: Vec<Bid>,
    pub payload_start_ms_into_slot: i64,
    pub block_hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd, Clone)]
pub struct Bid {
    pub timestamp: i64,
    pub pubkey: String,
    pub block_hash: String,
    pub parent_hash: String,
    pub block_number: String,
    pub slot: String,
    pub ua: String,
    pub relay: String,
    pub bid_value: Decimal,
}


#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SlotInfoWithoutBids<'a> {
    pub slot_uid: &'a str,
    pub slot: &'a str,
    pub block_number: &'a str,
    pub header_start_ms_into_slot: i64,
    pub payload_start_ms_into_slot: i64,
    pub block_hash: &'a str,
    pub is_proxy_win: bool,
    pub is_winning_bid_highest: bool,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub el_reward_increase_eth: Decimal,
    pub el_reward_increase_wei: U256,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub onchain_bid_value: Decimal,
    pub onchain_bid_delivered_relay: String,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub second_highest_bid_value: Decimal,
    pub second_higher_bid_delivered_relay: String,
    pub is_payload_received: bool,
    pub el_reward_increase_percentage: u64,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub el_reward_increase_percent_precise: Decimal,
    pub equal_to_proxy_bidders: String,
    pub is_equal_to_proxy_bid: bool,
    #[serde(serialize_with = "decimal_to_fixed")]
    pub fee_per_block: Decimal,
}

pub fn decimal_to_fixed<S>(x: &Decimal, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{:.18}", x))
}

impl SlotInfo {
    pub fn new(slot_uid: String) -> Self {
        Self {
            slot_uid,
            slot: String::new(),
            block_number: String::new(),
            info: Default::default(),
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: Decimal::ZERO ,
            onchain_bid_value:Decimal::ZERO ,
            second_highest_bid_value: Decimal::ZERO ,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: Decimal::ZERO ,
            equal_to_proxy_bidders:String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block: Decimal::ZERO,
        }
    }

    // initializes with both slot_uid and slot
    #[allow(dead_code)]
    pub fn new_with_slot_uid_and_slot(slot_uid: String, slot: String) -> Self {
        Self {
            slot_uid,
            slot,
            block_number: String::new(),
            info: Default::default(),
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: Decimal::ZERO ,
            onchain_bid_value: Decimal::ZERO ,
            second_highest_bid_value: Decimal::ZERO ,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: Decimal::ZERO ,
            equal_to_proxy_bidders:String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block: Decimal::ZERO ,
        }
    }

}

impl CommitBoostSlotInfo {
    pub fn new(slot_uid: String, slot: String) -> Self {
        Self {
            slot_uid,
            slot,
            block_number: String::new(),
            block_hash: String::new(),
            requests: std::collections::HashMap::new(),
            selected_req_id: None,
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: Decimal::ZERO ,
            onchain_bid_value: Decimal::ZERO ,
            second_highest_bid_value: Decimal::ZERO ,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: Decimal::ZERO ,
            equal_to_proxy_bidders: String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block: Decimal::ZERO ,
            pending_blinded_block_hashes: Vec::new(),
        }
    }
}
pub trait SlotTrait {
    fn get_uid(&self) -> &str;
    fn get_block_number(&self) -> &str;
    fn get_slot(&self) -> &str;
    fn get_block_hash(&self) -> &str;
    fn get_header_start(&self) -> i64;
    fn get_payload_start(&self) -> i64;
    fn is_proxy_win(&self) -> bool;
    fn is_winning_bid_highest(&self) -> bool;
    fn get_el_reward_eth(&self) -> Decimal;
    fn get_el_reward_wei(&self) -> U256;
    fn get_onchain_bid_value(&self) -> Decimal;
    fn get_onchain_bid_delivered_relay(&self) -> &str;
    fn get_second_highest_bid_value(&self) -> Decimal;
    fn get_second_higher_bid_delivered_relay(&self) -> &str;
    fn is_payload_received(&self) -> bool;
    fn get_el_reward_percentage(&self) -> u64;
    fn get_el_reward_precise(&self) -> Decimal;
    fn get_equal_to_proxy_bidders(&self) -> &str;
    fn is_equal_to_proxy_bid(&self) -> bool;
    fn get_fee_per_block(&self) -> Decimal;
}

impl SlotTrait for SlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_hash(&self) -> &str { &self.info.block_hash }
    fn get_header_start(&self) -> i64 { self.info.header_start_ms_into_slot }
    fn get_payload_start(&self) -> i64 { self.info.payload_start_ms_into_slot }
    fn is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_el_reward_eth(&self) -> Decimal { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_onchain_bid_value(&self) -> Decimal { self.onchain_bid_value }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_second_highest_bid_value(&self) -> Decimal { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> Decimal { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> Decimal { self.fee_per_block }
}

impl SlotTrait for CommitBoostSlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_hash(&self) -> &str { &self.block_hash }

    fn get_header_start(&self) -> i64 {
        self.selected_req_id
            .as_ref()
            .and_then(|rid| self.requests.get(rid))
            .map(|req| req.header_start_ms_into_slot)
            .unwrap_or_default()
    }

    fn get_payload_start(&self) -> i64 {
        self.selected_req_id
            .as_ref()
            .and_then(|rid| self.requests.get(rid))
            .map(|req| req.payload_start_ms_into_slot)
            .unwrap_or_default()
    }

    fn is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_el_reward_eth(&self) -> Decimal { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_onchain_bid_value(&self) -> Decimal { self.onchain_bid_value }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_second_highest_bid_value(&self) -> Decimal { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn is_payload_received(&self) -> bool { self.selected_req_id.is_some() }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> Decimal { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> Decimal { self.fee_per_block }
}
