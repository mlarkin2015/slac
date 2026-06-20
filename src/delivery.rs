use crate::config::DeliveryConfig;
use std::env;
use std::io::Write;
use std::process::{Command, Stdio};

/// Delegates final delivery to the configured local delivery command.
///
/// `raw_mail` is written unchanged to the child process stdin. Postcondition:
/// returns the child exit status, or `75` if the process was terminated without
/// a normal exit code. The caller decides how to interpret delivery failure.
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

    Ok(status.code().unwrap_or(75))
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
