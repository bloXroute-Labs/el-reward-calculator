
pub fn is_relay_proxy(relay: &str) -> bool {
    relay.contains("relay-proxy") || relay.contains("Relay Proxy") || relay.contains("rproxy") || relay.contains("rpoxy") // handle typo
}
