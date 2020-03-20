# BenchMon - A benchmarking-oriented system monitor

The Linux community has access to a great wealth of system monitoring tool, but
as a software performance analyst, I have often found myself longing for a more
specialized hybrid between a system monitor and benchmark harness, which would
ideally fulfill all of the following criteria:

- Works on your benchmarking node in all of its special snowflake glory:
    * No graphics stack
    * Minimal open ports, possibly not even SSH (batch scheduler)
    * Ancient kernel and glibc versions
    * Unpleasant system administration policy
- Clear decoupling between "online" `stdout` use and actual data collection:
    * `stdout` is not the main output of the system monitor, only a facility for
      making sure that benchmarks are running well and tracking their progress.
    * Full system activity logs are emitted into CSV files for post-processing.
    * Can tune `stdout` output to an arbitrarily low refresh rate, or turn it
      off entirely if it adds too much system background noise.
    * Can tune data collection rate arbitrarily high without spamming `stdout`.
    * When RAM usage is acceptable, measurements can be integrally buffered in
      RAM, and only emitted after all benchmarks have been executed.
- Supports all the system activity metrics you want to know about:
    * CPU frequencies and utilization: overall, per-core, per-use, per-process
    * RAM and swap utilization: overall, per-use, per-process
    * Disk activity (bytes and IOPS) and utilization: hardware and per-process
    * Network activity (bytes, packets, errors, drops): hardware and per-process
    * Sensors (only temperature for now)
    * Performance monitoring counters and kernel tracing info.
    * Composite metrics deduced from the above data sources, such as average
      bytes per I/O operation or aggregated I/O across processes.
    * Choose which metrics you want to monitor for minimal overhead!
- In addition to "passive" monitoring, also works as a benchmark runner:
    * Baseline system load check.
    * Warm-up benchmark runs, based on elapsed time and run count criteria.
    * Multiple benchmark runs, with separate data collection.
    * Asymptotic scaling tests.
    * Produces basic statistics and fancy markdown/asciidoc reports.
    * Points out your most likely hardware bottlenecks, overall and over time.
- Watches and warns about common "benchmark smells" automatically:
    * Suspicious system configuration (multi-user activity, heavy swap usage,
      virtualization...)
    * Abnormal background system load (with automatic process blaming).
    * High metrics variance across benchmark executions.

Now, that's an ambitious project. I'm not sure if it will succeed, and even if
it does, I suspect that I'll need to scrap some of the above goals to make it
doable. But if this project works out, I think it will be of interest to other
people besides just me...