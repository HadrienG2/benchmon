mod clock;
mod cpu;
mod filesystem;
mod format;
mod memory;
mod network;
mod os;
mod process;
mod sensors;
mod users;

use chrono::Local as LocalTime;

use futures_util::{
    future::{FutureExt, TryFutureExt},
    stream::{StreamExt, TryStreamExt},
    try_join,
};

use slog::{info, o, Drain, Logger};

use std::{sync::Mutex, thread, time::Duration};

use structopt::StructOpt;

// Command-line options
#[derive(Debug, StructOpt)]
#[structopt(rename_all = "kebab-case")]
/// A benchmarking-oriented system monitor
struct CliOpts {
    /// Report the host system's characteristics on startup
    #[structopt(long)]
    startup_report: bool,

    /// Desired date/time format, in strftime notation
    #[structopt(long, default_value = "%H:%M:%S")]
    time_format: String,
}

#[async_std::main]
async fn main() -> heim::Result<()> {
    // Parse the command-line options
    let cli_opts = CliOpts::from_args();

    // Set up a logger
    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator).build();
    let drain = Mutex::new(drain).fuse();
    let log = slog::Logger::root(drain, o!("benchmon version" => env!("CARGO_PKG_VERSION")));

    // Produce the initial system report, if asked to
    if cli_opts.startup_report {
        startup_report(&log).await?;
    }

    // Prepare to print periodical clock measurements
    //
    // TODO: Should use different format for stdout records and file records,
    //       once dedicated CSV file output is supported.
    let clock_formatter = clock::Formatter::new(&cli_opts.time_format);

    // Perform general system monitoring
    //
    // TODO: Once we have a good system monitor, also allow using it to monitor
    //       execution of some benchmark. Measure baseline before starting
    //       benchmark execution. Also monitor child getrusage() during process
    //       execution, and wall-clock execution time.
    //
    let mut newlines_since_last_header = u64::MAX;
    loop {
        // Print a header describing the measurements in the beginning, and if
        // we are outputting to a terminal, re-print it once per page of output.
        const HEADER_HEIGHT: u64 = 1;
        let term_height = termize::dimensions_stdout()
            .map(|(_width, height)| height as u64)
            .unwrap_or(u64::MAX);
        if newlines_since_last_header >= term_height - HEADER_HEIGHT {
            println!("{}|", clock_formatter.display_title());
            newlines_since_last_header = 1;
        }

        // Measure the time
        // TODO: Monitor other quantities
        // TODO: Make the set of monitored quantities configurable
        let local_time = LocalTime::now();

        // Display the measurements
        // TODO: Print multiple quantities in a tabular fashion
        // TODO: In addition to stdout, support in-memory records, dump to file
        println!("{}|", clock_formatter.display_data(local_time));
        newlines_since_last_header += 1;

        // Wait for a while
        // TODO: Make period configurable
        thread::sleep(Duration::new(1, 0));
    }

    // TODO: After end of benchmark execution, produce tabular data sets for
    //       manual inspection to begin with, and later implement direct
    //       support for fancy plots (with plotters? plotly?)
}

/// Describe the host system on application startup
async fn startup_report(log: &Logger) -> heim::Result<()> {
    // Ask heim to start fetching all the system info we need...
    // (with a bit of future boxing here and there to reduce type complexity)
    info!(log, "Probing host system characteristics...");
    // - CPU info
    let global_cpu_freq = heim::cpu::frequency().boxed();
    #[cfg(target_os = "linux")]
    let per_cpu_freqs = heim::cpu::os::linux::frequencies()
        .try_collect::<Vec<_>>()
        .map_ok(Some)
        .boxed();
    #[cfg(not(target_os = "linux"))]
    let per_cpu_freqs = futures_util::future::ok(None);
    let logical_cpus = heim::cpu::logical_count();
    let physical_cpus = heim::cpu::physical_count();
    // - Platform info (= OS info + CPU architecture)
    let platform = heim::host::platform();
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
    //
    // FIXME: This stream is where 80% of the type complexity lies (crate max
    //        type length goes from ~230000 to ~47000 upon commenting sensor
    //        reporting out), but I cannot box it because that causes a weird
    //        E0308 "one type is more general than the other" error.
    //
    //        There are multiple reports of similar confusing errors on the
    //        rustc bugtracker, I subscribed to those for now and will try again
    //        after they are fixed.
    //
    let temperatures = heim::sensors::temperatures().try_collect::<Vec<_>>();
    // - Virtualization info
    let virt = heim::virt::detect().boxed();
    // - User connexion info
    let user_connections = heim::host::users().try_collect::<Vec<_>>();
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
    filesystem::startup_report(&log, disk_partitions_and_usage);

    // Report network configuration
    let network_interfaces = network_interfaces.await?;
    network::startup_report(&log, network_interfaces);

    // Report sensor configuration
    let temperatures = temperatures.await?;
    sensors::startup_report(&log, temperatures);

    // Report operating system and use of virtualization
    let virt = virt.await;
    os::startup_report(&log, platform, virt);

    // Report open user sessions
    let user_connections = user_connections.await?;
    users::startup_report(&log, user_connections);

    // Report running processes
    let processes = processes.await?;
    process::startup_report(&log, processes);
    Ok(())
}
