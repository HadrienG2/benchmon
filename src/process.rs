use heim::{
    process::{Command, Pid, Process, ProcessError},
    units::Time,
};

use std::path::PathBuf;

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
                        // we consider that this failure only affects the
                        // active ProcessInfo field.
                        Err(ProcessError::AccessDenied(res_pid)) => {
                            assert_eq!(res_pid, pid);
                            Err(ProcessInfoFieldError::AccessDenied)
                        }

                        // If we got a NoSuchProcess or a ZombieProcess
                        // error, we consider it to invalidate the entire
                        // ProcessInfo struct, but still bubble up the Pid.
                        Err(ProcessError::NoSuchProcess(res_pid)) => {
                            assert_eq!(res_pid, pid);
                            return Ok((pid, Err(ProcessInfoError::NoSuchProcess)));
                        }
                        Err(ProcessError::ZombieProcess(res_pid)) => {
                            assert_eq!(res_pid, pid);
                            return Ok((pid, Err(ProcessInfoError::ZombieProcess)));
                        }

                        // If we got a Load error, we treat it as an
                        // unrecoverable heim failure, as advised by the
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
        Err(ProcessError::AccessDenied(_pid)) =>
        // As far as I know, this error cannot happen. The Process type
        // is basically a thin Pid wrapper, so if you don't have enough
        // permissions to get the wrapper, you shouldn't have enough
        // permissions to get a Pid either, and thus a Load error should
        // occur, not an AccessDenied one. Therefore it doesn't make
        // sense to account for this error in ProcessInfoError.
        {
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
