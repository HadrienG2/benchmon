use heim::host::{Pid, User};

use slog::{debug, info, o, warn, Logger};

use std::collections::{BTreeMap, BTreeSet};

// FIXME: Make Heim expose this type
type SessionId = i32;

/// What we know about a logged-in system user
#[derive(Default)]
struct UserStats {
    /// Total number of connections opened by this user
    connection_count: usize,

    /// Breakdown of these connections into sessions and login processes
    /// (This data is, for now, only available on Linux)
    sessions_to_pids: Option<BTreeMap<SessionId, BTreeSet<Pid>>>,
}

/// Report on the host's open user sessions
pub fn startup_report(log: &Logger, user_connections: Vec<User>) {
    // The OS APIs give us a list of active user connections, when what we
    // actually want is a breakdown of these connections by user, and by user
    // session on OSes that have that concept. Let's build that.
    debug!(log, "Processing user connection list...");
    let mut usernames_to_stats = BTreeMap::<String, UserStats>::new();
    for connection in user_connections {
        let username = connection.username().to_owned();
        let user_log = log.new(o!("username" => username.clone()));
        debug!(user_log, "Found a user connection");

        let user_stats = usernames_to_stats.entry(username).or_default();
        user_stats.connection_count += 1;

        #[cfg(target_os = "linux")]
        {
            use heim::host::os::linux::UserExt;
            debug!(user_log,
                   "Got Linux-specific connection details";
                   "login process PID" => connection.pid(),
                   "(pseudo-)tty name" => connection.terminal(),
                   "terminal identifier" => connection.id(),
                   "remote hostname" => connection.hostname(),
                   "remote IP address" => ?connection.address(),
                   "session ID" => connection.session_id());
            let session_stats = user_stats
                .sessions_to_pids
                .get_or_insert_with(Default::default)
                .entry(connection.session_id())
                .or_default();
            let insert_result = session_stats.insert(connection.pid());
            assert!(insert_result, "Observed the same login PID twice!");
        }
    }

    // Display the previously computed breakdown of user connections
    for (username, stats) in &mut usernames_to_stats {
        let user_log = log.new(o!("username" => username.clone()));
        info!(user_log, "Found a logged-in user";
              "open connection count" => stats.connection_count);
        if let Some(ref mut sessions_to_pids) = &mut stats.sessions_to_pids {
            for (session_id, login_pids) in sessions_to_pids {
                info!(user_log,
                      "Got details of a user session";
                      "session ID" => session_id,
                      "login process PID(s)" => ?login_pids);
            }
        }
    }

    // Warn if other users are active on this system
    if usernames_to_stats.len() > 1 {
        warn!(
            log,
            "Detected multiple logged-in users, make sure others keep the \
             system quiet while your benchmarks are running!"
        );
    }
}
