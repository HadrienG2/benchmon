use heim::{cpu::CpuFrequency, host::Arch, units::frequency::megahertz};

use slog::{debug, info, warn, Logger};

/// Report on the host's CPU configuration
pub fn startup_report(
    log: &Logger,
    cpu_arch: Arch,
    logical_cpus: u64,
    physical_cpus: Option<u64>,
    global_cpu_freq: CpuFrequency,
    per_cpu_freqs: Option<Vec<CpuFrequency>>,
) {
    info!(log, "Received CPU configuration information";
          "architecture" => ?cpu_arch,
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