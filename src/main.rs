// FIXME: I probably need to have a word with the heim dev about this
#![type_length_limit="20000000"]

use async_std::prelude::*;

use futures_util::{pin_mut, try_join};

use heim::units::{
    frequency::megahertz,
    information::{bit, gibibyte, mebibyte, tebibyte},
    Information,
};

use std::collections::HashSet;


#[async_std::main]
async fn main() -> heim::Result<()> {
    // FIXME: Switch to a real logging system with timestamps. Maybe
    //        log+env_logger+kv? Or slog? Or another logger? Hierarchical and
    //        structured logging capabilities would be useful.
    // FIXME: Reduce reliance on Debug printout, use our own format
    println!("Probing host system characteristics...");

    let cpu_frequency = heim::cpu::frequency();
    let disk_partitions = heim::disk::partitions();
    let logical_cpus = heim::cpu::logical_count();
    let physical_cpus = heim::cpu::physical_count();
    let platform = heim::host::platform();
    let user_sessions = heim::host::users();
    let virt = heim::virt::detect();
    // TODO: Retrieve other "static" info: total memory, network interfaces,
    //       current process + initial process list, sensor range

    let (cpu_frequency, logical_cpus, physical_cpus, platform) =
        try_join!(cpu_frequency, logical_cpus, physical_cpus, platform)?;
    let virt = virt.await;  // FIXME: Ask heim author to make this consistent
    
    println!("- Host platform: {:#?}", platform);
    if let Some(virt) = virt {
        println!("WARNING: Virtualization platform {:?} detected, make sure \
                           that it doesn't bias the kind of benchmark that \
                           you are doing!", virt);
    }

    println!("- Logged-in user(s):");
    pin_mut!(user_sessions);
    let mut unique_usernames = HashSet::new();
    while let Some(user) = user_sessions.next().await {
        // TODO: On linux, decide if we want to collect OS-specific user info.
        //       Most of it seems useless, but I may try to print it out to
        //       check. And login process Pid could possibly be used to blame
        //       background load on another user. It's all speculative though.
        unique_usernames.insert(user?.username().to_owned());
    }
    for username in &unique_usernames {
        println!("    * {}", username);
    }
    if unique_usernames.len() > 1 {
        println!("WARNING: Multiple users detected, make sure other logged-in \
                           users are aware of your benchmarking activities!");
    }

    print!("- {} logical CPU(s)", logical_cpus);
    if let Some(physical_cpus) = physical_cpus {
        print!(", {} physical core(s)", physical_cpus);
    } else {
        print!(" physical core count is unknown");
    }
    // FIXME: On linux, query per-CPU frequency range, and print it instead of
    //        the global info if it varies between cores (rare, but can happen).
    print!(", frequency range is ");
    if let (Some(min), Some(max)) = (cpu_frequency.min(), cpu_frequency.max()) {
        println!("{} to {} MHz",
                 min.get::<megahertz>(), max.get::<megahertz>());
    } else {
        println!("unknown");
    }

    println!("- Active filesystem mount(s):");
    pin_mut!(disk_partitions);
    while let Some(partition) = disk_partitions.next().await {
        let partition = partition?;
        print!("    * {:?}, with ", partition);
        match heim::disk::usage(partition.mount_point()).await {
            Ok(usage) if usage.total() != Information::new::<bit>(0) => {
                let capacity = usage.total();
                print!("a total capacity of ");
                // FIXME: Check uom docs, there has to be a better way
                if capacity > Information::new::<tebibyte>(10) {
                    println!("{} TiB", capacity.get::<tebibyte>());
                } else if capacity > Information::new::<gibibyte>(10) {
                    println!("{} GiB", capacity.get::<gibibyte>());
                } else {
                    println!("{} MiB", capacity.get::<mebibyte>());
                }
            },
            Ok(_) => {
                println!("zero capacity (pseudo-filesystem?)");
            }
            Err(e) => {
                println!("capacity check error {}", e);
            }
        }
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
