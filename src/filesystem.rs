use crate::format;

use heim::{
    disk::{Partition, Usage},
    units::information::byte,
};

use slog::{debug, info, Logger};

use std::collections::{BTreeMap, BTreeSet};

/// Report on the host's file system configuration
pub fn startup_report(
    log: &Logger,
    disk_partitions_and_usage: Vec<(Partition, heim::Result<Usage>)>,
) {
    // The OS APIs give us a list of filesystem mounts (at least on Unix), but
    // as performance engineers what we're really interested in are the physical
    // devices that back these mount points. Let's try to reverse-engineer that
    // information from mount properties...
    debug!(log, "Processing filesystem mount list...");
    let mut dev_to_mounts = BTreeMap::<_, BTreeSet<_>>::new();
    for (partition, usage) in disk_partitions_and_usage {
        // Disk capacity and disk usage will be used (if available) as a
        // last-resort disambiguation key for mounts with identical device name
        // and size (e.g. unrelated tmpfs mounts on Linux).
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
            Ok(capacity) => format!("{}", format::display_information(capacity)),
            Err(err) => format!("Unavailable ({})", err),
        };
        let formatted_filesystem = partition.file_system().as_str().to_owned();

        // Mount points and grouped and sorted by device name, then capacity,
        // then filesystem type, and finally the number of used bytes (which we
        // will not display, but can use as a disambiguation key for tmpfs).
        let mount_list = dev_to_mounts
            .entry((
                formatted_device,
                formatted_capacity,
                formatted_filesystem,
                known_used_bytes,
            ))
            .or_default();
        let insert_result = mount_list.insert(partition.mount_point().to_owned());
        assert!(insert_result, "Observed the same mount point twice!");
    }

    // Display the deduplicated filesystem-backing devices, with their mounts
    for ((device, capacity, file_system, _used_bytes), mount_list) in dev_to_mounts {
        info!(log, "Found a mounted device";
              "device name" => device,
              "capacity" => capacity,
              "file system" => file_system,
              "mount point(s)" => ?mount_list);
    }
}
