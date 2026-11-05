use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};

use serde::Deserialize;

use crate::config::HomeNetworkConfig;

/// The network eco answers other devices on. Café and office networks often
/// reuse the same subnet, so the subnet alone doesn't prove the machine is home;
/// the router's hardware address does.
#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(try_from = "HomeNetworkConfig")]
pub struct HomeNetwork {
    network: u32,
    mask: u32,
    router_mac: [u8; 6],
}

impl HomeNetwork {
    pub fn parse(subnet: &str, router_mac: &str) -> Result<Self, String> {
        let invalid_subnet = || format!("home_network.subnet {subnet:?} is not like \"192.168.1.0/24\"");
        let (address, prefix) = subnet.split_once('/').ok_or_else(invalid_subnet)?;
        let address: Ipv4Addr = address.parse().map_err(|_| invalid_subnet())?;
        let prefix: u32 = prefix.parse().ok().filter(|p| (1..=32).contains(p)).ok_or_else(invalid_subnet)?;
        let mask = u32::MAX << (32 - prefix);
        let router_mac = parse_mac(router_mac)
            .ok_or_else(|| format!("home_network.router_mac {router_mac:?} is not like \"aa:bb:cc:dd:ee:ff\""))?;
        Ok(Self { network: u32::from(address) & mask, mask, router_mac })
    }

    fn contains(&self, ip: Ipv4Addr) -> bool {
        u32::from(ip) & self.mask == self.network
    }

    /// Whether this machine is connected to the home router right now.
    pub fn is_current(&self) -> bool {
        router_mac() == Some(self.router_mac)
    }

    /// Decides on a connection from `peer` that arrived at `local`, short of the
    /// router lookup. This machine itself is always allowed.
    pub fn judge(&self, peer: IpAddr, local: IpAddr) -> Verdict {
        // A dual-stack listener reports IPv4 peers as IPv4-mapped IPv6 addresses.
        let peer = peer.to_canonical();
        if peer.is_loopback() || peer == local.to_canonical() {
            return Verdict::Allow;
        }
        match peer {
            IpAddr::V4(ip) if self.contains(ip) => Verdict::AllowIfHome,
            _ => Verdict::Refuse,
        }
    }
}

impl TryFrom<HomeNetworkConfig> for HomeNetwork {
    type Error = String;

    fn try_from(config: HomeNetworkConfig) -> Result<Self, String> {
        Self::parse(&config.subnet, &config.router_mac)
    }
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Allow,
    /// Allowed only while this machine is on the home network (`is_current`).
    AllowIfHome,
    Refuse,
}

/// Parses a colon-separated MAC. macOS `arp` drops leading zeros ("a:1b:..."),
/// so octets are compared as numbers rather than strings.
fn parse_mac(mac: &str) -> Option<[u8; 6]> {
    let octets: Vec<u8> = mac.split(':').map(|o| u8::from_str_radix(o, 16).ok()).collect::<Option<_>>()?;
    octets.try_into().ok()
}

pub fn format_mac(mac: [u8; 6]) -> String {
    mac.map(|o| format!("{o:02x}")).join(":")
}

/// This machine's LAN address and netmask: the interface that routes to the default gateway.
pub fn lan_address() -> Option<(Ipv4Addr, Ipv4Addr)> {
    let gateway = default_gateway()?;
    // Connecting a UDP socket sends nothing; it only asks the OS which local
    // address would be used to reach the gateway.
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((gateway, 9)).ok()?;
    let IpAddr::V4(ip) = socket.local_addr().ok()?.ip() else { return None };
    let netmask = if_addrs::get_if_addrs().ok()?.into_iter().find_map(|interface| match interface.addr {
        if_addrs::IfAddr::V4(v4) if v4.ip == ip => Some(v4.netmask),
        _ => None,
    })?;
    Some((ip, netmask))
}

/// The address other devices use to reach `listen`: the LAN address when
/// listening on every interface.
pub fn reachable_addr(listen: SocketAddr) -> Option<SocketAddr> {
    if !listen.ip().is_unspecified() {
        return Some(listen);
    }
    lan_address().map(|(ip, _)| SocketAddr::new(ip.into(), listen.port()))
}

pub fn subnet_of(ip: Ipv4Addr, netmask: Ipv4Addr) -> String {
    let mask = u32::from(netmask);
    format!("{}/{}", Ipv4Addr::from(u32::from(ip) & mask), mask.count_ones())
}

/// The hardware address of the default gateway, i.e. the router this machine is on now.
pub fn router_mac() -> Option<[u8; 6]> {
    gateway_mac(default_gateway()?)
}

#[cfg(target_os = "linux")]
fn default_gateway() -> Option<Ipv4Addr> {
    parse_proc_route(&std::fs::read_to_string("/proc/net/route").ok()?)
}

#[cfg(target_os = "linux")]
fn gateway_mac(gateway: Ipv4Addr) -> Option<[u8; 6]> {
    parse_proc_arp(&std::fs::read_to_string("/proc/net/arp").ok()?, gateway)
}

#[cfg(target_os = "macos")]
fn default_gateway() -> Option<Ipv4Addr> {
    let output = std::process::Command::new("/sbin/route").args(["-n", "get", "default"]).output().ok()?;
    let output = String::from_utf8_lossy(&output.stdout);
    output.lines().find_map(|line| line.trim().strip_prefix("gateway:")?.trim().parse().ok())
}

#[cfg(target_os = "macos")]
fn gateway_mac(gateway: Ipv4Addr) -> Option<[u8; 6]> {
    let output = std::process::Command::new("/usr/sbin/arp").args(["-n", &gateway.to_string()]).output().ok()?;
    // "? (192.168.1.1) at aa:bb:cc:dd:ee:ff on en0 ifscope [ethernet]"
    let output = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = output.split_whitespace().collect();
    if fields.get(2) != Some(&"at") {
        return None;
    }
    parse_mac(fields.get(3)?)
}

/// Finds the default route in `/proc/net/route`. Addresses there are the raw
/// network-order bytes printed as a host-order hex integer. VPNs often add
/// 0.0.0.0/1 routes, which share the default's destination but not its mask.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_route(table: &str) -> Option<Ipv4Addr> {
    const RTF_GATEWAY: u32 = 0x2;
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let flags = u32::from_str_radix(fields.get(3)?, 16).ok()?;
        if *fields.get(1)? != "00000000" || *fields.get(7)? != "00000000" || flags & RTF_GATEWAY == 0 {
            return None;
        }
        Some(Ipv4Addr::from(u32::from_str_radix(fields[2], 16).ok()?.to_ne_bytes()))
    })
}

/// Looks up `gateway` in `/proc/net/arp`. Incomplete entries show an all-zero address.
#[cfg(any(target_os = "linux", test))]
fn parse_proc_arp(table: &str, gateway: Ipv4Addr) -> Option<[u8; 6]> {
    let gateway = gateway.to_string();
    table.lines().skip(1).find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() != Some(&gateway.as_str()) {
            return None;
        }
        parse_mac(fields.get(3)?).filter(|mac| *mac != [0; 6])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_matches_arp_output_without_leading_zeros() {
        assert_eq!(parse_mac("a:1b:c:dd:e:ff"), parse_mac("0A:1B:0C:DD:0E:FF"));
        assert_eq!(format_mac(parse_mac("a:1b:c:dd:e:ff").unwrap()), "0a:1b:0c:dd:0e:ff");
        assert_eq!(parse_mac("aa:bb:cc:dd:ee"), None);
        assert_eq!(parse_mac("aa:bb:cc:dd:ee:zz"), None);
    }

    #[test]
    fn home_subnet_masks_host_bits() {
        let home = HomeNetwork::parse("192.168.1.5/24", "aa:bb:cc:dd:ee:ff").unwrap();
        assert!(home.contains(Ipv4Addr::new(192, 168, 1, 77)));
        assert!(!home.contains(Ipv4Addr::new(192, 168, 2, 77)));
        assert!(HomeNetwork::parse("192.168.1.0", "aa:bb:cc:dd:ee:ff").is_err());
        assert!(HomeNetwork::parse("192.168.1.0/33", "aa:bb:cc:dd:ee:ff").is_err());
        assert_eq!(subnet_of(Ipv4Addr::new(10, 0, 5, 9), Ipv4Addr::new(255, 255, 252, 0)), "10.0.4.0/22");
    }

    #[test]
    fn judges_peers_before_the_router_lookup() {
        let home = HomeNetwork::parse("192.168.1.0/24", "aa:bb:cc:dd:ee:ff").unwrap();
        let lan: IpAddr = "192.168.1.40".parse().unwrap();
        let tailnet: IpAddr = "100.64.0.5".parse().unwrap();
        let judge = |peer: &str, local: IpAddr| home.judge(peer.parse().unwrap(), local);
        assert_eq!(judge("127.0.0.1", lan), Verdict::Allow);
        assert_eq!(judge("::ffff:127.0.0.1", lan), Verdict::Allow);
        assert_eq!(judge("192.168.1.77", lan), Verdict::AllowIfHome);
        assert_eq!(judge("::ffff:192.168.1.77", lan), Verdict::AllowIfHome);
        assert_eq!(judge("10.0.0.3", lan), Verdict::Refuse);
        // eco listening on a Tailscale address still reaches itself.
        assert_eq!(judge("100.64.0.5", tailnet), Verdict::Allow);
    }

    #[test]
    fn reads_linux_default_gateway_and_its_mac() {
        let route = "Iface\tDestination\tGateway \tFlags\tRefCnt\tUse\tMetric\tMask\t\tMTU\tWindow\tIRTT\n\
                     tun0\t00000000\t0100080A\t0003\t0\t0\t0\t00000080\t0\t0\t0\n\
                     eth0\t0001A8C0\t00000000\t0001\t0\t0\t100\t00FFFFFF\t0\t0\t0\n\
                     eth0\t00000000\t0101A8C0\t0003\t0\t0\t100\t00000000\t0\t0\t0\n";
        let gateway = parse_proc_route(route).unwrap();
        // The table holds network-order bytes, so only little-endian hosts read it as below.
        if cfg!(target_endian = "little") {
            assert_eq!(gateway, Ipv4Addr::new(192, 168, 1, 1));
        }
        let arp = "IP address       HW type     Flags       HW address            Mask     Device\n\
                   192.168.1.7      0x1         0x0         00:00:00:00:00:00     *        eth0\n\
                   192.168.1.1      0x1         0x2         aa:bb:cc:dd:ee:ff     *        eth0\n";
        assert_eq!(parse_proc_arp(arp, Ipv4Addr::new(192, 168, 1, 1)), parse_mac("aa:bb:cc:dd:ee:ff"));
        assert_eq!(parse_proc_arp(arp, Ipv4Addr::new(192, 168, 1, 7)), None);
    }
}
