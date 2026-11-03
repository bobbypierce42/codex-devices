use codex_devices::{Error, Result};
use serde::Serialize;
use std::path::PathBuf;

pub(super) enum Target {
    Local { app_server: Option<PathBuf> },
    Peer { alias: String, app_server: PathBuf },
}

#[derive(Serialize)]
pub(super) struct Report {
    pub schema_version: u32,
    pub mode: &'static str,
    pub status: &'static str,
    pub transport: &'static str,
    pub peer: Option<String>,
    pub app_server: String,
    pub account_read_supported: bool,
    pub account_type: &'static str,
    pub auth_status_supported: bool,
    pub auth_method: &'static str,
    pub auth_status_error_code: Option<i64>,
    pub token_requested: bool,
    pub refresh_requested: bool,
    pub device_requests: u32,
    pub mutating_requests: u32,
    pub device_inventory_verified: bool,
}

impl Report {
    pub(super) fn exit_code(&self) -> i32 {
        if self.status == "metadata-only" { 0 } else { 4 }
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn audit(_: Target) -> Result<Report> {
    Err(Error::refused(
        "Native sign-in preflight currently supports a macOS controller only.",
    ))
}

#[cfg(target_os = "macos")]
pub(super) fn audit(target: Target) -> Result<Report> {
    macos::audit(target)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::path::{Component, Path};
    use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
    use std::time::{Duration, Instant};

    fn failed(message: &'static str) -> Error {
        Error::new("local-preflight", message, 4)
    }

    fn executable(explicit: Option<PathBuf>) -> Result<PathBuf> {
        if let Some(path) = explicit {
            if !path.is_absolute() || !path.is_file() {
                return Err(Error::new(
                    "usage",
                    "--app-server requires an existing absolute executable path.",
                    2,
                ));
            }
            return Ok(path);
        }
        let found: Vec<_> = ["/Applications/ChatGPT.app", "/Applications/Codex.app"]
            .into_iter()
            .filter_map(|app| {
                [
                    "Contents/Resources/codex-cli/bin/codex",
                    "Contents/Resources/codex",
                ]
                .into_iter()
                .map(|member| Path::new(app).join(member))
                .find(|p| p.is_file())
            })
            .collect();
        match found.as_slice() {
            [path] => Ok(path.clone()),
            [] => Err(Error::refused(
                "No bundled Codex app server was found. Choose its absolute path with --app-server.",
            )),
            _ => Err(Error::refused(
                "Several desktop apps were found. Choose the intended app server with --app-server.",
            )),
        }
    }

    fn peer_command(alias: &str, executable: &Path) -> Result<Command> {
        if alias.is_empty()
            || alias.len() > 253
            || !alias.starts_with(|c: char| c.is_ascii_alphanumeric())
            || !alias
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            return Err(Error::new(
                "usage",
                "--peer needs a simple SSH host alias or DNS name. Configure user and port in SSH config.",
                2,
            ));
        }
        let path = executable.to_str().filter(|path| {
            executable.is_absolute()
                && !path.chars().any(char::is_control)
                && !executable.components().any(|c| c == Component::ParentDir)
        }).ok_or_else(|| Error::new("usage", "The remote --app-server path must be absolute UTF-8 without controls or parent traversal.", 2))?;
        let remote = format!(
            "exec '{}' app-server --listen stdio://",
            path.replace('\'', "'\"'\"'")
        );
        let mut command = Command::new("/usr/bin/ssh");
        command.args([
            "-T",
            "-oBatchMode=yes",
            "-oStrictHostKeyChecking=yes",
            "-oUpdateHostKeys=no",
            "-oConnectTimeout=5",
            "-oPermitLocalCommand=no",
            "-oClearAllForwardings=yes",
            "-oForwardAgent=no",
            "-oForwardX11=no",
            "-oControlMaster=no",
            "-oControlPath=none",
            "--",
            alias,
            &remote,
        ]);
        Ok(command)
    }

    struct Session {
        child: Child,
        stdin: Option<ChildStdin>,
        stdout: ChildStdout,
        pending: Vec<u8>,
        received: usize,
        deadline: Instant,
    }

    impl Session {
        fn start(mut command: Command, timeout: Duration) -> Result<Self> {
            let mut child = command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|_| failed("Could not start the selected metadata transport."))?;
            let session = Self {
                stdin: child.stdin.take(),
                stdout: child.stdout.take().expect("piped stdout"),
                child,
                pending: Vec::new(),
                received: 0,
                deadline: Instant::now() + timeout,
            };
            let flags = fcntl_getfl(&session.stdout)
                .map_err(|_| failed("Could not configure the local protocol pipe."))?;
            fcntl_setfl(&session.stdout, flags | OFlags::NONBLOCK)
                .map_err(|_| failed("Could not configure the local protocol pipe."))?;
            Ok(session)
        }

        fn send(&mut self, value: Value) -> Result<()> {
            let bytes = serde_json::to_vec(&value).expect("fixed protocol request");
            let pipe = self.stdin.as_mut().expect("owned protocol input");
            pipe.write_all(&bytes)
                .and_then(|()| pipe.write_all(b"\n"))
                .and_then(|()| pipe.flush())
                .map_err(|_| failed("The local app server stopped accepting metadata requests."))
        }

        fn response(&mut self, id: u64) -> Result<Value> {
            let mut chunk = [0_u8; 4096];
            while Instant::now() < self.deadline {
                if let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
                    let line: Vec<_> = self.pending.drain(..=end).collect();
                    let value: Value = serde_json::from_slice(&line).map_err(|_| {
                        failed("The local app server returned invalid protocol JSON.")
                    })?;
                    if value.get("method").is_some() && value.get("id").is_some() {
                        return Err(failed(
                            "An unsolicited server request stopped the read-only preflight.",
                        ));
                    }
                    if value["id"].as_u64() == Some(id) {
                        if value.get("result").is_some() == value.get("error").is_some() {
                            return Err(failed(
                                "The local app server returned an ambiguous response.",
                            ));
                        }
                        return Ok(value);
                    }
                    continue;
                }
                match self.stdout.read(&mut chunk) {
                    Ok(0) => {
                        return Err(failed(
                            "The local app server ended before its metadata response.",
                        ));
                    }
                    Ok(length) => {
                        self.received += length;
                        if self.received > 1024 * 1024 || self.pending.len() + length > 65536 {
                            return Err(failed(
                                "The local protocol response exceeded its size budget.",
                            ));
                        }
                        self.pending.extend_from_slice(&chunk[..length]);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(_) => return Err(failed("Could not read the local metadata response.")),
                }
            }
            Err(failed("The local sign-in preflight timed out."))
        }
    }

    impl Drop for Session {
        fn drop(&mut self) {
            self.stdin.take();
            for _ in 0..20 {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    fn label(value: &Value, allowed: &[&'static str]) -> &'static str {
        allowed
            .iter()
            .copied()
            .find(|s| value.as_str() == Some(*s))
            .unwrap_or("unknown")
    }

    pub(super) fn audit(target: Target) -> Result<Report> {
        let (executable, peer, command) = match target {
            Target::Local { app_server } => {
                let executable = executable(app_server)?;
                let mut command = Command::new(&executable);
                command.args(["app-server", "--listen", "stdio://"]);
                (executable, None, command)
            }
            Target::Peer { alias, app_server } => {
                let command = peer_command(&alias, &app_server)?;
                (app_server, Some(alias), command)
            }
        };
        let mut session = Session::start(command, Duration::from_secs(15))?;
        session.send(json!({"id":1,"method":"initialize","params":{
            "clientInfo":{"name":"codex-devices-preflight","version":env!("CARGO_PKG_VERSION")},
            "capabilities":{"experimentalApi":true,"explicitGatewayOauth":true}}}))?;
        if session.response(1)?.get("error").is_some() {
            return Err(failed(
                "The selected app server does not support this metadata preflight.",
            ));
        }
        session.send(json!({"method":"initialized"}))?;
        session.send(json!({"id":2,"method":"account/read","params":{"refreshToken":false}}))?;
        let account = session.response(2)?;
        session.send(json!({"id":3,"method":"getAuthStatus","params":{"includeToken":false,"refreshToken":false}}))?;
        let auth = session.response(3)?;
        if auth["result"]
            .get("authToken")
            .is_some_and(|token| !token.is_null())
        {
            return Err(failed(
                "The server unexpectedly included a token. Its value was discarded; preflight stopped.",
            ));
        }
        let account_type = label(
            &account["result"]["account"]["type"],
            &["chatgpt", "apiKey", "amazonBedrock"],
        );
        let auth_method = label(
            &auth["result"]["authMethod"],
            &["chatgpt", "chatgptAuthTokens", "apikey"],
        );
        let complete = account.get("result").is_some()
            && auth.get("result").is_some()
            && account_type == "chatgpt"
            && ["chatgpt", "chatgptAuthTokens"].contains(&auth_method);
        Ok(Report {
            schema_version: 1,
            mode: if peer.is_some() {
                "peer-sign-in-preflight"
            } else {
                "local-sign-in-preflight"
            },
            status: if complete { "metadata-only" } else { "partial" },
            transport: if peer.is_some() {
                "ssh-stdio"
            } else {
                "local-stdio"
            },
            peer,
            app_server: executable.to_string_lossy().into_owned(),
            account_read_supported: account.get("result").is_some(),
            account_type,
            auth_status_supported: auth.get("result").is_some(),
            auth_method,
            auth_status_error_code: auth["error"]["code"].as_i64(),
            token_requested: false,
            refresh_requested: false,
            device_requests: 0,
            mutating_requests: 0,
            device_inventory_verified: false,
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn metadata_deadline_expires_without_reading_or_executing_authentication() {
            let mut session = Session::start(Command::new("/bin/cat"), Duration::ZERO).unwrap();
            assert_eq!(
                session.response(1).unwrap_err().message,
                "The local sign-in preflight timed out."
            );
        }

        #[test]
        fn ssh_command_keeps_host_checks_and_quotes_the_remote_executable() {
            let path = Path::new("/Applications/Owner's App;$(touch forbidden)/codex");
            let command = peer_command("demo-mac.example", path).unwrap();
            assert_eq!(command.get_program(), "/usr/bin/ssh");
            let args: Vec<_> = command.get_args().map(|s| s.to_str().unwrap()).collect();
            assert_eq!(
                args,
                [
                    "-T",
                    "-oBatchMode=yes",
                    "-oStrictHostKeyChecking=yes",
                    "-oUpdateHostKeys=no",
                    "-oConnectTimeout=5",
                    "-oPermitLocalCommand=no",
                    "-oClearAllForwardings=yes",
                    "-oForwardAgent=no",
                    "-oForwardX11=no",
                    "-oControlMaster=no",
                    "-oControlPath=none",
                    "--",
                    "demo-mac.example",
                    "exec '/Applications/Owner'\"'\"'s App;$(touch forbidden)/codex' app-server --listen stdio://",
                ]
            );
        }

        #[test]
        fn ssh_validation_refuses_option_injection_and_ambiguous_paths() {
            for alias in [
                "",
                "-oProxyCommand=evil",
                "demo;evil",
                "user@demo",
                "demo\nother",
                "::1",
            ] {
                assert_eq!(
                    peer_command(alias, Path::new("/invented/codex"))
                        .unwrap_err()
                        .exit_code,
                    2
                );
            }
            for path in ["codex", "/invented/../codex", "/invented/codex\nother"] {
                assert_eq!(
                    peer_command("demo-mac", Path::new(path))
                        .unwrap_err()
                        .exit_code,
                    2
                );
            }
        }
    }
}
