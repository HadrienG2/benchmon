use crate::format;

use heim::memory::{Memory, Swap};

use slog::{info, warn, Logger};

/// Report on the host's memory configuration
pub fn startup_report(log: &Logger, memory: Memory, swap: Swap) {
    info!(log, "Received memory configuration information";
          "RAM size" => %format::display_information(memory.total()),
          "swap size" => %format::display_information(swap.total()));

    if swap.used() > swap.total() / 10 {
        warn!(
            log,
            "Non-negligible use of swap detected, make sure that it doesn't
             bias your benchmark!";
            "swap usage" => %format::display_information(swap.used())
        );
    }
}
