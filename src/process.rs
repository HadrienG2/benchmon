use heim::{
    process::{Command, Pid, Process, ProcessError},
    units::Time,
};

use slog::{debug, info, o, warn, Logger};

use std::{
    collections::{
        btree_set::BTreeSet,
        hash_map::{Entry, HashMap},
    },
    path::PathBuf,
};

/// Result of a detailed initial process info query.
pub struct ProcessInfo {
    /// PID of the parent process
    pub parent_pid: Result<Pid, ProcessInfoFieldError>,

    /// Name of this process
    pub name: Result<String, ProcessInfoFieldError>,

    /// Path to this process' executable
    pub exe: Result<PathBuf, ProcessInfoFieldError>,

    /// Command line with which the process was invoked
    pub command: Result<Command, ProcessInfoFieldError>,

    /// Time at which the process was created, since Unix epoch
    // TODO: Convert to something like SystemTime instead
    pub create_time: Result<Time, ProcessInfoFieldError>,
}

/// Error which can occur while fetching a specific piece of process
/// information, without that invalidating the entire ProcessInfo struct.
//
// NOTE: If we got a NoSuchProcess or a ZombieProcess error, we consider
//       that the entire ProcessInfo is bogus: the process has exited, and
//       if we queried again, we'd get the same error for all info.
//
//       If we got a Load error, we consider that the entire process
//       enumeration process is bogus and will abort that higher-level
//       process. This follows the "this error is unrecoverable" design
//       principle of heim::Error.
//
//       This only leaves AccessDenied errors as something which solely
//       affects the current process info field, for now.
//
//       Also, as far as I know, when querying a process property, the
//       error's `pid` field can only contain the Pid of the target process.
//       I'll cross-check that assumption with an assertion.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoFieldError {
    /// Not enough permissions to query this ProcessInfo field.
    AccessDenied,
}

/// Error which invalidates the entire ProcessInfo query.
//
// NOTE: As far as I know, AccessDenied errors cannot upon while listing
//       processes, but only while enumerating their individual fields. But
//       I will cross-check that with an assertion.
//
//       Load errors are treated as fatal errors, and therefore abort the
//       enumeration of the entire process tree, not just the current
//       process.
//
//       Also, as far as I know, when enumerating processes or querying
//       their properties, the error's `pid` field can only contain the Pid
//       of the target process. I'll cross-check that assumption with an
//       assertion whenever possible.
#[non_exhaustive]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProcessInfoError {
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
///   enumeration returns the Pid and a process exit error.
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

        // Not enough permission to get a Process struct (?)
        Err(ProcessError::AccessDenied(_pid)) => {
            // As far as I know, this error cannot happen. The Process type
            // is basically a thin Pid wrapper, so if you don't have enough
            // permissions to get the wrapper, you shouldn't have enough
            // permissions to get a Pid either, and thus a Load error should
            // occur, not an AccessDenied one. Therefore it doesn't make
            // sense to account for this error in ProcessInfoError.
            unreachable!()
        }

        // Unrecoverable heim failure upon loading process data, we didn't
        // even manage to get the process' Pid.
        Err(ProcessError::Load(err)) => Err(err),

        // Since heim uses nonexhaustive enums, we must
        // error out at runtime instead of at compile time
        _ => unimplemented!("Unsupported process query error"),
    }
}

/// Report on the host's running processes
pub fn log_report(log: &Logger, processes: Vec<(Pid, Result<ProcessInfo, ProcessInfoError>)>) {
    /// A node in the process tree
    struct ProcessNode {
        /// The heim Process object from process enumeration
        ///
        /// If a process is first referred to as a parent of another process,
        /// Err(ProcessError::NoSuchProcess(pid)) will be inserted as a
        /// placeholder value. This placeholder should eventually be replaced by
        /// the corresponding process enumeration result, unless...
        ///
        /// 1. An unlucky race condition occured and a process was seen during
        ///    process enumeration, but not the parent of that process.
        /// 2. The process of interest is a special PID that does not actually
        ///    map into a user-mode system process (like PID 0 on Linux).
        process_info_result: Result<ProcessInfo, ProcessInfoError>,

        /// Children of this process in the process tree
        children: BTreeSet<Pid>,
    }

    // Build a process tree
    let mut process_tree_nodes = HashMap::<Pid, ProcessNode>::new();
    for (pid, process_info_result) in processes {
        // Did we query this process' parent successfully?
        // If so, add it as a child of that parent process in the tree
        let parent_pid_result = process_info_result.as_ref().map(|info| info.parent_pid);
        if let Ok(Ok(parent_pid)) = parent_pid_result {
            let insert_result = process_tree_nodes
                .entry(parent_pid)
                .or_insert(ProcessNode {
                    // Use NoSuchProcess error as a placeholder to
                    // reduce tree data model complexity a little bit.
                    process_info_result: Err(ProcessInfoError::NoSuchProcess),
                    children: BTreeSet::new(),
                })
                .children
                .insert(pid);
            assert!(insert_result, "Registered the same child twice!");
        }

        // Now, fill that process' node in the process tree
        match process_tree_nodes.entry(pid) {
            // No entry yet: either this process was seen before its children or
            // it does not have any child process.
            Entry::Vacant(vacant_entry) => {
                vacant_entry.insert(ProcessNode {
                    process_info_result,
                    children: BTreeSet::new(),
                });
            }

            // An entry exists, most likely filled because a child was observed
            // before the parent and had to create its parent's entry. Check
            // that this is the case and fill in the corresponding node.
            Entry::Occupied(occupied_entry) => {
                let old_process_info_result = std::mem::replace(
                    &mut occupied_entry.into_mut().process_info_result,
                    process_info_result,
                );
                assert!(
                    matches!(
                        old_process_info_result,
                        Err(ProcessInfoError::NoSuchProcess)
                    ),
                    "Invalid pre-existing process node info!"
                );
            }
        }
    }

    // Enumerate the roots of the process tree, which have no known parents
    let mut process_tree_roots = BTreeSet::<Pid>::new();
    for (&pid, node) in &process_tree_nodes {
        match &node
            .process_info_result
            .as_ref()
            .map(|info| info.parent_pid)
        {
            // Process has a parent, so it's not a root. Skip it.
            Ok(Ok(_parent_pid)) => {}

            // Process has no known parent, register it as a process tree root.
            _ => {
                let insert_result = process_tree_roots.insert(pid);
                assert!(insert_result, "Registered the same root twice!");
            }
        }
    }

    // We are now ready to recursively print the process tree
    fn print_process_tree(
        log: &Logger,
        current_pid: Pid,
        process_tree_nodes: &HashMap<Pid, ProcessNode>,
    ) {
        // Get the tree node associated with the current process
        let current_node = &process_tree_nodes[&current_pid];

        // Display that node
        match &current_node.process_info_result {
            Ok(process_info) => {
                info!(log, "Found a process";
                      // FIXME: Go beyond debug repr, ideally try to aggregate
                      //        some errors like nonexistent or zombie too.
                      "pid" => current_pid,
                      "name" => ?process_info.name,
                      "executable path" => ?process_info.exe,
                      "command line" => ?process_info.command,
                      "creation time" => ?process_info.create_time);
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

        // Recursively print child nodes
        let children_log = log.new(o!("parent pid" => current_pid));
        for &child_pid in &current_node.children {
            print_process_tree(&children_log, child_pid, process_tree_nodes);
        }
    }
    //
    for root_pid in process_tree_roots {
        print_process_tree(&log, root_pid, &process_tree_nodes);
    }
}
