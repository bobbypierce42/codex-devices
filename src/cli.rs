use codex_devices::{DEMO, Error, RenameChange, Result, read_input};
use std::ffi::OsString;
use std::path::PathBuf;

pub(super) enum InputSource {
    Demo,
    File(PathBuf),
}

impl InputSource {
    pub(super) fn read(self) -> Result<Vec<u8>> {
        match self {
            Self::Demo => Ok(DEMO.to_vec()),
            Self::File(path) => read_input(&path),
        }
    }
}

pub(super) enum Command {
    Audit(InputSource),
    AuditNative(crate::local::Target),
    Backup {
        source: InputSource,
        out: PathBuf,
    },
    PrepareCleanup {
        source: InputSource,
        out: PathBuf,
        selected: Vec<String>,
    },
    ApplyCleanup {
        directory: PathBuf,
        approved: String,
    },
    PrepareRename {
        source: InputSource,
        out: PathBuf,
        changes: Vec<RenameChange>,
    },
    PrepareNameProposal {
        snapshot: PathBuf,
        out: PathBuf,
        change: RenameChange,
    },
    VerifyNameProposal {
        directory: PathBuf,
        expected: String,
        after: Option<PathBuf>,
    },
    ApplyRename {
        directory: PathBuf,
        approved: String,
    },
    VerifyBackup {
        directory: PathBuf,
        expected: Option<String>,
    },
    VerifyOperation {
        directory: PathBuf,
        expected: Option<String>,
    },
}

pub(super) struct Invocation {
    pub(super) command: Command,
    pub(super) json: bool,
}

fn usage(message: impl Into<String>) -> Error {
    Error::new("usage", message, 2)
}

struct Arguments {
    flags: std::collections::BTreeSet<String>,
    values: std::collections::BTreeMap<String, Vec<OsString>>,
}

impl Arguments {
    fn parse(raw: &[OsString]) -> Result<Self> {
        let mut args = Self {
            flags: Default::default(),
            values: Default::default(),
        };
        let mut raw = raw.iter();
        while let Some(argument) = raw.next() {
            let name = argument
                .to_str()
                .ok_or_else(|| usage("Option names must be UTF-8."))?;
            match name {
                "--demo" | "--json" | "--local" => {
                    if !args.flags.insert(name.into()) {
                        return Err(usage(format!("Repeated option: {name}.")));
                    }
                }
                "--input"
                | "--snapshot"
                | "--name-proposal"
                | "--app-server"
                | "--peer"
                | "--out"
                | "--backup"
                | "--operation"
                | "--apply"
                | "--resume"
                | "--select"
                | "--set"
                | "--approve-plan-sha256"
                | "--expected-plan-sha256"
                | "--expected-manifest-sha256" => {
                    let value = raw.next().filter(|value| !value.to_string_lossy().starts_with("--"))
                        .ok_or_else(|| usage(format!("{name} needs a value. Prefix a literal flag-like path with ./ if needed.")))?;
                    let values = args.values.entry(name.into()).or_default();
                    if !["--select", "--set"].contains(&name) && !values.is_empty() {
                        return Err(usage(format!("Repeated option: {name}.")));
                    }
                    values.push(value.clone());
                }
                _ => {
                    return Err(usage(
                        "Unknown option. Use --help for the supported command options.",
                    ));
                }
            }
        }
        Ok(args)
    }

    fn flag(&mut self, name: &str) -> bool {
        self.flags.remove(name)
    }

    fn value(&mut self, name: &str) -> Option<OsString> {
        self.values.remove(name).and_then(|mut values| values.pop())
    }

    fn path(&mut self, name: &str) -> Result<PathBuf> {
        self.value(name)
            .map(PathBuf::from)
            .ok_or_else(|| usage(format!("{name} is required.")))
    }

    fn text(&mut self, name: &str) -> Result<Option<String>> {
        self.value(name)
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| usage(format!("{name} requires UTF-8 text.")))
            })
            .transpose()
    }

    fn source(&mut self) -> Result<InputSource> {
        match (self.flag("--demo"), self.value("--input")) {
            (true, None) => Ok(InputSource::Demo),
            (false, Some(path)) => Ok(InputSource::File(path.into())),
            _ => Err(usage("Choose exactly one source: --demo or --input FILE.")),
        }
    }

    fn selection(&mut self) -> Result<Vec<String>> {
        let ids: Vec<_> = self
            .values
            .remove("--select")
            .unwrap_or_default()
            .into_iter()
            .map(|value| {
                value
                    .into_string()
                    .map_err(|_| usage("Selectors must be full UTF-8 environment IDs."))
            })
            .collect::<Result<_>>()?;
        if ids.is_empty() {
            return Err(usage("Cleanup preview needs explicit --select full IDs."));
        }
        Ok(ids)
    }

    fn finish(self) -> Result<()> {
        if !self.flags.is_empty() || !self.values.is_empty() {
            return Err(usage(
                "An option does not belong to this command or operation mode.",
            ));
        }
        Ok(())
    }
}

fn rename(args: &mut Arguments) -> Result<Command> {
    match (args.value("--apply"), args.value("--resume")) {
        (Some(directory), None) | (None, Some(directory)) => Ok(Command::ApplyRename {
            directory: directory.into(),
            approved: args
                .text("--approve-plan-sha256")?
                .ok_or_else(|| usage("Apply/resume requires --approve-plan-sha256."))?,
        }),
        (None, None) => {
            let raw = args.values.remove("--set").unwrap_or_default();
            if raw.is_empty() {
                return Err(usage(
                    "Rename preview needs explicit --set FULL_ID=NAME mappings.",
                ));
            }
            let changes: Vec<RenameChange> = raw
                .into_iter()
                .map(|value| {
                    let value = value
                        .into_string()
                        .map_err(|_| usage("Rename mappings require UTF-8 text."))?;
                    let (id, name) = value
                        .split_once('=')
                        .filter(|(id, _)| !id.is_empty())
                        .ok_or_else(|| usage("Each --set must contain FULL_ID=NAME."))?;
                    Ok(RenameChange {
                        environment_id: id.into(),
                        new_name: name.into(),
                    })
                })
                .collect::<Result<_>>()?;
            if let Some(snapshot) = args.value("--snapshot") {
                if changes.len() != 1 {
                    return Err(usage(
                        "A name-only proposal requires exactly one --set FULL_ID=NAME.",
                    ));
                }
                Ok(Command::PrepareNameProposal {
                    snapshot: snapshot.into(),
                    out: args.path("--out")?,
                    change: changes.into_iter().next().expect("one mapping"),
                })
            } else {
                Ok(Command::PrepareRename {
                    source: args.source()?,
                    out: args.path("--out")?,
                    changes,
                })
            }
        }
        _ => Err(usage("Choose only one of --apply or --resume.")),
    }
}

fn cleanup(args: &mut Arguments) -> Result<Command> {
    match (args.value("--apply"), args.value("--resume")) {
        (Some(directory), None) | (None, Some(directory)) => Ok(Command::ApplyCleanup {
            directory: directory.into(),
            approved: args
                .text("--approve-plan-sha256")?
                .ok_or_else(|| usage("Apply/resume requires --approve-plan-sha256."))?,
        }),
        (None, None) => Ok(Command::PrepareCleanup {
            source: args.source()?,
            out: args.path("--out")?,
            selected: args.selection()?,
        }),
        _ => Err(usage("Choose only one of --apply or --resume.")),
    }
}

fn verification(args: &mut Arguments) -> Result<Command> {
    if let Some(directory) = args.value("--name-proposal") {
        return Ok(Command::VerifyNameProposal {
            directory: directory.into(),
            expected: args.text("--expected-plan-sha256")?.ok_or_else(|| {
                usage("Name proposal verification requires --expected-plan-sha256.")
            })?,
            after: args.value("--input").map(PathBuf::from),
        });
    }
    match (args.value("--backup"), args.value("--operation")) {
        (Some(directory), None) => Ok(Command::VerifyBackup {
            directory: directory.into(),
            expected: args.text("--expected-manifest-sha256")?,
        }),
        (None, Some(directory)) => Ok(Command::VerifyOperation {
            directory: directory.into(),
            expected: args.text("--expected-plan-sha256")?,
        }),
        _ => Err(usage(
            "verify requires exactly one --backup or --operation.",
        )),
    }
}

pub(super) fn parse(args: &[OsString]) -> Result<Invocation> {
    let name = args.first().and_then(|arg| arg.to_str()).ok_or_else(|| {
        usage("Choose audit, backup, rename, cleanup, or verify. Use --help for examples.")
    })?;
    let mut args = Arguments::parse(&args[1..])?;
    let json = args.flag("--json");
    let command = match name {
        "audit" => match (args.flag("--local"), args.text("--peer")?) {
            (true, None) => Command::AuditNative(crate::local::Target::Local {
                app_server: args.value("--app-server").map(PathBuf::from),
            }),
            (false, Some(alias)) => Command::AuditNative(crate::local::Target::Peer {
                alias,
                app_server: args.path("--app-server")?,
            }),
            (false, None) => Command::Audit(args.source()?),
            _ => return Err(usage("Choose --local or --peer, never both.")),
        },
        "backup" => Command::Backup {
            source: args.source()?,
            out: args.path("--out")?,
        },
        "cleanup" => cleanup(&mut args)?,
        "rename" => rename(&mut args)?,
        "verify" => verification(&mut args)?,
        _ => return Err(usage("Unknown command. Use --help for available commands.")),
    };
    args.finish()?;
    Ok(Invocation { command, json })
}
