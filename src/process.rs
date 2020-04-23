use chrono::{DateTime, Local};

use heim::{
    process::{Command, Pid, Process, ProcessError},
    units::{
        time::{nanosecond, second},
        Time,
    },
};

use slog::{debug, error, o, warn, Logger};

use std::{
    borrow::Cow,
    collections::{
        btree_set::BTreeSet,
        hash_map::{Entry, HashMap},
    },
    path::PathBuf,
    time::{Duration, SystemTime},
};

/// The process tree that is generated and printed during the initial report
#[derive(Default)]
struct ProcessTree {
    /// Roots of the process tree, which have no known parent
    roots: BTreeSet<Pid>,

    /// Nodes of the process tree (per-process info + children)
    nodes: HashMap<Pid, ProcessTreeNode>,
}

impl<ProcessInfoIter> From<ProcessInfoIter> for ProcessTree
where
    ProcessInfoIter: IntoIterator<Item = (Pid, Result<ProcessInfo, ProcessInfoError>)>,
{
    /// Build a process tree from per-process info
    fn from(process_info_iter: ProcessInfoIter) -> Self {
        // Setup our input iterator and process tree
        let process_info_iter = process_info_iter.into_iter();
        let mut process_tree = ProcessTree::default();
        if let Some(num_processes) = process_info_iter.size_hint().1 {
            process_tree.nodes.reserve(num_processes);
        }

        // Fill in the process tree's nodes
        for (pid, process_info) in process_info_iter {
            // Did we query this process' parent successfully?
            // If so, add it as a child of that parent process in the tree
            let parent_pid_result = process_info.as_ref().map(|info| info.parent_pid);
            if let Ok(Ok(parent_pid)) = parent_pid_result {
                let insert_result = process_tree
                    .nodes
                    .entry(parent_pid)
                    .or_insert(ProcessTreeNode {
                        // Use NoSuchProcess error as a placeholder to
                        // reduce tree data model complexity a little bit.
                        process_info: Err(ProcessInfoError::NoSuchProcess),
                        children: BTreeSet::new(),
                    })
                    .children
                    .insert(pid);
                assert!(insert_result, "Registered the same child twice!");
            }

            // Now, fill that process' node in the process tree
            match process_tree.nodes.entry(pid) {
                // No entry yet: either this process was seen before its children or
                // it does not have any child process.
                Entry::Vacant(vacant_entry) => {
                    vacant_entry.insert(ProcessTreeNode {
                        process_info,
                        children: BTreeSet::new(),
                    });
                }

                // An entry exists, most likely filled because a child was observed
                // before the parent and had to create its parent's entry. Check
                // that this is the case and fill in the corresponding node.
                Entry::Occupied(occupied_entry) => {
                    let old_process_info = std::mem::replace(
                        &mut occupied_entry.into_mut().process_info,
                        process_info,
                    );
                    assert!(
                        matches!(old_process_info, Err(ProcessInfoError::NoSuchProcess)),
                        "Invalid pre-existing process node info!"
                    );
                }
            }
        }

        // Enumerate the roots of the process tree, which have no known parents
        //
        // NOTE: Could build the tree root set dynamically above to avoid this
        //       second algorithmic pass, but it would make the code less clear
        //       while this is not a performance bottleneck right now.
        //
        for (&pid, node) in &process_tree.nodes {
            match &node.process_info.as_ref().map(|info| info.parent_pid) {
                // Process has a parent, so it's not a root. Skip it.
                Ok(Ok(_parent_pid)) => {}

                // Process has no known parent, register it as a process tree root.
                _ => {
                    let insert_result = process_tree.roots.insert(pid);
                    assert!(insert_result, "Registered the same root twice!");
                }
            }
        }

        process_tree
    }
}

impl ProcessTree {
    /// Log the contents of the process tree (for the benchmon startup report)
    pub fn log(&self, log: &Logger) {
        for &root_pid in &self.roots {
            self.log_subtree(&log, root_pid);
        }
    }

    /// Log a subtree of the process tree
    fn log_subtree(&self, log: &Logger, current_pid: Pid) {
        // Get the tree node associated with the current process
        let current_node = &self.nodes[&current_pid];

        // Log the info from that node
        match &current_node.process_info {
            Ok(process_info) => {
                let print_err =
                    |err: &ProcessInfoFieldError| Cow::from(format!("Unavailable ({:?})", err));
                let process_name = match &process_info.name {
                    Ok(name) => name.into(),
                    Err(err) => print_err(err),
                };
                let process_exe = match &process_info.exe {
                    Ok(exe) => {
                        if exe.iter().count() == 0 {
                            "None".into()
                        } else {
                            exe.to_string_lossy()
                        }
                    }
                    Err(err) => print_err(err),
                };
                let process_command = match &process_info.command {
                    Ok(command) => {
                        let args = command
                            .into_iter()
                            .map(|arg| arg.to_string_lossy())
                            .collect::<Vec<_>>();
                        if args.is_empty() {
                            "None".into()
                        } else {
                            args.join(" ").into()
                        }
                    }
                    Err(err) => print_err(err),
                };
                let process_create_time = match &process_info.create_time {
                    Ok(create_time) => {
                        let secs = create_time.get::<second>().floor();
                        let nsecs = create_time.get::<nanosecond>() - 1_000_000_000.0 * secs;
                        let duration = Duration::new(secs as u64, nsecs as u32);
                        let system_time = SystemTime::UNIX_EPOCH + duration;
                        let date_time = DateTime::<Local>::from(system_time);
                        format!("{}", date_time).into()
                    }
                    Err(err) => print_err(err),
                };
                debug!(log, "Found a process";
                       "pid" => current_pid,
                       "name" => %process_name,
                       "executable path" => %process_exe,
                       "command line" => %process_command,
                       "creation time" => %process_create_time);
            }

            Err(ProcessInfoError::AccessDenied) => {
                error!(log, "Found a process, but access to its info was denied";
                       "pid" => current_pid);
            }

            Err(ProcessInfoError::NoSuchProcess) => {
                debug!(log, "Found a nonexistent process (it likely vanished, \
                             or isn't a real system process)";
                       "pid" => current_pid);
            }

            Err(ProcessInfoError::ZombieProcess) => {
                warn!(log, "Found a process in the zombie state";
                      "pid" => current_pid);
            }
        }

        // Recursively log info about child nodes
        let children_log = log.new(o!("parent pid" => current_pid));
        for &child_pid in &current_node.children {
            self.log_subtree(&children_log, child_pid);
        }
    }
}

/// A node in the process tree from the initial report
struct ProcessTreeNode {
    /// Info about this process gathered during process enumeration
    ///
    /// If a process is first referred to as a parent of another process,
    /// Err(ProcessInfoError::NoSuchProcess) will be inserted as a placeholder
    /// value. This placeholder should eventually be replaced by the
    /// corresponding process enumeration result, unless...
    ///
    /// 1. An unlucky race condition occured and a process was seen during
    ///    process enumeration, but not the parent of that process.
    /// 2. The process of interest is a special PID that does not actually
    ///    map into a user-mode system process (like PID 0 on Linux).
    process_info: Result<ProcessInfo, ProcessInfoError>,

    /// Children of this process in the process tree
    children: BTreeSet<Pid>,
}

/// Result of a detailed initial process info query.
pub struct ProcessInfo {
    /// PID of the parent process
    parent_pid: Result<Pid, ProcessInfoFieldError>,

    /// Name of this process
    name: Result<String, ProcessInfoFieldError>,

    /// Path to this process' executable
    exe: Result<PathBuf, ProcessInfoFieldError>,

    /// Command line with which the process was invoked
    command: Result<Command, ProcessInfoFieldError>,

    /// Time at which the process was created, since Unix epoch
    // TODO: Convert to something like SystemTime instead
    create_time: Result<Time, ProcessInfoFieldError>,
}

/// Error which can occur while fetching a specific piece of process
/// information, without that invalidating the entire ProcessInfo struct.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoFieldError {
    /// Not enough permissions to query this ProcessInfo field.
    AccessDenied,
}

/// Error which invalidates the entire ProcessInfo query.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoError {
    /// Not enough permissions to query a Process struct.
    AccessDenied,

    /// The process exited during the query and doesn't exist anymore.
    NoSuchProcess,

    /// Same as above, but the parent didn't reclaim its exit status yet.
    ZombieProcess,
}

/// Starting from a Result of the process enumeration process, try to fetch
/// as much process info as possible, and produce a report on that.
///
/// The output is a three-layered Result cake:
///
/// - In the worst case, process enumeration failed badly and we couldn't
///   even get a Pid. In that case, aborting the enumeration is recommended.
/// - Even if we did get a Pid, we may not get more than that because the
///   target process has exited and all info was lost. In that case, the
///   enumeration returns the Pid and a ProcessInfoError.
/// - Finally, we may succeed in querying some info about a process but not
///   all of it because we do not have enough permissions. This is
///   particularly frequent when querying daemons. In that case, only the
///   corresponding ProcessInfo field will error out.
///
pub async fn get_process_info(
    enumeration_result: Result<Process, ProcessError>,
) -> heim::Result<(Pid, Result<ProcessInfo, ProcessInfoError>)> {
    match enumeration_result {
        // Process was correctly enumerated
        Ok(process) => {
            // Get its PID
            let pid = process.pid();

            // And now, we're going to query ProcessInfo fields one by one,
            // and here the error handling will get a little hairy...
            macro_rules! get_info_field {
                ($field_name:ident) => {
                    match process.$field_name().await {
                        // If we got the info, we bubble it up happily
                        Ok(info) => Ok(info),

                        // If we got an AccessDenied error, we expect the
                        // received pid to match the process we queried, and
                        // we consider that this failure only affects the active
                        // ProcessInfo field.
                        Err(ProcessError::AccessDenied(res_pid)) => {
                            assert_eq!(res_pid, pid);
                            Err(ProcessInfoFieldError::AccessDenied)
                        }

                        // If we got a NoSuchProcess or a ZombieProcess error,
                        // we consider it to invalidate the entire ProcessInfo
                        // struct, but still bubble up the Pid.
                        Err(ProcessError::NoSuchProcess(res_pid)) => {
                            assert_eq!(res_pid, pid);
                            return Ok((pid, Err(ProcessInfoError::NoSuchProcess)));
                        }
                        Err(ProcessError::ZombieProcess(res_pid)) => {
                            assert_eq!(res_pid, pid);
                            return Ok((pid, Err(ProcessInfoError::ZombieProcess)));
                        }

                        // If we got a Load error, we treat it as an
                        // unrecoverable heim failure, as advised by the heim
                        // documentation, and thus we abort the entire
                        // process enumeration query.
                        Err(ProcessError::Load(err)) => return Err(err),

                        // Since heim uses nonexhaustive enums, we must
                        // error out at runtime instead of at compile time
                        _ => unimplemented!("Unsupported process query error"),
                    }
                };
            }

            // Once we know how to get a ProcessInfo struct field, we know
            // how to get the whole ProcessInfo struct.
            macro_rules! get_info_struct {
                ( $($field_name:ident),* ) => {
                    Ok((
                        pid,
                        Ok(ProcessInfo {
                            $( $field_name: get_info_field!($field_name) ),*
                        })
                    ))
                }
            }
            get_info_struct!(parent_pid, name, exe, command, create_time)
        }

        // Process doesn't exist anymore, most likely some kind of race
        // condition happened during enumeration.
        Err(ProcessError::NoSuchProcess(pid)) => Ok((pid, Err(ProcessInfoError::NoSuchProcess))),

        // Process is a zombie (it has exited, so every info but its status
        // was discarded, the status itself will be discarded once the
        // parent process waits for that)
        Err(ProcessError::ZombieProcess(pid)) => Ok((pid, Err(ProcessInfoError::ZombieProcess))),

        // Not enough permission to get a Process struct, but still got a Pid
        //
        // This error condition may seem a little curious to you if you think of
        // Process as a Pid wrapper. But it can actually happen because on some
        // platforms, heim unconditionally queries process creation times in
        // order to make the Process struct unambiguously equality-comparable.
        Err(ProcessError::AccessDenied(pid)) => Ok((pid, Err(ProcessInfoError::AccessDenied))),

        // Unrecoverable heim failure upon loading process data, we didn't
        // even manage to get the process' Pid.
        Err(ProcessError::Load(err)) => Err(err),

        // Since heim uses nonexhaustive enums, we must error out at runtime
        // instead of at compile time when an unknown error happens
        _ => unimplemented!("Unsupported process query error"),
    }
}

/// Report on the host's running processes
pub fn log_report(log: &Logger, processes: Vec<(Pid, Result<ProcessInfo, ProcessInfoError>)>) {
    // Build a process tree and log its contents
    debug!(log, "Processing process tree...");
    let process_tree = ProcessTree::from(processes);
    process_tree.log(log);
}
