// FIXME: I probably need to have a word with the heim dev about this
#![type_length_limit="20000000"]

use async_std::prelude::*;

use futures_util::{pin_mut, try_join};

use heim::units::{
    frequency::megahertz,
    information::{byte, gigabyte, kilobyte, megabyte, terabyte},
    thermodynamic_temperature::degree_celsius,
    Information,
};

use std::collections::HashMap;


#[async_std::main]
async fn main() -> heim::Result<()> {
    // FIXME: Switch to a real logging system with timestamps. Maybe
    //        log+env_logger+kv? Or slog? Or another logger? Hierarchical and
    //        structured logging capabilities would be useful.
    println!("Probing host system characteristics...");

    let cpu_frequency = heim::cpu::frequency();
    let disk_partitions = heim::disk::partitions();
    let logical_cpus = heim::cpu::logical_count();
    let memory = heim::memory::memory();
    let network_interfaces = heim::net::nic();
    let physical_cpus = heim::cpu::physical_count();
    let platform = heim::host::platform();
    let swap = heim::memory::swap();
    let temperatures = heim::sensors::temperatures();
    let user_sessions = heim::host::users();
    let virt = heim::virt::detect();
    // TODO: Retrieve other "static" info: current process + initial processes

    let (cpu_frequency, logical_cpus, memory, physical_cpus, platform, swap) =
        try_join!(cpu_frequency, logical_cpus, memory, physical_cpus, platform, swap)?;
    let virt = virt.await;  // FIXME: Ask heim author to make this consistent
    
    println!("- Host platform is {} ({} {} {})",
             platform.hostname(),
             platform.system(),
             platform.release(),
             platform.version());
    if let Some(virt) = virt {
        println!("WARNING: Virtualization host {:?} detected, make sure that \
                           it doesn't bias your benchmark!", virt);
    }

    println!("- Logged-in user(s):");
    pin_mut!(user_sessions);
    let mut usernames_to_sessions = HashMap::new();
    while let Some(user) = user_sessions.next().await {
        // TODO: On Linux, decide if we want to collect OS-specific user info.
        //       Most of it seems useless, but I may try to print it out to
        //       check. And login process Pid could possibly be used to blame
        //       background load on another user. It's all speculative though.
        let username = user?.username().to_owned();
        *usernames_to_sessions.entry(username).or_insert(0) += 1;
    }
    for (username, &session_count) in &usernames_to_sessions {
        print!("    * {}", username);
        if session_count > 1 {
            print!(" ({} sessions)", session_count);
        }
        println!();
    }
    if usernames_to_sessions.len() > 1 {
        println!("WARNING: Multiple users detected, make sure other logged-in \
                           users keep the system quiet during benchmarks!");
    }

    print!("- {} logical CPU(s)", logical_cpus);
    if let Some(physical_cpus) = physical_cpus {
        print!(", {} physical core(s)", physical_cpus);
    } else {
        print!(" physical core count is unknown");
    }
    print!(", architecture is {:?}", platform.architecture());
    // FIXME: On linux, query per-CPU frequency range, and print it instead of
    //        the global info if it varies between cores (rare, but can happen,
    //        especially in embedded architectures).
    print!(", frequency range is ");
    if let (Some(min), Some(max)) = (cpu_frequency.min(), cpu_frequency.max()) {
        println!("{} to {} MHz",
                 min.get::<megahertz>(), max.get::<megahertz>());
    } else {
        println!("unknown");
    }

    println!("- {} of RAM, {} of swap",
           format_information(memory.total()),
           format_information(swap.total()));
    if swap.used() > swap.total() / 10 {
        print!("WARNING: Non-negligible use of swap ({}) detected, make sure \
                         that it doesn't bias your benchmark!",
               format_information(swap.used()));
    }

    println!("- Filesystem mount(s):");
    pin_mut!(disk_partitions);
    while let Some(partition) = disk_partitions.next().await {
        let partition = partition?;
        // FIXME: Get rid of this Debug printout
        print!("    * {:?}, with ", partition);
        match heim::disk::usage(partition.mount_point()).await {
            Ok(usage) if usage.total() != Information::new::<byte>(0) => {
                println!("a capacity of {}", format_information(usage.total()));
            },
            Ok(_) => {
                println!("zero capacity (likely a pseudo-filesystem)");
            }
            Err(e) => {
                println!("failing capacity check ({})", e);
            }
        }
    }

    println!("- Network interface(s):");
    pin_mut!(network_interfaces);
    while let Some(nic) = network_interfaces.next().await {
        // FIXME: Get rid of this Debug printout
        println!("    * {:?}", nic?);
    }

    println!("- Temperature sensor(s):");
    pin_mut!(temperatures);
    while let Some(sensor) = temperatures.next().await {
        let sensor = sensor?;
        print!("    * ");
        if let Some(label) = sensor.label() {
            print!("\"{}\"", label);
        } else {
            print!("Unlabeled sensor");
        }
        print!(" from unit \"{}\" (", sensor.unit());
        if let Some(high) = sensor.high() {
            print!("high: {} °C", high.get::<degree_celsius>());
        } else {
            print!("no high trip point");
        }
        print!(", ");
        if let Some(critical) = sensor.critical() {
            print!("critical: {} °C", critical.get::<degree_celsius>());
        } else {
            print!("no critical trip point");
        }
        println!(")");
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


fn format_information(quantity: Information) -> String {
    // FIXME: This can be optimized with a log-based jump table, and probably
    //        deduplicated as well if I think hard enough about it.
    if quantity > Information::new::<terabyte>(1) {
        let terabytes = quantity.get::<terabyte>();
        let gigabytes = quantity.get::<gigabyte>() - 1000 * terabytes;
        format!("{}.{:03} TB", terabytes, gigabytes)
    } else if quantity > Information::new::<gigabyte>(1) {
        let gigabytes = quantity.get::<gigabyte>();
        let megabytes = quantity.get::<megabyte>() - 1000 * gigabytes;
        format!("{}.{:03} GB", gigabytes, megabytes)
    } else if quantity > Information::new::<megabyte>(1) {
        let megabytes = quantity.get::<megabyte>();
        let kilobytes = quantity.get::<kilobyte>() - 1000 * megabytes;
        format!("{}.{:03} MB", megabytes, kilobytes)
    } else if quantity > Information::new::<kilobyte>(1) {
        let kilobytes = quantity.get::<kilobyte>();
        let bytes = quantity.get::<byte>() - 1000 * kilobytes;
        format!("{}.{:03} kB", kilobytes, bytes)
    } else {
        format!("{} B", quantity.get::<byte>())
    }
}

