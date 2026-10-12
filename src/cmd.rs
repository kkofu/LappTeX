use std::process::Command;

pub(crate) fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(cmd)
        .args(args)
        .status()
        .map_err(|e| format!("Failed to run `{cmd}`: {e} (is it installed and on PATH?)"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{cmd} {}` failed with status {status}",
            args.join(" ")
        ))
    }
}

pub(crate) fn warn(msg: &str) {
    eprintln!("warning: {msg}");
}
