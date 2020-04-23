// FIXME: I probably need to have a word with the heim dev about this
#![type_length_limit = "20000000"]

mod cpu;
mod memory;
mod process;

use futures_util::{
    future::TryFutureExt,
    stream::{StreamExt, TryStreamExt},
    try_join,
};

use heim::{
    disk::{Partition, Usage},
    host::{Pid, Platform, User},
    net::{Address, MacAddr, Nic},
    sensors::TemperatureSensor,
    units::{
        information::byte,
        thermodynamic_temperature::degree_celsius,
        Information, ThermodynamicTemperature as Temperature,
    },
    virt::Virtualization,
};

use slog::{debug, info, o, warn, Drain, Logger};

use std::{
    collections::{btree_map::Entry, BTreeMap},
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Mutex,
};

#[async_std::main]
async fn main() -> heim::Result<()> {
    // Set up a logger
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build();
    let drain = Mutex::new(drain).fuse();
    let log = slog::Logger::root(drain, o!("benchmon version" => env!("CARGO_PKG_VERSION")));

    // Ask heim to start fetching all the system info we need...
    info!(log, "Probing host system characteristics...");
    // - CPU info
    let global_cpu_freq = heim::cpu::frequency();
    #[cfg(target_os = "linux")]
    let per_cpu_freqs = heim::cpu::os::linux::frequencies()
        .try_collect::<Vec<_>>()
        .map_ok(Some);
    #[cfg(not(target_os = "linux"))]
    let per_cpu_freqs = futures_util::future::ok(None);
    let logical_cpus = heim::cpu::logical_count();
    let physical_cpus = heim::cpu::physical_count();
    // - Memory info
    let memory = heim::memory::memory();
    let swap = heim::memory::swap();
    // - Filesystem info
    let disk_partitions_and_usage = heim::disk::partitions()
        .and_then(|partition| async {
            // NOTE: Failure to stat a partition is purposely treated as a
            //       non-fatal event, unlike all other failures, as it happens
            //       on random pseudo-filesystems that no one cares about.
            let usage_result = heim::disk::usage(partition.mount_point()).await;
            Ok((partition, usage_result))
        })
        .try_collect::<Vec<_>>();
    // - Network info
    let network_interfaces = heim::net::nic().try_collect::<Vec<_>>();
    // - Sensor info
    let temperatures = heim::sensors::temperatures().try_collect::<Vec<_>>();
    // - Platform info (= OS info + CPU architecture)
    let platform = heim::host::platform();
    // - User connexion info
    let user_connections = heim::host::users().try_collect::<Vec<_>>();
    // - Virtualization info
    let virt = heim::virt::detect();
    // - Initial processes info
    let processes = heim::process::processes()
        .then(process::get_process_info)
        .try_collect::<Vec<_>>();

    // Report CPU configuration
    let (platform, logical_cpus, physical_cpus, global_cpu_freq, per_cpu_freqs) = try_join!(
        platform,
        logical_cpus,
        physical_cpus,
        global_cpu_freq,
        per_cpu_freqs
    )?;
    cpu::startup_report(
        &log,
        platform.architecture(),
        logical_cpus,
        physical_cpus,
        global_cpu_freq,
        per_cpu_freqs,
    );

    // Report memory configuration
    let (memory, swap) = try_join!(memory, swap)?;
    memory::startup_report(&log, memory, swap);

    // Report filesystem configuration
    let disk_partitions_and_usage = disk_partitions_and_usage.await?;
    report_filesystem(&log, disk_partitions_and_usage);

    // Report network configuration
    let network_interfaces = network_interfaces.await?;
    report_network(&log, network_interfaces);

    // Report temperature sensor configuration
    let temperatures = temperatures.await?;
    report_temp_sensors(&log, temperatures);

    // Report operating system and use of virtualization
    let virt = virt.await;
    report_os(&log, platform, virt);

    // Report open user sessions
    let user_connections = user_connections.await?;
    report_users(&log, user_connections);

    // Report running processes
    let processes = processes.await?;
    process::startup_report(&log, processes);

    // TODO: Extract this system summary to a separate async fn, then start
    //       polling useful "dynamic" quantities in a system monitor like
    //       fashion. Try to mimick dstat's tabular output.
    // TODO: Once we have a good system monitor, start using it to monitor
    //       execution of some benchmark. Measure baseline before starting
    //       benchmark execution. Also monitor child getrusage() during process
    //       execution, and wall-clock execution time.
    // TODO: After end of benchmark execution, produce tabular data sets for
    //       manual inspection to begin with, and later implement direct
    //       support for fancy plots (with plotters? plotly?)
    // TODO: Add a way to selectively enable/disable stats.

    Ok(())
}

// Report on the host's file system configuration
fn report_filesystem(
    log: &Logger,
    disk_partitions_and_usage: Vec<(Partition, heim::Result<Usage>)>,
) {
    debug!(log, "Processing filesystem mount list...");
    let mut dev_to_mounts = BTreeMap::<_, Vec<_>>::new();
    for (partition, usage) in disk_partitions_and_usage {
        // Use disk capacity and disk usage (if available) as a last-resort
        // disambiguation key for mounts with identical device name and size
        // (e.g. unrelated tmpfs mounts on Linux).
        let known_used_bytes = usage
            .as_ref()
            .map(|usage| usage.used().get::<byte>())
            .unwrap_or(0);
        let capacity = usage.map(|usage| usage.total().clone());

        // Need to eagerly format device stats as otherwise they can't be used
        // as BTreeMap keys... which is kind of sad.
        let formatted_device = if let Some(device) = partition.device() {
            device.to_string_lossy().into_owned()
        } else {
            "none".to_owned()
        };
        let formatted_capacity = match capacity {
            Ok(capacity) => format_information(capacity),
            Err(err) => format!("Unavailable ({})", err),
        };
        let formatted_filesystem = partition.file_system().as_str().to_owned();

        // Group/sort mount points by sorted device name, then capacity, then
        // filesystem, and finally our hidden used storage disambiguation key.
        let mount_list = dev_to_mounts
            .entry((
                formatted_device,
                formatted_capacity,
                formatted_filesystem,
                known_used_bytes,
            ))
            .or_default();
        mount_list.push(partition.mount_point().to_owned());
    }

    for ((device, capacity, file_system, _used_bytes), mut mount_list) in dev_to_mounts {
        mount_list.sort();
        info!(log, "Found a mounted device";
              "device name" => device,
              "capacity" => capacity,
              "file system" => file_system,
              "mount point(s)" => ?mount_list);
    }
}

fn report_network(log: &Logger, network_interfaces: Vec<Nic>) {
    // TODO: Break this down in multiple functions during modularization
    // TODO: Consider exposing the data later on

    // Address of whatever the interface is plugged into
    #[derive(Debug, Eq, PartialEq)]
    enum TargetAddress<AddressType> {
        /// Broadcast address
        Broadcast(AddressType),

        /// Point-to-point destination address
        PointToPoint(AddressType),
    }

    // Address-specific properties
    #[derive(Debug, Eq, PartialEq)]
    struct AddressProperties<AddressType> {
        /// Address of a network interface
        address: AddressType,

        /// Associated subnet mask (if any)
        netmask: Option<AddressType>,

        /// Associated broadcast or destination address (if any)
        target: Option<TargetAddress<AddressType>>,
    }

    // Interface-wide stats (according to ifconfig)
    #[derive(Debug, Default)]
    struct InterfaceProperties {
        // These flags are available everywhere
        is_up: bool,
        is_loopback: bool,
        is_multicast: bool,

        // These flags are only available on some operating systems
        is_broadcast: Option<bool>,
        is_point_to_point: Option<bool>,

        // A network interface should only have one link-layer address, which
        // may or may not be reported by the underlying system API.
        link_address: Option<AddressProperties<MacAddr>>,

        // A network interface may have multiple network-layer addresses
        ipv4_addresses: Vec<AddressProperties<Ipv4Addr>>,
        ipv6_addresses: Vec<AddressProperties<Ipv6Addr>>,
    }

    debug!(log, "Processing network interface list...");
    let mut name_to_properties = BTreeMap::<String, InterfaceProperties>::new();
    for interface in network_interfaces {
        // Create interface record or check its consistency
        let name = interface.name().to_owned();
        let interface_properties = match name_to_properties.entry(name) {
            // Create interface record if it doesn't exist
            Entry::Vacant(entry) => {
                let properties = entry.insert(InterfaceProperties {
                    is_up: interface.is_up(),
                    is_loopback: interface.is_loopback(),
                    is_multicast: interface.is_multicast(),
                    is_broadcast: None,
                    is_point_to_point: None,
                    link_address: None,
                    ipv4_addresses: Vec::new(),
                    ipv6_addresses: Vec::new(),
                });
                #[cfg(target_os = "linux")]
                {
                    use heim::net::os::linux::NicExt;
                    assert!(
                        !(interface.is_broadcast() && interface.is_point_to_point()),
                        "A network interface should not be able to operate in \
                         broadcast and point-to-point mode simultaneously"
                    );
                    properties.is_broadcast = Some(interface.is_broadcast());
                    properties.is_point_to_point = Some(interface.is_point_to_point());
                }
                properties
            }
            // Check consistency of existing interface record flags
            Entry::Occupied(prop_entry) => {
                let properties = prop_entry.into_mut();
                const INCONSISTENT_STATUS_ERROR: &str =
                    "Reported network interface status flags are inconsistent";
                assert_eq!(
                    properties.is_up,
                    interface.is_up(),
                    "{}",
                    INCONSISTENT_STATUS_ERROR
                );
                assert_eq!(
                    properties.is_loopback,
                    interface.is_loopback(),
                    "{}",
                    INCONSISTENT_STATUS_ERROR
                );
                assert_eq!(
                    properties.is_multicast,
                    interface.is_multicast(),
                    "{}",
                    INCONSISTENT_STATUS_ERROR
                );
                #[cfg(target_os = "linux")]
                {
                    use heim::net::os::linux::NicExt;
                    assert_eq!(
                        properties.is_broadcast,
                        Some(interface.is_broadcast()),
                        "{}",
                        INCONSISTENT_STATUS_ERROR
                    );
                    assert_eq!(
                        properties.is_point_to_point,
                        Some(interface.is_point_to_point()),
                        "{}",
                        INCONSISTENT_STATUS_ERROR
                    );
                }
                properties
            }
        };

        // Helper function to deduplicate interface address enumeration code
        fn build_address_properties<AddressType>(
            interface: Nic,
            address: AddressType,
            mut unwrap_address: impl FnMut(Address) -> AddressType,
        ) -> AddressProperties<AddressType> {
            // Assert that netmask (if any) uses same address format
            let netmask = interface.netmask().map(&mut unwrap_address);

            // Assert that destination (if any) uses same address format and
            // take note of the fact that this is a point-to-point concept
            let mut target = interface
                .destination()
                .map(&mut unwrap_address)
                .map(TargetAddress::PointToPoint);

            // If on linux...
            #[cfg(target_os = "linux")]
            {
                use heim::net::os::linux::NicExt;

                // Check broadcast address
                let broadcast = interface.broadcast();

                // If a destination is set...
                if target.is_some() {
                    // Make sure we're in point-to-point mode
                    assert!(
                        interface.is_point_to_point(),
                        "Network interface claims not to operate in \
                         point-to-point mode, but has a destination address"
                    );
                    // Make sure no broadcast address is set
                    assert_eq!(
                        broadcast, None,
                        "Network interface claims to operate in \
                         point-to-point mode, but has a broadcast address."
                    );
                } else if broadcast.is_some() {
                    // If a broadcast is set, make sure we're in broadcast mode
                    assert!(
                        interface.is_broadcast(),
                        "Network interface claims not to operate in \
                         broadcast mode, but has a broadcast address"
                    );
                    target = broadcast.map(unwrap_address).map(TargetAddress::Broadcast);
                }
            }

            // Emit pre-digested address properties
            AddressProperties {
                address,
                netmask,
                target,
            }
        }

        match interface.address() {
            // Process interface link address (should be unique)
            Address::Link(mac_address) => {
                assert_eq!(
                    interface_properties.link_address, None,
                    "A network interface should only have one link address"
                );
                let unwrap_link_address = |address: Address| -> MacAddr {
                    if let Address::Link(mac_addr) = address {
                        mac_addr
                    } else {
                        unreachable!("Expected a link-layer address")
                    }
                };
                interface_properties.link_address = Some(build_address_properties(
                    interface,
                    mac_address,
                    unwrap_link_address,
                ));
            }

            // Process IPv4 interface address
            Address::Inet(SocketAddr::V4(ipv4_sock_addr)) => {
                assert_eq!(ipv4_sock_addr.port(), 0, "Expected an IP address");
                let unwrap_ipv4_address = |address: Address| -> Ipv4Addr {
                    if let Address::Inet(SocketAddr::V4(ipv4_sock_addr)) = address {
                        assert_eq!(ipv4_sock_addr.port(), 0, "Expected an IP address");
                        *ipv4_sock_addr.ip()
                    } else {
                        unreachable!("Expected an IPv4 address")
                    }
                };
                interface_properties
                    .ipv4_addresses
                    .push(build_address_properties(
                        interface,
                        *ipv4_sock_addr.ip(),
                        unwrap_ipv4_address,
                    ));
            }

            // Process IPv6 interface address
            //
            // FIXME: Put Inet(V6) version back in the unreachable match arm
            //        once heim resolves the bug that lets this case happen.
            Address::Inet(SocketAddr::V6(ipv6_sock_addr))
            | Address::Inet6(SocketAddr::V6(ipv6_sock_addr)) => {
                assert_eq!(
                    ipv6_sock_addr.port(),
                    0,
                    "Expected an internet-layer address"
                );
                let unwrap_ipv6_address = |address: Address| -> Ipv6Addr {
                    // FIXME: See above
                    if let Address::Inet(SocketAddr::V6(ipv6_sock_addr))
                    | Address::Inet6(SocketAddr::V6(ipv6_sock_addr)) = address
                    {
                        assert_eq!(ipv6_sock_addr.port(), 0, "Expected an IP address");
                        *ipv6_sock_addr.ip()
                    } else {
                        unreachable!("Expected an IPv6 address")
                    }
                };
                interface_properties
                    .ipv6_addresses
                    .push(build_address_properties(
                        interface,
                        *ipv6_sock_addr.ip(),
                        unwrap_ipv6_address,
                    ));
            }

            // These combinations don't make sense, the heim API probably
            // shouldn't allow them to occur.
            Address::Inet6(SocketAddr::V4(_)) => unreachable!(
                "Received IP address with an inconsistent type {:?}",
                interface.address()
            ),

            // Can't use an exhaustive match, per heim design choice
            _ => panic!("Unsupported network interface address type"),
        }
    }

    // Now it's time to report on the interfaces that we observed
    for (name, interface) in name_to_properties {
        let nic_log = log.new(o!("interface name" => name));

        // Report status flags
        info!(nic_log, "Found a network interface";
              "up" => interface.is_up,
              "loopback" => interface.is_loopback,
              "multicast" => interface.is_multicast,
              "broadcast" => interface.is_broadcast,
              "point-to-point" => interface.is_point_to_point);

        // Report link address, if any
        if let Some(link_address_props) = interface.link_address {
            assert_eq!(
                link_address_props.netmask, None,
                "Link-layer addresses shouldn't have subnet masks"
            );
            let formatted_broadcast = match link_address_props.target {
                Some(TargetAddress::Broadcast(addr)) => format!("{}", addr),
                None => "None".to_owned(),
                Some(TargetAddress::PointToPoint(_addr)) => {
                    unreachable!(
                        "Point-to-point links shouldn't have a destination \
                         address"
                    );
                }
            };
            info!(nic_log, "Got a link-layer address";
                  "address" => %link_address_props.address,
                  "broadcast address" => formatted_broadcast);
        }

        // Report IPv4 addresses
        for ipv4_address_props in interface.ipv4_addresses {
            let netmask = ipv4_address_props
                .netmask
                .expect("IP addresses should have a subnet mask");
            info!(nic_log, "Got an IPv4 address";
                  "address" => ?ipv4_address_props.address,
                  "netmask" => ?netmask,
                  "target" => ?ipv4_address_props.target);
        }

        // Report IPv6 addresses
        for ipv6_address_props in interface.ipv6_addresses {
            let netmask = ipv6_address_props
                .netmask
                .expect("IP addresses should have a subnet mask");
            info!(nic_log, "Got an IPv6 address";
                  "address" => ?ipv6_address_props.address,
                  "netmask" => ?netmask,
                  "target" => ?ipv6_address_props.target);
        }
    }
}

/// Report on the host's temperature sensors
fn report_temp_sensors(log: &Logger, temperatures: Vec<TemperatureSensor>) {
    // TODO: Consider exposing this later on
    struct SensorProperties {
        label: Option<String>,
        high_trip_point: Option<Temperature>,
        critical_trip_point: Option<Temperature>,
    }
    let mut unit_to_sensors = BTreeMap::<String, Vec<_>>::new();

    debug!(log, "Processing temperature sensor list...");
    for sensor in temperatures {
        let sensor_list = unit_to_sensors.entry(sensor.unit().to_owned()).or_default();
        sensor_list.push(SensorProperties {
            label: sensor.label().map(|label| label.to_owned()),
            high_trip_point: sensor.high(),
            critical_trip_point: sensor.critical(),
        });
    }

    for (unit, mut sensor_list) in unit_to_sensors {
        let unit_log = log.new(o!("sensor unit" => unit));
        sensor_list.sort_by_cached_key(|sensor| sensor.label.clone());
        for sensor in sensor_list {
            let to_celsius = |t_opt: Option<Temperature>| t_opt.map(|t| t.get::<degree_celsius>());
            info!(unit_log, "Found a temperature sensor";
                  "label" => sensor.label,
                  "high trip point (°C)" => to_celsius(sensor.high_trip_point),
                  "critical trip point (°C)" => to_celsius(sensor.critical_trip_point));
        }
    }
}

/// Report on the host's operating system and use of virtualization
fn report_os(log: &Logger, platform: Platform, virt: Option<Virtualization>) {
    info!(
        log,
        "Received host OS information";
        "hostname" => platform.hostname(),
        "OS name" => platform.system(),
        "OS release" => platform.release(),
        "OS version" => platform.version()
    );

    if let Some(virt) = virt {
        warn!(
            log,
            "Found underlying virtualization layers, make sure that they don't \
             bias your benchmarks!";
            "detected virtualization scheme" => ?virt
        );
    }
}

/// Report on the host's open user sessions
fn report_users(log: &Logger, user_connections: Vec<User>) {
    // TODO: Consider returning some of this for future use
    type SessionId = i32; // FIXME: Make heim expose this
    #[derive(Default)]
    struct UserStats {
        /// Total number of connections opened by this user
        connection_count: usize,

        /// Breakdown of these connections into sessions and login processes
        /// (This data is, for now, only available on Linux)
        sessions_to_pids: Option<BTreeMap<SessionId, Vec<Pid>>>,
    };
    let mut usernames_to_stats = BTreeMap::<String, UserStats>::new();

    debug!(log, "Processing user connection list...");
    for connection in user_connections {
        let username = connection.username().to_owned();
        let user_log = log.new(o!("username" => username.clone()));
        debug!(user_log, "Found a new user connection");

        let user_stats = usernames_to_stats.entry(username).or_default();
        user_stats.connection_count += 1;

        #[cfg(target_os = "linux")]
        {
            use heim::host::os::linux::UserExt;
            debug!(user_log,
                   "Got Linux-specific connection details";
                   "login process PID" => connection.pid(),
                   "(pseudo-)tty name" => connection.terminal(),
                   "terminal identifier" => connection.id(),
                   "remote hostname" => connection.hostname(),
                   "remote IP address" => ?connection.address(),
                   "session ID" => connection.session_id());
            let session_stats = user_stats
                .sessions_to_pids
                .get_or_insert_with(Default::default)
                .entry(connection.session_id())
                .or_default();
            session_stats.push(connection.pid());
        }
    }

    for (username, stats) in &mut usernames_to_stats {
        let user_log = log.new(o!("username" => username.clone()));
        info!(user_log, "Found a logged-in user";
              "open connection count" => stats.connection_count);
        if let Some(ref mut sessions_to_pids) = &mut stats.sessions_to_pids {
            for (session_id, login_pids) in sessions_to_pids {
                login_pids.sort();
                info!(user_log,
                      "Got details of a user session";
                      "session ID" => session_id,
                      "login process PID(s)" => ?login_pids);
            }
        }
    }

    if usernames_to_stats.len() > 1 {
        warn!(
            log,
            "Detected multiple logged-in users, make sure others keep the \
             system quiet while your benchmarks are running!"
        );
    }
}

/// Pretty-print a quantity of information from heim
fn format_information(quantity: Information) -> String {
    // Get the quantity of information in bytes
    let bytes = quantity.get::<byte>();

    // General recipe for printing fractional SI information quantities
    let format_bytes = |power_of_10, unit| {
        let base = 10_u64.pow(power_of_10);
        let integral_part = bytes / base;
        let fractional_part = (bytes / (base / 1000)) % 1000;
        format!("{}.{:03} {}", integral_part, fractional_part, unit)
    };

    // Check the order of magnitude and pick the right SI multiple
    match (bytes as f64).log10().trunc() as u8 {
        0..=2 => format!("{} B", bytes),
        3..=5 => format_bytes(3, "kB"),
        6..=8 => format_bytes(6, "MB"),
        9..=11 => format_bytes(9, "GB"),
        _ => format_bytes(12, "TB")
    }
}
