use crate::config::Config;
use crate::engine::EngineHandle;
use crate::Metrics;
use bytes::Bytes;
use recall_core::command::{metadata, ClientCommand, Command};
use recall_protocol::Reply;
use std::sync::atomic::Ordering;
use subtle::ConstantTimeEq;

pub(crate) struct Session {
    id: u64,
    authenticated: bool,
    name: Option<Bytes>,
    library_name: Option<Bytes>,
    library_version: Option<Bytes>,
}

impl Session {
    pub(crate) fn new(id: u64, config: &Config) -> Self {
        Self {
            id,
            authenticated: config.password.is_none(),
            name: None,
            library_name: None,
            library_version: None,
        }
    }

    pub(crate) async fn execute(
        &mut self,
        command: Command,
        engine: &EngineHandle,
        config: &Config,
        metrics: &Metrics,
    ) -> (Reply, bool) {
        match command {
            Command::Auth { username, password } => {
                let reply = match authenticate(config, username.as_deref(), &password) {
                    Ok(()) => {
                        self.authenticated = true;
                        Reply::ok()
                    }
                    Err(reply) => reply,
                };
                return (reply, false);
            }
            Command::Hello { protocol, auth, name } => {
                if protocol.is_some_and(|version| version != 2) {
                    return (Reply::error("NOPROTO unsupported protocol version"), false);
                }
                if let Some((username, password)) = auth {
                    if let Err(reply) = authenticate(config, Some(&username), &password) {
                        return (reply, false);
                    }
                    self.authenticated = true;
                }
                if !self.authenticated {
                    return (Reply::error("NOAUTH Authentication required."), false);
                }
                if let Some(name) = name {
                    self.name = nonempty(name);
                }
                return (self.hello_reply(), false);
            }
            _ if !self.authenticated => {
                return (Reply::error("NOAUTH Authentication required."), false);
            }
            _ => {}
        }

        let reply = match command {
            Command::Data(operation) => engine.execute(operation).await,
            Command::Ping(None) => Reply::Simple(Bytes::from_static(b"PONG")),
            Command::Ping(Some(value)) | Command::Echo(value) => Reply::bulk(value),
            Command::Quit => return (Reply::ok(), true),
            Command::Select(0) => Reply::ok(),
            Command::Select(_) => Reply::error("ERR DB index is out of range"),
            Command::Client(ClientCommand::GetName) => Reply::Bulk(self.name.clone()),
            Command::Client(ClientCommand::SetName(name)) => {
                self.name = nonempty(name);
                Reply::ok()
            }
            Command::Client(ClientCommand::SetInfo { library_name, value }) => {
                if library_name {
                    self.library_name = nonempty(value);
                } else {
                    self.library_version = nonempty(value);
                }
                Reply::ok()
            }
            Command::Metadata(command) => metadata(command),
            Command::Info(section) => info(section.as_deref(), engine, metrics).await,
            Command::Auth { .. } | Command::Hello { .. } => unreachable!("handled before authentication gate"),
        };
        (reply, false)
    }

    fn hello_reply(&self) -> Reply {
        Reply::Array(vec![
            Reply::bulk("server"), Reply::bulk("recall"),
            Reply::bulk("version"), Reply::bulk(env!("CARGO_PKG_VERSION")),
            Reply::bulk("proto"), Reply::Integer(2),
            Reply::bulk("id"), Reply::Integer(self.id as i64),
            Reply::bulk("mode"), Reply::bulk("standalone"),
            Reply::bulk("role"), Reply::bulk("master"),
            Reply::bulk("modules"), Reply::Array(vec![]),
        ])
    }
}

fn nonempty(value: Bytes) -> Option<Bytes> {
    if value.is_empty() { None } else { Some(value) }
}

fn authenticate(config: &Config, username: Option<&[u8]>, supplied: &[u8]) -> Result<(), Reply> {
    let Some(expected) = &config.password else {
        return Err(Reply::error("ERR AUTH called without any password configured for the default user."));
    };
    let password_matches = bool::from(expected.as_ref().ct_eq(supplied));
    let user_matches = username.is_none_or(|name| name == b"default");
    if password_matches && user_matches {
        Ok(())
    } else {
        Err(Reply::error("WRONGPASS invalid username-password pair or user is disabled."))
    }
}

async fn info(section: Option<&[u8]>, engine: &EngineHandle, metrics: &Metrics) -> Reply {
    let include = |name: &[u8]| section.is_none_or(|value| {
        value.eq_ignore_ascii_case(b"all") || value.eq_ignore_ascii_case(b"default") || value.eq_ignore_ascii_case(name)
    });
    let mut output = String::new();
    if include(b"server") {
        output.push_str(&format!(
            "# Server\r\nserver_name:recall\r\nrecall_version:{}\r\nmode:standalone\r\npersistence:memory-only\r\nengine_workers:{}\r\n",
            env!("CARGO_PKG_VERSION"), engine.worker_count(),
        ));
    }
    if include(b"clients") {
        output.push_str(&format!("# Clients\r\nconnected_clients:{}\r\n", metrics.active_connections.load(Ordering::Relaxed)));
    }
    if include(b"stats") {
        output.push_str(&format!(
            "# Stats\r\ntotal_connections_received:{}\r\ntotal_commands_processed:{}\r\nrejected_connections:{}\r\nprotocol_errors:{}\r\nio_timeouts:{}\r\n",
            metrics.total_connections.load(Ordering::Relaxed),
            metrics.total_commands.load(Ordering::Relaxed),
            metrics.rejected_connections.load(Ordering::Relaxed),
            metrics.protocol_errors.load(Ordering::Relaxed),
            metrics.io_timeouts.load(Ordering::Relaxed),
        ));
    }
    if include(b"memory") {
        let stats = engine.stats().await;
        output.push_str("# Memory\r\naccounting:live-key-value-payload-only\r\n");
        output.push_str(&format!(
            "payload_bytes:{}\r\nallocated_metadata_model:preallocated-per-owner\r\nphysical_keys:{}\r\nexpiring_keys:{}\r\n",
            stats.iter().map(|owner| owner.payload_bytes).sum::<usize>(),
            stats.iter().map(|owner| owner.keys).sum::<usize>(),
            stats.iter().map(|owner| owner.expiring_keys).sum::<usize>(),
        ));
        for (index, owner) in stats.iter().enumerate() {
            output.push_str(&format!(
                "owner_{index}:keys={},payload_bytes={},key_limit={},payload_limit={},active_expired={},applied={}\r\n",
                owner.keys, owner.payload_bytes, owner.max_keys, owner.max_payload_bytes, owner.expired_keys, owner.applied_commands,
            ));
        }
    }
    Reply::bulk(output)
}

