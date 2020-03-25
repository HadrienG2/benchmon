// FIXME: I probably need to have a word with the heim dev about this
#![type_length_limit = "40000000"]

use futures_util::{
    future::FutureExt,
    pin_mut,
    stream::{Stream, StreamExt},
    try_join,
};

use heim::{
    cpu::CpuFrequency,
    host::{Arch, Pid, Platform, User},
    memory::{Memory, Swap},
    units::{
        frequency::megahertz,
        information::{byte, gigabyte, kilobyte, megabyte, terabyte},
        thermodynamic_temperature::degree_celsius,
        Information,
    },
    virt::Virtualization,
};

use slog::{debug, info, o, warn, Drain, Logger};

use std::{collections::BTreeMap, sync::Mutex};

#[async_std::main]
async fn main() -> heim::Result<()> {
    // Set up a logger
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build();
    let drain = Mutex::new(drain).fuse();
    let log = slog::Logger::root(drain, o!("benchmon version" => env!("CARGO_PKG_VERSION")));

    // Query all system info at once, leveraging heim's asynchronous nature...
    info!(log, "Probing host system characteristics...");
    let global_cpu_freq = heim::cpu::frequency();
    #[cfg(target_os = "linux")]
    let per_cpu_freqs = Some(heim::cpu::os::linux::frequencies());
    #[cfg(not(target_os = "linux"))]
    let per_cpu_freqs = None;
    let disk_partitions = heim::disk::partitions();
    let logical_cpus = heim::cpu::logical_count();
    let memory = heim::memory::memory();
    let network_interfaces = heim::net::nic();
    let physical_cpus = heim::cpu::physical_count();
    let platform = heim::host::platform();
    let swap = heim::memory::swap();
    let temperatures = heim::sensors::temperatures();
    let user_connections = heim::host::users();
    let virt = heim::virt::detect().map(Ok);
    // TODO: Retrieve current process + initial processes info
    let (global_cpu_freq, logical_cpus, memory, physical_cpus, platform, swap, virt) = try_join!(
        global_cpu_freq,
        logical_cpus,
        memory,
        physical_cpus,
        platform,
        swap,
        virt
    )?;

    // Report operating system and use of virtualization
    // (CPU architecture doesn't really belong here and will be displayed later)
    let cpu_arch = platform.architecture();
    report_os(&log, platform, virt);

    // Report open user sessions
    report_users(&log, user_connections).await?;

    // Report CPU configuration
    report_cpus(
        &log,
        cpu_arch,
        logical_cpus,
        physical_cpus,
        global_cpu_freq,
        per_cpu_freqs,
    )
    .await?;

    // Report memory configuration
    report_memory(&log, memory, swap);

    // TODO: Finish work-in-progress slog port

    // Report filesystem mounts
    println!("- Filesystem mount(s):");
    pin_mut!(disk_partitions);
    // TODO: Instead of displaying output of raw iteration, collect and sort by
    //       mount point.
    while let Some(partition) = disk_partitions.next().await {
        let partition = partition?;
        // FIXME: Replace Debug printout with controlled format
        print!("    * {:?}, with ", partition);
        match heim::disk::usage(partition.mount_point()).await {
            Ok(usage) if usage.total() != Information::new::<byte>(0) => {
                println!("a capacity of {}", format_information(usage.total()));
            }
            Ok(_) => {
                println!("zero capacity (likely a pseudo-filesystem)");
            }
            Err(e) => {
                println!("failing capacity check ({})", e);
            }
        }
    }

    // Report network interfaces
    println!("- Network interface(s):");
    pin_mut!(network_interfaces);
    while let Some(nic) = network_interfaces.next().await {
        // TODO: Group by name and sort alphabetically using a BTreeMap
        // FIXME: Replace Debug printout with controlled format
        println!("    * {:?}", nic?);
    }

    // Report temperature sensors
    println!("- Temperature sensor(s):");
    pin_mut!(temperatures);
    while let Some(sensor) = temperatures.next().await {
        // TODO: Group by unit and sort alphabetically using a BTreeMap
        let sensor = sensor?;
        print!("    * ");
        if let Some(label) = sensor.label() {
            print!("\"{}\"", label);
        } else {
            print!("Unlabeled sensor");
        }
        print!(" from unit \"{}\" (", sensor.unit());
        if let Some(high) = sensor.high() {
            print!("high: {} °C", high.get::<degree_celsius>());
        } else {
            print!("no high trip point");
        }
        print!(", ");
        if let Some(critical) = sensor.critical() {
            print!("critical: {} °C", critical.get::<degree_celsius>());
        } else {
            print!("no critical trip point");
        }
        println!(")");
    }

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

/// Report on the host' operating system and use of virtualization
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

/// Report on the host' open user sessions
async fn report_users(
    log: &Logger,
    user_connections: impl Stream<Item = heim::Result<User>>,
) -> heim::Result<()> {
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
    let mut usernames_to_stats = BTreeMap::<_, UserStats>::new();

    info!(log, "Enumerating user connections...");
    pin_mut!(user_connections);
    while let Some(connection) = user_connections.next().await {
        let connection = connection?;
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

    for (username, stats) in &usernames_to_stats {
        let user_log = log.new(o!("username" => username.clone()));
        info!(user_log, "Found a logged-in user";
              "open connection count" => stats.connection_count);
        if let Some(sessions_to_pids) = &stats.sessions_to_pids {
            for (session_id, login_pids) in sessions_to_pids {
                info!(user_log,
                      "Got user session details";
                      "session ID" => session_id,
                      "login process PIDs" => ?login_pids);
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

    Ok(())
}

/// Report on the host's CPU configuration
async fn report_cpus(
    log: &Logger,
    cpu_arch: Arch,
    logical_cpus: u64,
    physical_cpus: Option<u64>,
    global_cpu_freq: CpuFrequency,
    per_cpu_freqs: Option<impl Stream<Item = heim::Result<CpuFrequency>>>,
) -> heim::Result<()> {
    info!(log, "Received CPU configuration information";
          "architecture" => ?cpu_arch,
          "logical CPU count" => logical_cpus,
          "physical CPU count" => physical_cpus);

    let log_freq_range = |log: &Logger, title: &str, freq: &CpuFrequency| {
        if let (Some(min), Some(max)) = (freq.min(), freq.max()) {
            info!(log, "Found {} frequency range", title;
                  "min frequency (MHz)" => min.get::<megahertz>(),
                  "max frequency (MHz)" => max.get::<megahertz>());
        } else {
            warn!(log, "Some {} frequency range data is missing", title;
                  "min frequency" => ?freq.min(),
                  "max frequency" => ?freq.max());
        }
    };

    // If a per-CPU frequency breakdown is available, check if the frequency
    // range differs from one CPU to another. This can happen on some embedded
    // architectures (ARM big.LITTLE comes to mind), but should be rare on the
    // typical x86-ish benchmarking node.
    //
    // If the frequency range is CPU-dependent, log the detailed breakdown,
    // otherwise stick with the cross-platform default of only printing the
    // global CPU frequency range, since it's more concise.
    //
    let mut printing_detailed_freqs = false;
    if let Some(per_cpu_freqs) = per_cpu_freqs {
        let global_freq_range = (global_cpu_freq.min(), global_cpu_freq.max());
        let cpu_indices_and_freqs = per_cpu_freqs.enumerate();
        debug!(
            log,
            "Per-CPU frequency ranges are available, enumerating them..."
        );
        pin_mut!(cpu_indices_and_freqs);
        while let Some((idx, freq)) = cpu_indices_and_freqs.next().await {
            let cpu_log = log.new(o!("logical cpu index" => idx));
            let freq = freq?;
            if printing_detailed_freqs {
                log_freq_range(&cpu_log, "per-CPU", &freq);
            } else if (freq.min(), freq.max()) != global_freq_range {
                printing_detailed_freqs = true;
                for old_idx in 0..idx {
                    let old_cpu_log = log.new(o!("logical cpu index" => old_idx));
                    log_freq_range(&old_cpu_log, "per-CPU", &global_cpu_freq);
                }
                log_freq_range(&cpu_log, "per-CPU", &freq);
            }
        }

        if !printing_detailed_freqs {
            debug!(
                log,
                "Per-CPU frequency ranges match global frequency range, no \
                 need for a detailed breakdown"
            );
        }
    }

    if !printing_detailed_freqs {
        log_freq_range(&log, "global CPU", &global_cpu_freq);
    }

    Ok(())
}

// Report on the host's memory configuration
fn report_memory(log: &Logger, memory: Memory, swap: Swap) {
    info!(log, "Received memory configuration information";
          "RAM size" => format_information(memory.total()),
          "swap size" => format_information(swap.total()));

    if swap.used() > swap.total() / 10 {
        warn!(
            log,
            "Non-negligible use of swap detected, make sure that it doesn't
             bias your benchmark!";
            "swap usage" => format_information(swap.used())
        );
    }
}

/// Pretty-print a quantity of information from heim
fn format_information(quantity: Information) -> String {
    // FIXME: This can be optimized with a log-based jump table, and probably
    //        deduplicated as well if I think hard enough about it.
    if quantity > Information::new::<terabyte>(1) {
        let terabytes = quantity.get::<terabyte>();
        let gigabytes = quantity.get::<gigabyte>() - 1000 * terabytes;
        format!("{}.{:03} TB", terabytes, gigabytes)
    } else if quantity > Information::new::<gigabyte>(1) {
        let gigabytes = quantity.get::<gigabyte>();
        let megabytes = quantity.get::<megabyte>() - 1000 * gigabytes;
        format!("{}.{:03} GB", gigabytes, megabytes)
    } else if quantity > Information::new::<megabyte>(1) {
        let megabytes = quantity.get::<megabyte>();
        let kilobytes = quantity.get::<kilobyte>() - 1000 * megabytes;
        format!("{}.{:03} MB", megabytes, kilobytes)
    } else if quantity > Information::new::<kilobyte>(1) {
        let kilobytes = quantity.get::<kilobyte>();
        let bytes = quantity.get::<byte>() - 1000 * kilobytes;
        format!("{}.{:03} kB", kilobytes, bytes)
    } else {
        format!("{} B", quantity.get::<byte>())
    }
}
