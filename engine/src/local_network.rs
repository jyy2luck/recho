use std::net::{IpAddr, Ipv4Addr, UdpSocket};

pub fn proxy_addresses() -> (Ipv4Addr, Vec<Ipv4Addr>) {
    let interfaces = if_addrs::get_if_addrs().unwrap_or_default();
    // UDP connect only asks the OS for a route; no datagram is sent.
    let routed = (|| {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
        match socket.local_addr().ok()?.ip() {
            IpAddr::V4(address) => Some(address),
            IpAddr::V6(_) => None,
        }
    })();
    let preferred = preferred_address(&interfaces, routed);
    (preferred, selectable_addresses(&interfaces, preferred))
}

fn interface_address(interface: &if_addrs::Interface) -> Option<Ipv4Addr> {
    match interface.ip() {
        IpAddr::V4(address)
            if interface.is_oper_up()
                && !address.is_loopback()
                && !address.is_unspecified()
                && !address.is_link_local()
                && !address.is_multicast()
                && !address.is_broadcast() =>
        {
            Some(address)
        }
        _ => None,
    }
}

fn selectable_addresses(interfaces: &[if_addrs::Interface], preferred: Ipv4Addr) -> Vec<Ipv4Addr> {
    let mut addresses: Vec<_> = interfaces.iter().filter_map(interface_address).collect();
    addresses.sort_unstable();
    addresses.dedup();
    if !addresses.contains(&preferred) {
        addresses.push(preferred);
    }
    addresses
}

fn preferred_address(interfaces: &[if_addrs::Interface], routed: Option<Ipv4Addr>) -> Ipv4Addr {
    let addresses: Vec<_> = interfaces
        .iter()
        .filter(|interface| !interface.is_p2p())
        .filter_map(|interface| {
            interface_address(interface).map(|address| (address, is_hardware(interface)))
        })
        .collect();
    select_address(&addresses, routed)
}

#[cfg(windows)]
fn is_hardware(interface: &if_addrs::Interface) -> bool {
    use windows_sys::Win32::NetworkManagement::IpHelper::{GetIfEntry2, MIB_IF_ROW2};
    let Some(index) = interface.index else {
        return false;
    };
    let mut row = MIB_IF_ROW2 {
        InterfaceIndex: index,
        ..Default::default()
    };
    // HardwareInterface is the first bit; TAP adapters can report an Ethernet type.
    unsafe { GetIfEntry2(&mut row) == 0 && row.InterfaceAndOperStatusFlags._bitfield & 1 != 0 }
}

#[cfg(not(windows))]
fn is_hardware(_: &if_addrs::Interface) -> bool {
    true
}

fn select_address(candidates: &[(Ipv4Addr, bool)], routed: Option<Ipv4Addr>) -> Ipv4Addr {
    let has_hardware = candidates.iter().any(|(_, hardware)| *hardware);
    let addresses: Vec<_> = candidates
        .iter()
        .filter(|(_, hardware)| *hardware == has_hardware)
        .map(|(address, _)| *address)
        .collect();
    routed
        .filter(|address| addresses.contains(address))
        .or_else(|| addresses.iter().copied().find(Ipv4Addr::is_private))
        .or_else(|| addresses.first().copied())
        .unwrap_or(Ipv4Addr::LOCALHOST)
}

#[cfg(test)]
mod tests {
    use super::*;
    use if_addrs::{IfAddr, IfOperStatus, Ifv4Addr, Interface};

    fn interface(address: &str) -> Interface {
        Interface {
            name: "test".into(),
            addr: IfAddr::V4(Ifv4Addr {
                ip: address.parse().unwrap(),
                netmask: Ipv4Addr::new(255, 255, 255, 0),
                prefixlen: 24,
                broadcast: None,
            }),
            index: None,
            oper_status: IfOperStatus::Up,
            is_p2p: false,
            #[cfg(windows)]
            adapter_name: "test".into(),
        }
    }

    #[test]
    fn prefers_routed_active_address_then_private_and_refreshes() {
        let mut interfaces = vec![interface("203.0.113.2"), interface("192.168.31.230")];
        let lan = interfaces[1].ip().to_string().parse().unwrap();
        assert_eq!(preferred_address(&interfaces, Some(lan)), lan);
        assert_eq!(preferred_address(&interfaces, None), lan);
        let routed = "203.0.113.2".parse().unwrap();
        assert_eq!(preferred_address(&interfaces, Some(routed)), routed);
        interfaces[1] = interface("192.168.2.3");
        assert_eq!(
            preferred_address(&interfaces, Some(lan)),
            "192.168.2.3".parse::<Ipv4Addr>().unwrap()
        );
        assert_eq!(preferred_address(&interfaces[..1], None), routed);
    }

    #[test]
    fn excludes_unusable_interfaces_and_falls_back_to_loopback() {
        let mut interfaces: Vec<_> = [
            "127.0.0.1",
            "0.0.0.0",
            "169.254.1.2",
            "224.0.0.1",
            "255.255.255.255",
        ]
        .map(interface)
        .into();
        let mut down = interface("192.168.1.2");
        down.oper_status = IfOperStatus::Down;
        interfaces.push(down);
        assert_eq!(
            selectable_addresses(&interfaces, Ipv4Addr::LOCALHOST),
            [Ipv4Addr::LOCALHOST]
        );
        let mut tunnel = interface("10.0.0.2");
        tunnel.is_p2p = true;
        interfaces.push(tunnel);
        assert_eq!(
            preferred_address(&interfaces, Some("10.0.0.2".parse().unwrap())),
            Ipv4Addr::LOCALHOST
        );
        assert_eq!(preferred_address(&[], None), Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn selectable_addresses_include_virtual_interfaces_once() {
        let mut interfaces = vec![
            interface("192.168.31.230"),
            interface("172.16.3.18"),
            interface("192.168.31.230"),
            interface("127.0.0.1"),
        ];
        interfaces[1].is_p2p = true;
        assert_eq!(
            selectable_addresses(&interfaces, "192.168.31.230".parse().unwrap()),
            [
                "172.16.3.18".parse::<Ipv4Addr>().unwrap(),
                "192.168.31.230".parse().unwrap()
            ]
        );
        assert_eq!(
            selectable_addresses(&[], Ipv4Addr::LOCALHOST),
            [Ipv4Addr::LOCALHOST]
        );
        let tunnels = &interfaces[1..2];
        let preferred = preferred_address(tunnels, None);
        assert_eq!(preferred, Ipv4Addr::LOCALHOST);
        assert_eq!(
            selectable_addresses(tunnels, preferred),
            ["172.16.3.18".parse::<Ipv4Addr>().unwrap(), preferred]
        );
    }

    #[test]
    fn prefers_physical_lan_over_tap_route_but_supports_virtual_only_hosts() {
        let tap = "172.16.3.18".parse().unwrap();
        let wifi = "192.168.31.230".parse().unwrap();
        let ethernet = "10.0.1.2".parse().unwrap();
        let addresses = [(tap, false), (ethernet, true), (wifi, true)];
        assert_eq!(select_address(&addresses, Some(tap)), ethernet);
        assert_eq!(select_address(&addresses, Some(wifi)), wifi);
        assert_eq!(select_address(&addresses[..1], Some(tap)), tap);
    }
}
