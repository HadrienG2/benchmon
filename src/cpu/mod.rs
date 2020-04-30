//! Query and display CPU information

pub mod freq;

use futures_util::{
    future::{FutureExt, TryFutureExt},
    stream::TryStreamExt,
    try_join,
};

use heim::{
    cpu::{CpuFrequency, CpuStats, CpuTime},
    units::{frequency::megahertz, time::second, Frequency, Time},
};

use slog::{debug, info, warn, Logger};

use std::time::{Duration, Instant};

/// Range of possible CPU frequencies
#[derive(Clone, Copy)]
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
///
/// User time, kernel time, etc. are reported as a fractions of the overall
/// elapsed CPU time, since that's both lighter-weight than one Duration per
/// timing and most useful for real-time system monitoring.
///
/// If the use of single-precision floating point is bothering you, remember
/// that you should not measure, say, the total user time spent doing something
/// by accumulating user times across many small measurement periods, as that
/// will lead to loss of accuracy no matter which floating-point type you use.
/// Instead, you should measure one `heim::cpu::time()` point at the beginning
/// of the operation, one at the end, and subtract the results of these two
/// measurements to get an elapsed timing.
///
// TODO: Once we support benchmark runner operation, provide a good API for such
//       "measuring the user/kernel time spent doing something" kind of use
//       cases. Ideally something like Instant::now() and elapsed().
pub struct DurationBreakdown {
    /// Overall CPU time elapsed since last measurement
    pub overall: Duration,

    /// Fraction of time spent in user mode processes (including guests)
    pub user_frac: f32,

    /// Fraction of time spent in kernel mode processes
    pub system_frac: f32,

    /// Fraction of time spent doing nothing
    pub idle_frac: f32,

    /// Fractions of time spent doing Linux-specific activities
    #[cfg(target_os = "linux")]
    pub linux_fracs: LinuxDurationFracs,
}

/// Linux-specific complement to CPUDuration
pub struct LinuxDurationFracs {
    /// Fraction of time spent in niced user mode processes (including guests)
    pub nice_frac: f32,

    /// Fraction of time spent waiting for I/O to complete
    pub io_wait_frac: f32,

    /// Fraction of time spent servicing hardware interrupts
    pub irq_frac: f32,

    /// Fraction of time spent servicing software interrupts
    pub soft_irq_frac: f32,

    /// Fraction of time spent by other OSes running in a virtualized environment
    pub steal_frac: f32,

    /// Fraction of time spent running a vCPU for Linux-controlled guests
    pub guest_frac: Option<f32>,

    /// Fraction of time spent running a vCPU for niced Linux-controlled guests
    pub guest_nice_frac: Option<f32>,
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
    pub async fn new() -> heim::Result<Self> {
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

    /// Report what we know about the overall CPU frequency range
    pub fn frequency_range(&self) -> &FrequencyRange {
        &self.frequency_range
    }

    // TODO: Mesurer fréquence absolue et relative (cf ci-dessous)

    /// Report the change in CPU statistics since the last measurement
    ///
    /// If you want the CPU statistics since boot, it is better to call
    /// `heim::cpu::stats()` directly.
    ///
    pub async fn stats_change(&mut self) -> heim::Result<StatsDelta> {
        #[cfg(target_os = "linux")]
        use heim::cpu::os::linux::CpuStatsExt;

        let stats = heim::cpu::stats().await?;
        let result = StatsDelta {
            new_ctx_switches: stats.ctx_switches() - self.stats.ctx_switches(),
            new_interrupts: stats.interrupts() - self.stats.interrupts(),
            #[cfg(target_os = "linux")]
            new_soft_interrupts: stats.soft_interrupts() - self.stats.soft_interrupts(),
        };
        self.stats = stats;
        Ok(result)
    }

    /// Report the change in aggregated CPU timings since the last measurement
    ///
    /// If you want the CPU timings since boot, it is better to call
    /// `heim::cpu::time()` directly.
    ///
    pub async fn time_change(&mut self) -> heim::Result<DurationBreakdown> {
        #[cfg(target_os = "linux")]
        use heim::cpu::os::linux::CpuTimeExt;

        let time = heim::cpu::time().await?;
        let timestamp = Instant::now();
        let (old_time, old_timestamp) = &self.time;

        // NOTE: This will be wrong if logical_count changes, but that event is
        //       so uncommon (it requires a complex VM setup) that we can afford
        //       not to handle it in this particular measurement.
        let overall = (timestamp - *old_timestamp) * self.logical_count as u32;
        let overall_secs = overall.as_secs_f64();
        let to_frac = |time: Time| -> f32 {
            let time_secs = time.get::<second>();
            (time_secs / overall_secs) as f32
        };
        let guest_sub = |t1: Option<Time>, t2: Option<Time>| -> Option<Time> {
            match (t1, t2) {
                (Some(t1), Some(t2)) => Some(t1 - t2),
                (None, None) => None,
                _ => unreachable!(),
            }
        };

        let result = DurationBreakdown {
            overall,
            user_frac: to_frac(time.user() - old_time.user()),
            system_frac: to_frac(time.system() - old_time.system()),
            idle_frac: to_frac(time.idle() - old_time.idle()),
            #[cfg(target_os = "linux")]
            linux_fracs: LinuxDurationFracs {
                nice_frac: to_frac(time.nice() - old_time.nice()),
                io_wait_frac: to_frac(time.io_wait() - old_time.io_wait()),
                irq_frac: to_frac(time.irq() - old_time.irq()),
                soft_irq_frac: to_frac(time.soft_irq() - old_time.soft_irq()),
                steal_frac: to_frac(time.steal() - old_time.steal()),
                guest_frac: guest_sub(time.guest(), old_time.guest()).map(to_frac),
                guest_nice_frac: guest_sub(time.guest_nice(), old_time.guest_nice()).map(to_frac),
            },
        };

        self.time = (time, timestamp);
        Ok(result)
    }

    /// Number of logical CPU cores (including e.g. hyperthreads)
    pub fn logical_count(&self) -> u64 {
        self.logical_count
    }

    /// Report what we know about per-CPU frequency ranges
    ///
    /// If available, will report one entry per logical CPU cores.
    ///
    pub fn frequency_ranges(&self) -> Option<&[FrequencyRange]> {
        let frequency_ranges = self.frequency_ranges.as_ref()?;
        debug_assert_eq!(frequency_ranges.len() as u64, self.logical_count);
        Some(&frequency_ranges[..])
    }

    // TODO: CPU frequencies
    //       (Must detect change in CPU core count & panic w/ clear error
    //        should also assert that frequency range remains the same)
    // TODO: Relative CPU frequencies, if available, 0 is min and 1 is max
    //       (Based on frequency_ranges + frequencies)
    // TODO: Elapsed per-CPU times (reuse time_change logic!)
    //       (Must detect change in CPU core count & panic w/ clear error)

    /// Number of physical CPU cores, if known
    pub fn physical_count(&self) -> Option<u64> {
        self.physical_count
    }
}

/// Report on the host's CPU configuration
// TODO: Move to Monitor
// TODO: Report some other things that we now record in Monitor, such as stats
//       and timings since boot.
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
