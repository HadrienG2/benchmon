mod clock;
mod cpu;
mod filesystem;
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

use heim::units::{information::byte, Information};

use slog::{info, o, Drain, Logger};

use std::{
    fmt::{self, Display},
    sync::Mutex,
    thread,
    time::Duration,
};

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

    // Do dynamic system monitoring
    // TODO: Should use different format for stdout records and file records,
    //       once file output is supported.
    let clock_formatter = clock::Formatter::new(&cli_opts.time_format);

    // TODO: Once we have a good system monitor, also allow using it to monitor
    //       execution of some benchmark. Measure baseline before starting
    //       benchmark execution. Also monitor child getrusage() during process
    //       execution, and wall-clock execution time.
    //
    // TODO: Repeat headers every screenful of data like dstat does
    // TODO: Merge title functionality into ClockFormatter, call it format_title
    // TODO: If ClockFormatter output width is shorter than title, increase output width
    const CLOCK_TITLE: &str = "time";
    print_col_header(CLOCK_TITLE, clock_formatter.output_width());
    println!();
    loop {
        // TODO: Monitor other quantities
        // TODO: Make the set of monitored quantities configurable
        let local_time = LocalTime::now();
        // TODO: Print multiple quantities in a tabular fashion
        // TODO: In addition to stdout, support in-memory records, dump to file
        println!("{}|", clock_formatter.format(local_time));
        // TODO: Make this configurable
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

/// Pretty-print a quantity of information from heim
fn format_information(quantity: Information) -> String {
    // Get the quantity of information in bytes
    let bytes = quantity.get::<byte>();

    // Check that quantity's order of magnitude
    let magnitude = if bytes > 0 {
        (bytes as f64).log10().trunc() as u8
    } else {
        0
    };

    // General recipe for printing fractional SI information quantities
    let format_bytes = |unit_magnitude, unit| {
        let base = 10_u64.pow(unit_magnitude);
        let integral_part = bytes / base;
        let fractional_part = (bytes / (base / 1000)) % 1000;
        format!("{}.{:03} {}", integral_part, fractional_part, unit)
    };

    // Select the right recipe depending on the order of magnitude
    match magnitude {
        0..=2 => format!("{} B", bytes),
        3..=5 => format_bytes(3, "kB"),
        6..=8 => format_bytes(6, "MB"),
        9..=11 => format_bytes(9, "GB"),
        _ => format_bytes(12, "TB"),
    }
}

/// Wrapper around a Display impl which enforces a fixed output width
struct FixedDisplay<InnerDisplay: Display> {
    /// Inner Display implementation
    inner: InnerDisplay,

    /// Desired output width (should be an upper bound of inner's output width)
    max_output_width: usize,
}

impl<InnerDisplay: Display> FixedDisplay<InnerDisplay> {
    /// Build a fixed-width display
    pub fn new(inner: InnerDisplay, max_output_width: usize) -> Self {
        Self {
            inner,
            max_output_width,
        }
    }
}

impl<InnerDisplay: Display> Display for FixedDisplay<InnerDisplay> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "{inner:width$}",
            inner = &self.inner,
            width = self.max_output_width
        )
    }
}

/// Display the header of a column of measurements
fn print_col_header(name: &str, width: usize) {
    print!("{0:-^1$}|", name, width)
}
