/*
BSD 2-Clause License

Copyright (c) 2026, Mike Larkin <mlarkin@nested.page>

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND
ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED
WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE FOR
ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES
INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES;
LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION HOWEVER CAUSED AND ON
ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
INCLUDING NEGLIGENCE OR OTHERWISE ARISING IN ANY WAY OUT OF THE USE OF THIS
SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
*/

mod classifier;
mod config;
mod corrections;
mod delivery;
mod log;
mod mail_headers;
mod mbox;
mod mda;
mod prompt;
mod review;
mod selftest;
mod sysexits;
mod tui;

use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Mda,
    Web,
    Tui,
    Test,
    List,
    Show,
    Move,
}

#[derive(Debug, Clone)]
pub struct Cli {
    mode: Mode,
    config_path: Option<PathBuf>,
    debug: bool,
    mailbox: Option<String>,
    id: Option<usize>,
    to_mailbox: Option<String>,
    reason: Option<String>,
}

fn main() {
    let cli = match parse_cli(env::args().skip(1)) {
        Ok(cli) => cli,
        Err(err) => {
            eprintln!("{err}");
            print_usage();
            std::process::exit(64);
        }
    };

    let logger = log::Logger::new(cli.debug);

    let result = match cli.mode {
        Mode::Mda => mda::run(cli.config_path.as_deref(), &logger),
        Mode::Web => {
            logger.info("web mode is not implemented yet");
            Err("web mode is not implemented yet".into())
        }
        Mode::Tui => tui::run(cli.config_path.as_deref()),
        Mode::Test => {
            let result = selftest::run(cli.config_path.as_deref());
            if let Err(err) = &result {
                eprintln!("slac test: failed: {err}");
            }
            result
        }
        Mode::List => review::list(cli.config_path.as_deref(), cli.mailbox.as_deref()),
        Mode::Show => review::show(cli.config_path.as_deref(), cli.mailbox.as_deref(), cli.id),
        Mode::Move => review::move_message(
            cli.config_path.as_deref(),
            cli.mailbox.as_deref(),
            cli.id,
            cli.to_mailbox.as_deref(),
            cli.reason.as_deref(),
        ),
    };

    if let Err(err) = result {
        if cli.mode != Mode::Test {
            logger.err(&format!("slac failed: {err}"));
        }
        std::process::exit(sysexits::EX_TEMPFAIL);
    }
}

fn parse_cli<I>(args: I) -> Result<Cli, String>
where
    I: IntoIterator<Item = String>,
{
    let mut mode = None;
    let mut config_path = None;
    let mut debug = false;
    let mut mailbox = None;
    let mut id = None;
    let mut to_mailbox = None;
    let mut reason = None;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "mda" => mode = Some(Mode::Mda),
            "web" => mode = Some(Mode::Web),
            "tui" => mode = Some(Mode::Tui),
            "test" => mode = Some(Mode::Test),
            "list" => mode = Some(Mode::List),
            "show" => mode = Some(Mode::Show),
            "move" => mode = Some(Mode::Move),
            "-d" | "--debug" => debug = true,
            "-c" | "--config" => {
                let Some(path) = iter.next() else {
                    return Err(format!("{arg} requires a path"));
                };
                config_path = Some(PathBuf::from(path));
            }
            "--mailbox" => {
                let Some(value) = iter.next() else {
                    return Err("--mailbox requires inbox or spam".to_string());
                };
                mailbox = Some(value);
            }
            "--id" => {
                let Some(value) = iter.next() else {
                    return Err("--id requires a numeric message id".to_string());
                };
                id = Some(
                    value
                        .parse()
                        .map_err(|_| format!("invalid numeric id: {value}"))?,
                );
            }
            "--to" => {
                let Some(value) = iter.next() else {
                    return Err("--to requires inbox or spam".to_string());
                };
                to_mailbox = Some(value);
            }
            "--reason" => {
                let Some(value) = iter.next() else {
                    return Err("--reason requires text".to_string());
                };
                reason = Some(value);
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            unknown => return Err(format!("unknown argument: {unknown}")),
        }
    }

    Ok(Cli {
        mode: mode.unwrap_or(Mode::Mda),
        config_path,
        debug,
        mailbox,
        id,
        to_mailbox,
        reason,
    })
}

fn print_usage() {
    eprintln!(
        "usage: slac [-d] [-c path] [mda|web|tui|test]\n\
         test: slac [-c path] test\n\
         review: slac [-c path] list --mailbox inbox|spam\n\
         review: slac [-c path] show --mailbox inbox|spam --id N\n\
         review: slac [-c path] move --mailbox inbox|spam --id N --to inbox|spam [--reason text]\n\
         default mode is mda\n\
         -d, --debug       also log to stderr\n\
         -c, --config      TOML config path"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_mda() {
        let cli = parse_cli(Vec::<String>::new()).unwrap();
        assert_eq!(cli.mode, Mode::Mda);
        assert!(!cli.debug);
    }

    #[test]
    fn parses_debug_config_and_mode() {
        let cli = parse_cli(["-d", "-c", "slac.toml", "web"].map(String::from)).unwrap();
        assert_eq!(cli.mode, Mode::Web);
        assert!(cli.debug);
        assert_eq!(cli.config_path.unwrap(), PathBuf::from("slac.toml"));
    }

    #[test]
    fn parses_review_move() {
        let cli = parse_cli(
            [
                "move",
                "--mailbox",
                "spam",
                "--id",
                "3",
                "--to",
                "inbox",
                "--reason",
                "false positive",
            ]
            .map(String::from),
        )
        .unwrap();
        assert_eq!(cli.mode, Mode::Move);
        assert_eq!(cli.mailbox.as_deref(), Some("spam"));
        assert_eq!(cli.id, Some(3));
        assert_eq!(cli.to_mailbox.as_deref(), Some("inbox"));
        assert_eq!(cli.reason.as_deref(), Some("false positive"));
    }

    #[test]
    fn parses_test_mode() {
        let cli = parse_cli(["-c", "/etc/slac.toml", "test"].map(String::from)).unwrap();
        assert_eq!(cli.mode, Mode::Test);
        assert_eq!(cli.config_path.unwrap(), PathBuf::from("/etc/slac.toml"));
    }
}
