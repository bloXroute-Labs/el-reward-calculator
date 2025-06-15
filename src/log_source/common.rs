use crate::log_source::types::{Bid};
use url::Url;

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
