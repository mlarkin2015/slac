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

use crate::config::DeliveryConfig;
use crate::sysexits::EX_TEMPFAIL;
use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

/// Delegates final delivery to the configured local delivery command.
///
/// `raw_mail` is written unchanged to the child process stdin. Postcondition:
/// returns the child exit status, or `EX_TEMPFAIL` if the process was
/// terminated without a normal exit code. The caller decides how to interpret
/// delivery failure.
pub fn deliver(raw_mail: &[u8], config: &DeliveryConfig) -> Result<i32, String> {
    let args = expand_args(&config.args);
    let mut child = Command::new(&config.command)
        .args(&args)
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to spawn delivery command {}: {err}", config.command))?;

    {
        let stdin = child
            .stdin
            .as_mut()
            .ok_or("failed to open delivery command stdin")?;
        stdin
            .write_all(raw_mail)
            .map_err(|err| format!("failed to write message to delivery command: {err}"))?;
    }

    let status = child
        .wait()
        .map_err(|err| format!("failed waiting for delivery command: {err}"))?;

    Ok(status.code().unwrap_or(EX_TEMPFAIL))
}

pub fn expand_args(args: &[String]) -> Vec<String> {
    args.iter().map(|arg| expand_template(arg)).collect()
}

/// Expands SLAC's small set of delivery/path placeholders from the MDA
/// environment. Unknown placeholders are intentionally left unchanged.
pub fn expand_template(template: &str) -> String {
    let sender = env::var("SENDER").unwrap_or_default();
    let user = env::var("USER")
        .or_else(|_| env::var("LOGNAME"))
        .unwrap_or_default();
    let recipient = env::var("RECIPIENT").unwrap_or_default();
    let home = env::var("HOME").unwrap_or_default();

    template
        .replace("{sender}", &sender)
        .replace("{user}", &user)
        .replace("{recipient}", &recipient)
        .replace("{home}", &home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_unknown_templates_alone() {
        let args = expand_args(&["{missing}".to_string()]);
        assert_eq!(args, vec!["{missing}".to_string()]);
    }
}
