use heim::{host::Platform, virt::Virtualization};

use slog::{info, warn, Logger};

/// Report on the host's operating system and use of virtualization
pub fn startup_report(log: &Logger, platform: Platform, virt: Option<Virtualization>) {
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
