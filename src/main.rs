use olhm::collector::{Collector, cpu::CpuCollector};
use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

const USAGE: &str = "Usage: olhm help\n       olhm gui [cpu|ram|storage|gpu] [--interval SECONDS]\n       olhm info cpu|ram|storage|gpu\n       olhm snapshot cpu|ram|storage|gpu [--interval SECONDS]\n       olhm monitor cpu|ram|storage|gpu [--interval SECONDS] [--count N]\n       olhm collect cpu [--format json|debug]\n\nhelp: display this help.\ngui: open the CPU/RAM/storage/GPU window (requires a build with --features gui).\ninfo: display static information for the selected category.\nsnapshot: wait for two readings and display one monitoring snapshot.\nmonitor: live CPU, RAM/swap, storage or GPU monitoring (Ctrl+C to stop).\n  --interval: 0.1 to 60 seconds, default 1.\n  --count: monitor frame count including the initial counter baseline; default unlimited.\nNo arguments: collect CPU and output JSON.";

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Gui {
        target: String,
        interval: Duration,
    },
    Info {
        target: String,
    },
    Snapshot {
        target: String,
        interval: Duration,
    },
    Monitor {
        target: String,
        interval: Duration,
        count: Option<usize>,
    },
    Collect {
        debug: bool,
    },
    Help,
}

fn parse_target(target: &str) -> Result<String, &'static str> {
    if olhm::monitor::TARGETS.contains(&target) {
        Ok(target.into())
    } else {
        Err("unsupported hardware category; use olhm help")
    }
}

fn parse_interval(value: &str) -> Result<Duration, &'static str> {
    let seconds = value
        .parse::<f64>()
        .map_err(|_| "invalid interval; expected 0.1 to 60 seconds")?;
    if !seconds.is_finite() || !(0.1..=60.0).contains(&seconds) {
        return Err("invalid interval; expected 0.1 to 60 seconds");
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn parse_args(args: &[String]) -> Result<Command, &'static str> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["help"] | ["--help"] | ["-h"] => Ok(Command::Help),
        ["gui"] => Ok(Command::Gui {
            target: "cpu".into(),
            interval: Duration::from_secs(1),
        }),
        ["gui", "--interval", value] => Ok(Command::Gui {
            target: "cpu".into(),
            interval: parse_interval(value)?,
        }),
        ["gui", target] => Ok(Command::Gui {
            target: parse_target(target)?,
            interval: Duration::from_secs(1),
        }),
        ["gui", target, "--interval", value] => Ok(Command::Gui {
            target: parse_target(target)?,
            interval: parse_interval(value)?,
        }),
        ["info", target] => Ok(Command::Info {
            target: parse_target(target)?,
        }),
        ["snapshot", target] => Ok(Command::Snapshot {
            target: parse_target(target)?,
            interval: Duration::from_secs(1),
        }),
        ["snapshot", target, "--interval", value] => Ok(Command::Snapshot {
            target: parse_target(target)?,
            interval: parse_interval(value)?,
        }),
        ["monitor", target, options @ ..] => {
            let target = parse_target(target)?;
            let mut interval = None;
            let mut count = None;
            let (pairs, remainder) = options.as_chunks::<2>();
            for pair in pairs {
                match pair {
                    ["--interval", value] if interval.is_none() => {
                        interval = Some(parse_interval(value)?);
                    }
                    ["--count", value] if count.is_none() => {
                        let value = value
                            .parse::<usize>()
                            .map_err(|_| "invalid count; expected a positive integer")?;
                        if value == 0 {
                            return Err("count must be greater than zero");
                        }
                        count = Some(value);
                    }
                    _ => return Err("unknown or duplicate monitor option; use olhm --help"),
                }
            }
            if !remainder.is_empty() {
                return Err("missing monitor option value; use olhm --help");
            }
            Ok(Command::Monitor {
                target,
                interval: interval.unwrap_or(Duration::from_secs(1)),
                count,
            })
        }
        [] | ["collect", "cpu"] | ["collect", "cpu", "--format", "json"] => {
            Ok(Command::Collect { debug: false })
        }
        ["collect", "cpu", "--format", "debug"] => Ok(Command::Collect { debug: true }),
        _ => Err("invalid arguments; use olhm --help"),
    }
}

#[derive(serde::Serialize)]
struct CpuDocument<'a> {
    schema_version: u32,
    snapshot: &'a olhm::raw::cpu::RawCpuSnapshot,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match run() {
        Err(error)
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) =>
        {
            Ok(())
        }
        result => result,
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let command = parse_args(&std::env::args().skip(1).collect::<Vec<_>>())
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidInput, message))?;
    let refresh =
        io::stdout().is_terminal() && std::env::var("TERM").is_ok_and(|term| term != "dumb");
    let mut output = io::BufWriter::new(io::stdout().lock());
    match command {
        Command::Gui { target, interval } => {
            #[cfg(feature = "gui")]
            olhm::presentation::gui::run(target, interval)?;
            #[cfg(not(feature = "gui"))]
            {
                let _ = (target, interval);
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "GUI is not included in this build; use cargo run --features gui -- gui cpu",
                )
                .into());
            }
        }
        Command::Help => writeln!(output, "{USAGE}")?,
        Command::Info { target } => {
            let session = olhm::monitor::open(&target)?;
            for device in session.info_devices() {
                olhm::presentation::text::write_info(&mut output, &device)?;
            }
        }
        Command::Snapshot { target, interval } => {
            let mut session = olhm::monitor::open(&target)?;
            let started = Instant::now();
            session.sample();
            std::thread::sleep(interval.saturating_sub(started.elapsed()));
            olhm::presentation::text::write_snapshot(&mut output, &session.sample())?;
        }
        Command::Monitor {
            target,
            interval,
            count,
        } => {
            let mut session = olhm::monitor::open(&target)?;
            let mut frames = 0_usize;
            loop {
                let started = Instant::now();
                olhm::presentation::text::write_monitor(&mut output, &session.sample(), refresh)?;
                output.flush()?;
                if let Some(limit) = count {
                    frames += 1;
                    if frames >= limit {
                        break;
                    }
                }
                std::thread::sleep(interval.saturating_sub(started.elapsed()));
            }
        }
        Command::Collect { debug } => {
            let snapshot = CpuCollector::default().collect()?;
            if debug {
                writeln!(output, "{snapshot:#?}")?;
            } else {
                serde_json::to_writer_pretty(
                    &mut output,
                    &CpuDocument {
                        schema_version: 1,
                        snapshot: &snapshot,
                    },
                )?;
                writeln!(output)?;
            }
        }
    }
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Command, &'static str> {
        parse_args(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn gui_options_preserve_interval_validation() {
        for args in [vec!["gui"], vec!["gui", "cpu"]] {
            assert_eq!(
                parse(&args),
                Ok(Command::Gui {
                    target: "cpu".into(),
                    interval: Duration::from_secs(1)
                })
            );
        }
        for args in [
            vec!["gui", "--interval", "0.5"],
            vec!["gui", "cpu", "--interval", "0.5"],
        ] {
            assert_eq!(
                parse(&args),
                Ok(Command::Gui {
                    target: "cpu".into(),
                    interval: Duration::from_millis(500)
                })
            );
        }
        for args in [
            vec!["gui", "unknown"],
            vec!["gui", "cpu", "--interval", "0"],
            vec!["gui", "--interval", "NaN"],
            vec!["gui", "cpu", "--interval"],
            vec!["gui", "cpu", "--count", "2"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn supported_commands_and_invalid_arguments() {
        assert_eq!(parse(&[]), Ok(Command::Collect { debug: false }));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["help"]), Ok(Command::Help));
        assert_eq!(
            parse(&["info", "cpu"]),
            Ok(Command::Info {
                target: "cpu".into()
            })
        );
        assert_eq!(
            parse(&["collect", "cpu"]),
            Ok(Command::Collect { debug: false })
        );
        assert_eq!(
            parse(&["collect", "cpu", "--format", "json"]),
            Ok(Command::Collect { debug: false })
        );
        assert_eq!(
            parse(&["collect", "cpu", "--format", "debug"]),
            Ok(Command::Collect { debug: true })
        );
        for args in [
            vec!["collect", "gpu"],
            vec!["info", "unknown"],
            vec!["info", "cpu", "extra"],
            vec!["help", "extra"],
            vec!["collect", "cpu", "--format"],
            vec!["collect", "cpu", "--format", "yaml"],
            vec!["--help", "extra"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn ram_commands_share_target_and_interval_validation() {
        assert_eq!(
            parse(&["info", "ram"]),
            Ok(Command::Info {
                target: "ram".into()
            })
        );
        assert_eq!(
            parse(&["gui", "ram"]),
            Ok(Command::Gui {
                target: "ram".into(),
                interval: Duration::from_secs(1)
            })
        );
        assert_eq!(
            parse(&["gui", "ram", "--interval", "0.5"]),
            Ok(Command::Gui {
                target: "ram".into(),
                interval: Duration::from_millis(500)
            })
        );
        assert_eq!(
            parse(&["snapshot", "ram", "--interval", "0.25"]),
            Ok(Command::Snapshot {
                target: "ram".into(),
                interval: Duration::from_millis(250)
            })
        );
        assert_eq!(
            parse(&["monitor", "ram", "--count", "2"]),
            Ok(Command::Monitor {
                target: "ram".into(),
                interval: Duration::from_secs(1),
                count: Some(2)
            })
        );
        for args in [
            vec!["snapshot", "ram", "--interval", "NaN"],
            vec!["gui", "ram", "--interval", "0"],
            vec!["monitor", "ram", "--count", "0"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn gpu_commands_use_the_shared_parser_and_validate_options() {
        assert_eq!(
            parse(&["info", "gpu"]),
            Ok(Command::Info {
                target: "gpu".into()
            })
        );
        assert_eq!(
            parse(&["snapshot", "gpu", "--interval", "0.2"]),
            Ok(Command::Snapshot {
                target: "gpu".into(),
                interval: Duration::from_millis(200)
            })
        );
        assert_eq!(
            parse(&["gui", "gpu"]),
            Ok(Command::Gui {
                target: "gpu".into(),
                interval: Duration::from_secs(1)
            })
        );
        assert_eq!(
            parse(&["monitor", "gpu", "--count", "2"]),
            Ok(Command::Monitor {
                target: "gpu".into(),
                interval: Duration::from_secs(1),
                count: Some(2)
            })
        );
        for args in [
            vec!["snapshot", "gpu", "--interval", "0"],
            vec!["monitor", "gpu", "--count", "0"],
            vec!["collect", "gpu"],
        ] {
            assert!(parse(&args).is_err());
        }
    }

    #[test]
    fn storage_targets_work_in_cli_and_gui_without_changing_raw_cpu_export() {
        assert_eq!(
            parse(&["info", "storage"]),
            Ok(Command::Info {
                target: "storage".into()
            })
        );
        assert_eq!(
            parse(&["gui", "storage", "--interval", "0.5"]),
            Ok(Command::Gui {
                target: "storage".into(),
                interval: Duration::from_millis(500)
            })
        );
        assert_eq!(
            parse(&["snapshot", "storage"]),
            Ok(Command::Snapshot {
                target: "storage".into(),
                interval: Duration::from_secs(1)
            })
        );
        assert_eq!(
            parse(&["monitor", "storage", "--count", "2"]),
            Ok(Command::Monitor {
                target: "storage".into(),
                interval: Duration::from_secs(1),
                count: Some(2)
            })
        );
        for args in [
            vec!["monitor", "storage", "--count", "0"],
            vec!["gui", "storage", "--interval", "NaN"],
            vec!["collect", "storage"],
        ] {
            assert!(parse(&args).is_err());
        }
        assert_eq!(parse(&[]), Ok(Command::Collect { debug: false }));
    }

    #[test]
    fn snapshot_options_are_validated_before_collection() {
        assert_eq!(
            parse(&["snapshot", "cpu"]),
            Ok(Command::Snapshot {
                target: "cpu".into(),
                interval: Duration::from_secs(1),
            })
        );
        assert_eq!(
            parse(&["snapshot", "cpu", "--interval", "0.25"]),
            Ok(Command::Snapshot {
                target: "cpu".into(),
                interval: Duration::from_millis(250),
            })
        );
        for args in [
            vec!["snapshot", "unknown"],
            vec!["snapshot", "cpu", "--interval"],
            vec!["snapshot", "cpu", "--interval", "0"],
            vec!["snapshot", "cpu", "--interval", "NaN"],
            vec!["snapshot", "cpu", "--count", "2"],
            vec!["snapshot", "cpu", "--interval", "1", "extra"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn monitor_options_are_validated_before_collection() {
        assert_eq!(
            parse(&["monitor", "cpu"]),
            Ok(Command::Monitor {
                target: "cpu".into(),
                interval: Duration::from_secs(1),
                count: None,
            })
        );
        assert_eq!(
            parse(&["monitor", "cpu", "--count", "3", "--interval", "0.25"]),
            Ok(Command::Monitor {
                target: "cpu".into(),
                interval: Duration::from_millis(250),
                count: Some(3),
            })
        );
        for options in [
            vec!["--interval"],
            vec!["--interval", "0"],
            vec!["--interval", "-1"],
            vec!["--interval", "NaN"],
            vec!["--interval", "inf"],
            vec!["--interval", "61"],
            vec!["--interval", "0.01"],
            vec!["--interval", "abc"],
            vec!["--count", "0"],
            vec!["--count", "-1"],
            vec!["--count", "1.5"],
            vec!["--count", "1", "--count", "2"],
            vec!["--interval", "1", "--interval", "2"],
            vec!["--format", "json"],
        ] {
            let mut args = vec!["monitor", "cpu"];
            args.extend(options);
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
