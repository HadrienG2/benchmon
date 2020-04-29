//! Query and display CPU information

pub mod freq;

use futures_util::{
    future::{FutureExt, TryFutureExt},
    stream::TryStreamExt,
    try_join,
};

use heim::{
    cpu::{CpuFrequency, CpuStats, CpuTime},
    units::{frequency::megahertz, Frequency},
};

use slog::{debug, info, warn, Logger};

use std::time::{Duration, Instant};

/// Range of possible CPU frequencies
pub struct FrequencyRange {
    /// Minimal CPU frequency (if known)
    pub min: Option<Frequency>,

    /// Maximal CPU frequency (if known)
    pub max: Option<Frequency>,
}

/// CPU statistics variation between two measurements
pub struct StatsDelta {
    /// New context switches (voluntary + involuntary)
    pub new_ctx_switches: u64,

    /// New interrupts
    pub new_interrupts: u64,

    /// New software interrupts (Linux-only)
    #[cfg(target_os = "linux")]
    pub new_soft_interrupts: u64,
}

/// Breakdown of elapsed CPU time by system activity
pub struct DurationBreakdown {
    /// Overall CPU time elapsed since last measurement
    // NOTE: Must multiply actual duration by logical_count for aggregated
    pub overall: Duration,

    /// Fraction of time spent in user mode processes (including guests)
    pub user_frac: f64,

    /// Fraction of time spent in kernel mode processes
    pub system_frac: f64,

    /// Fraction of time spent doing nothing
    pub idle_frac: f64,

    /// Fractions of time spent doing Linux-specific activities
    #[cfg(target_os = "linux")]
    pub linux_fracs: LinuxDurationFracs,
}

/// Linux-specific complement to CPUDuration
pub struct LinuxDurationFracs {
    /// Fraction of time spent in niced user mode processes (including guests)
    pub nice_frac: f64,

    /// Fraction of time spent waiting for I/O to complete
    pub io_wait_frac: f64,

    /// Fraction of time spent servicing hardware interrupts
    pub irq_frac: f64,

    /// Fraction of time spent servicing software interrupts
    pub soft_irq_frac: f64,

    /// Fraction of time spent by other OSes running in a virtualized environment
    pub steal_frac: f64,

    /// Fraction of time spent running a vCPU for Linux-controlled guests
    pub guest_frac: f64,

    /// Fraction of time spent running a vCPU for niced Linux-controlled guests
    pub guest_nice_frac: f64,
}

/// CPU monitoring mechanism
pub struct Monitor {
    /// Global frequency range
    frequency_range: FrequencyRange,

    /// Last measured statistics (context switches, interrupts, etc)
    stats: CpuStats,

    /// Last measured aggregated timings + associated timestamp
    time: (CpuTime, Instant),

    /// Number of logical cores
    logical_count: u64,

    /// Per-core frequencies (if known)
    // INVARIANT: Must keep frequency_ranges.len() == logical_count
    frequency_ranges: Option<Box<[FrequencyRange]>>,

    /// Per-core timings + associated timestamps
    // INVARIANT: Must keep times.len() == logical_count
    times: Box<[(CpuTime, Instant)]>,

    /// Number of physical cores (if known)
    physical_count: Option<u64>,
}

impl Monitor {
    /// Set up CPU monitoring
    async fn new() -> heim::Result<Self> {
        // Extend/narrow the raw heim measurements to make them more useful
        let extract_range = |freq: CpuFrequency| FrequencyRange {
            min: freq.min(),
            max: freq.max(),
        };
        let add_timestamp = |time: CpuTime| (time, Instant::now());

        // Request long-lasting CPU properties and initial CPU state
        // TODO: Do some type length profiling here
        let frequency_range = heim::cpu::frequency().map_ok(extract_range).boxed();
        let stats = heim::cpu::stats();
        let time = heim::cpu::time().map_ok(add_timestamp);
        let logical_count = heim::cpu::logical_count();
        #[cfg(target_os = "linux")]
        let frequency_ranges = heim::cpu::os::linux::frequencies()
            .map_ok(extract_range)
            .try_collect::<Vec<_>>()
            .map_ok(|vec| Some(vec.into_boxed_slice()))
            .boxed();
        #[cfg(not(target_os = "linux"))]
        let frequency_ranges = futures_util::future::ok(None);
        let times = heim::cpu::times()
            .map_ok(add_timestamp)
            .try_collect::<Vec<_>>()
            .map_ok(Vec::into_boxed_slice);
        let physical_count = heim::cpu::physical_count();

        // Wait for all the data to arrive and make this a monitor
        let (frequency_range, stats, time, logical_count, frequency_ranges, times, physical_count) =
            try_join!(
                frequency_range,
                stats,
                time,
                logical_count,
                frequency_ranges,
                times,
                physical_count
            )?;
        Ok(Self {
            frequency_range,
            stats,
            time,
            logical_count,
            frequency_ranges,
            times,
            physical_count,
        })
    }

    // TODO: Queries of all properties that update the inner state
    //       (use the delta structs defined above, and update the constructor
    //       if some shorthand to one of the above properties is introduced)
}

/// Report on the host's CPU configuration
pub fn startup_report(
    log: &Logger,
    logical_cpus: u64,
    physical_cpus: Option<u64>,
    global_cpu_freq: CpuFrequency,
    per_cpu_freqs: Option<Vec<CpuFrequency>>,
) {
    info!(log, "Received CPU configuration information";
          "logical CPU count" => logical_cpus,
          "physical CPU count" => physical_cpus);

    let log_freq_range = |freq: &CpuFrequency, cpu_name: &str| {
        if let (Some(min), Some(max)) = (freq.min(), freq.max()) {
            info!(log, "Found CPU frequency range";
                  "min frequency (MHz)" => min.get::<megahertz>(),
                  "max frequency (MHz)" => max.get::<megahertz>(),
                  "cpu" => cpu_name);
        } else {
            warn!(log, "Some CPU frequency range data is missing";
                  "min frequency" => ?freq.min(),
                  "max frequency" => ?freq.max(),
                  "cpu" => cpu_name);
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
        debug!(log, "Got per-CPU frequency ranges, processing them...");

        for (idx, freq) in per_cpu_freqs.into_iter().enumerate() {
            if printing_detailed_freqs {
                log_freq_range(&freq, &idx.to_string());
            } else if (freq.min(), freq.max()) != global_freq_range {
                printing_detailed_freqs = true;
                for old_idx in 0..idx {
                    log_freq_range(&global_cpu_freq, &old_idx.to_string());
                }
                log_freq_range(&freq, &idx.to_string());
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
        log_freq_range(&global_cpu_freq, "all");
    }
}
