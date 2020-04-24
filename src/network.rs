use heim::net::{Address, MacAddr, Nic};

use slog::{debug, info, o, Logger};

use std::{
    borrow::Cow,
    collections::{btree_map::Entry, BTreeMap},
    fmt::Debug,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
};

/// Unpack a heim `Address` which is assumed to be a link-layer address
fn unwrap_link_address(address: Address) -> MacAddr {
    if let Address::Link(mac_addr) = address {
        mac_addr
    } else {
        unreachable!("Expected a link-layer address")
    }
}

/// Unpack a heim `Address` which is assumed to be an IPv4 address
fn unwrap_ipv4_address(address: Address) -> Ipv4Addr {
    if let Address::Inet(SocketAddr::V4(ipv4_sock_addr)) = address {
        assert_eq!(ipv4_sock_addr.port(), 0, "Expected an IP address");
        *ipv4_sock_addr.ip()
    } else {
        unreachable!("Expected an IPv4 address")
    }
}

/// Unpack a heim `Address` which is assumed to be an IPv6 address
fn unwrap_ipv6_address(address: Address) -> Ipv6Addr {
    // FIXME: heim puts IPv6 addresses in an `Inet` wrapper, even though there
    //        is an `Inet6` wrapper. It probably shouldn't do that.
    if let Address::Inet(SocketAddr::V6(ipv6_sock_addr))
    | Address::Inet6(SocketAddr::V6(ipv6_sock_addr)) = address
    {
        assert_eq!(ipv6_sock_addr.port(), 0, "Expected an IP address");
        *ipv6_sock_addr.ip()
    } else {
        unreachable!("Expected an IPv6 address")
    }
}

/// Global properties of a network interface card (according to ifconfig)
#[derive(Debug, Default)]
struct InterfaceProperties {
    // These flags are available everywhere
    is_up: bool,
    is_loopback: bool,
    is_multicast: bool,

    // These flags may not always be available on some OSes
    link_type: Option<LinkType>,

    // A network interface should only have one link-layer address, which
    // may or may not be reported by the underlying system API.
    link_address: Option<AddressProperties<MacAddr>>,

    // A network interface may have multiple network-layer addresses
    ipv4_addresses: Vec<AddressProperties<Ipv4Addr>>,
    ipv6_addresses: Vec<AddressProperties<Ipv6Addr>>,
}

impl InterfaceProperties {
    /// Fill up global interface properties using the first heim Nic struct
    /// that was observed for this interface.
    pub fn new(interface: Nic) -> Self {
        // Record the basic interface-wide properties
        let mut result = InterfaceProperties {
            is_up: interface.is_up(),
            is_loopback: interface.is_loopback(),
            is_multicast: interface.is_multicast(),
            link_type: LinkType::check(&interface),
            ..Self::default()
        };

        // Register the inner address of the input Nic struct
        result.add_address(interface);

        // Emit the resulting interface properties record
        result
    }

    /// Register a new address of this interface
    pub fn add_address(&mut self, interface: Nic) {
        // Make sure the interface-wide properties remain consistent
        const BAD_STAT: &str = "Reported NIC status is inconsistent";
        assert_eq!(self.is_up, interface.is_up(), "{}", BAD_STAT);
        assert_eq!(self.is_loopback, interface.is_loopback(), "{}", BAD_STAT);
        assert_eq!(self.is_multicast, interface.is_multicast(), "{}", BAD_STAT);

        // In the case of link type, new info can emerge
        match (self.link_type, LinkType::check(&interface)) {
            // If we already had some info, check new one for consistency
            (Some(old), Some(new)) => assert_eq!(old, new, "{}", BAD_STAT),

            // If we didn't have some info, record new one as it comes
            (None, new @ Some(_)) => self.link_type = new,

            // If no new info is incoming, statu quo is always consistent
            (_, None) => {}
        }

        // Register a new interface address of the right type
        match interface.address() {
            // Process interface link address (should be unique)
            Address::Link(mac_address) => {
                assert_eq!(self.link_address, None, "Link address should be unique");
                assert_eq!(interface.netmask(), None, "No netmasks at link layer");
                assert_eq!(interface.destination(), None, "No dests at link layer");
                self.link_address = Some(AddressProperties::new(
                    interface,
                    mac_address,
                    unwrap_link_address,
                ));
            }

            // Process IPv4 interface address
            Address::Inet(SocketAddr::V4(ipv4_sock_addr)) => {
                assert_eq!(ipv4_sock_addr.port(), 0, "Expected an IP address");
                self.ipv4_addresses.push(AddressProperties::new(
                    interface,
                    *ipv4_sock_addr.ip(),
                    unwrap_ipv4_address,
                ));
            }

            // Process IPv6 interface address
            //
            // FIXME: Put Inet(V6) version back in the unreachable match arm
            //        once heim resolves the bug that lets this case happen.
            //
            Address::Inet(SocketAddr::V6(ipv6_sock_addr))
            | Address::Inet6(SocketAddr::V6(ipv6_sock_addr)) => {
                assert_eq!(ipv6_sock_addr.port(), 0, "Expected an IP address");
                self.ipv6_addresses.push(AddressProperties::new(
                    interface,
                    *ipv6_sock_addr.ip(),
                    unwrap_ipv6_address,
                ));
            }

            // These combinations don't make sense, the heim API probably
            // shouldn't allow them to occur.
            Address::Inet6(SocketAddr::V4(_)) => unreachable!(
                "Received an IP address with an inconsistent type {:?}",
                interface.address()
            ),

            // Can't use an exhaustive match, per heim design choice
            _ => panic!("Unsupported network interface address type"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkType {
    /// Broadcast link
    Broadcast,

    /// Point-to-point link
    PointToPoint,

    /// Neither broadcast nor point-to-point (interface is most likely down)
    Neither,
}

impl LinkType {
    /// Try to check the link type of a heim Nic
    #[allow(unused_assignments)]
    pub fn check(interface: &Nic) -> Option<Self> {
        // On Linux, we have a precise way of checking the interface link type
        #[cfg(target_os = "linux")]
        {
            use heim::net::os::linux::NicExt;
            let link_type = match (interface.is_broadcast(), interface.is_point_to_point()) {
                (true, true) => unreachable!(
                    "A NIC cannot simultaneously operate in broadcast mode and \
                     in point-to-point mode"
                ),
                (true, false) => LinkType::Broadcast,
                (false, true) => LinkType::PointToPoint,
                (false, false) => LinkType::Neither,
            };
            Some(link_type)
        }

        // On other platforms, we can only check if there is a destination
        // address, which tells us that the link is point-to-point.
        #[cfg(not(target_os = "linux"))]
        {
            interface.destination.map(|_dest| LinkType::PointToPoint)
        }
    }
}

/// Properties which are specific to a given address of a network interface
#[derive(Debug, Eq, PartialEq)]
struct AddressProperties<AddressType> {
    /// Address of a network interface
    address: AddressType,

    /// Associated subnet mask (if any)
    netmask: Option<AddressType>,

    /// Associated broadcast or point-to-point destination address (if any)
    target: Option<AddressType>,
}

impl<AddressType> AddressProperties<AddressType> {
    /// Collect properties of a heim Nic, given 1/the pre-decoded network
    /// address of this Nic and 2/a way to decode other addresses from the Nic
    /// struct, asserting that they use the same format.
    fn new(
        interface: Nic,
        address: AddressType,
        mut unwrap_address: impl FnMut(Address) -> AddressType,
    ) -> Self {
        // Collect the netmask (if any)
        let netmask = interface.netmask().map(&mut unwrap_address);

        // Collect the point-to-point destination address (if any)
        let mut target = interface.destination().map(&mut unwrap_address);

        // If on linux...
        #[cfg(target_os = "linux")]
        {
            use heim::net::os::linux::NicExt;

            // Check the broadcast address
            let broadcast = interface.broadcast();

            // If a destination was set...
            if target.is_some() {
                // Make sure we're in point-to-point mode
                assert!(
                    interface.is_point_to_point(),
                    "Network interface claims not to operate in point-to-point \
                     mode, but it has a destination address"
                );
                // Make sure no broadcast address is set
                assert_eq!(
                    broadcast, None,
                    "Network interface claims to operate in point-to-point \
                     mode, but it has a broadcast address."
                );
            } else if broadcast.is_some() {
                // If a broadcast is set, make sure we're in broadcast mode
                assert!(
                    interface.is_broadcast(),
                    "Network interface claims not to operate in broadcast \
                     mode, but it has a broadcast address"
                );
                target = broadcast.map(unwrap_address);
            }
        }

        // Emit pre-digested address properties
        AddressProperties {
            address,
            netmask,
            target,
        }
    }
}

/// Report on the host's network connections
pub fn startup_report(log: &Logger, network_interfaces: Vec<Nic>) {
    // The heim Nic API mixes together global network interface properties and
    // network interface properties, which isn't very ergonomic. We'll start by
    // producing a more structured and less redundant summary.
    debug!(log, "Processing network interface list...");
    let mut name_to_properties = BTreeMap::<String, InterfaceProperties>::new();
    for interface in network_interfaces {
        // Create or update the corresponding network interface record
        let name = interface.name().to_owned();
        match name_to_properties.entry(name) {
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(InterfaceProperties::new(interface));
            }
            Entry::Occupied(occupied_entry) => {
                occupied_entry.into_mut().add_address(interface);
            }
        }
    }

    // Now it's time to report on the network interfaces that we observed
    for (name, interface) in name_to_properties {
        let nic_log = log.new(o!("interface name" => name));

        // Report status flags
        let link_type_str: Cow<str> = match interface.link_type {
            None => "Unknown".into(),
            Some(LinkType::Neither) => "None".into(),
            Some(link_type) => format!("{:?}", link_type).into(),
        };
        info!(nic_log, "Found a network interface";
              "up" => interface.is_up,
              "loopback" => interface.is_loopback,
              "multicast" => interface.is_multicast,
              "link type" => %link_type_str);

        // Report link address, if any
        if let Some(link_address_props) = interface.link_address {
            assert_eq!(
                link_address_props.netmask, None,
                "Link-layer addresses shouldn't have subnet masks"
            );
            let broadcast_str: Cow<str> = match link_address_props.target {
                Some(addr) => addr.to_string().into(),
                None => "None".into(),
            };
            info!(nic_log, "Got a link-layer address";
                  "address" => %link_address_props.address,
                  "broadcast" => %broadcast_str);
        }

        // General mechanism to print out IP-layer targets (bcast/dest)
        fn print_ip_target<Addr>(target: Option<Addr>) -> Cow<'static, str>
        where
            Addr: Debug,
        {
            match target {
                Some(addr) => format!("{:?}", addr).into(),
                None => "None".into(),
            }
        }

        // Report IPv4 addresses
        for ipv4_address_props in interface.ipv4_addresses {
            let netmask = ipv4_address_props
                .netmask
                .expect("IP addresses should have a subnet mask");
            info!(nic_log, "Got an IPv4 address";
                  "address" => ?ipv4_address_props.address,
                  "netmask" => ?netmask,
                  "bcast/dest" => %print_ip_target(ipv4_address_props.target));
        }

        // Report IPv6 addresses
        for ipv6_address_props in interface.ipv6_addresses {
            let netmask = ipv6_address_props
                .netmask
                .expect("IP addresses should have a subnet mask");
            info!(nic_log, "Got an IPv6 address";
                  "address" => ?ipv6_address_props.address,
                  "netmask" => ?netmask,
                  "bcast/dest" => %print_ip_target(ipv6_address_props.target));
        }
    }
}
