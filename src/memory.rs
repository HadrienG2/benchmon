use heim::memory::{Memory, Swap};

use slog::{info, warn, Logger};

// Report on the host's memory configuration
pub fn startup_report(log: &Logger, memory: Memory, swap: Swap) {
    info!(log, "Received memory configuration information";
          "RAM size" => crate::format_information(memory.total()),
          "swap size" => crate::format_information(swap.total()));

    if swap.used() > swap.total() / 10 {
        warn!(
            log,
            "Non-negligible use of swap detected, make sure that it doesn't
             bias your benchmark!";
            "swap usage" => crate::format_information(swap.used())
        );
    }
}
