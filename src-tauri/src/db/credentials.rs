use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use super::ssh_config;
use crate::models::{ConnectionEntry, PasswordSource};

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const ERROR_TAIL_LINES: usize = 3;
/// RDS tokens last 15 minutes, so pools get a new one well before that.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10 * 60);

fn aws_args(entry: &ConnectionEntry) -> Vec<String> {
    let mut args = vec![
        "rds".to_string(),
        "generate-db-auth-token".into(),
        "--hostname".into(),
        entry.host.clone(),
        "--port".into(),
        entry.port.to_string(),
        "--username".into(),
        entry.user.clone(),
        "--region".into(),
        entry.aws_region.clone(),
    ];
    if !entry.aws_profile.is_empty() {
        args.push("--profile".into());
        args.push(entry.aws_profile.clone());
    }
    args
}

fn last_lines(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines[lines.len().saturating_sub(ERROR_TAIL_LINES)..].join(" ")
}

/**
 * Gets the password for an entry whose password isn't typed or saved. Pass
 * the saved entry, not an SSH tunnel's local one: RDS signs the token for
 * the real host and port.
 */
pub async fn fetch(entry: &ConnectionEntry) -> Result<String, String> {
    let (mut command, label) = match entry.password_source {
        PasswordSource::Password => return Err("This connection doesn't fetch its password.".into()),
        PasswordSource::AwsIam => {
            let mut command = Command::new("aws");
            command.args(aws_args(entry));
            (command, "The AWS CLI")
        }
        PasswordSource::Command => {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let mut command = Command::new(shell);
            command
                .args(["-c", &entry.password_command])
                .env("RECON_DB_HOST", &entry.host)
                .env("RECON_DB_PORT", entry.port.to_string())
                .env("RECON_DB_USER", &entry.user);
            (command, "The password command")
        }
    };
    let run = command
        .env("PATH", ssh_config::login_path().await)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(FETCH_TIMEOUT, run).await {
        Err(_) => return Err(format!("{label} didn't finish within {} seconds.", FETCH_TIMEOUT.as_secs())),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(match entry.password_source {
                PasswordSource::AwsIam => "The AWS CLI (aws) wasn't found on your shell's PATH. Install it to use IAM logins.".into(),
                _ => format!("{label} couldn't start: {err}"),
            });
        }
        Ok(Err(err)) => return Err(format!("{label} couldn't start: {err}")),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        let tail = last_lines(&output.stderr);
        return Err(if tail.is_empty() {
            format!("{label} failed ({}).", output.status)
        } else {
            format!("{label} failed: {tail}")
        });
    }
    let password = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if password.is_empty() {
        return Err(format!("{label} printed nothing to use as the password."));
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Driver;

    fn entry(source: PasswordSource) -> ConnectionEntry {
        let mut entry: ConnectionEntry = serde_json::from_value(serde_json::json!({
            "name": "rds", "driver": "postgres", "host": "db.abc.us-east-1.rds.amazonaws.com",
            "port": 5432, "user": "app", "awsRegion": "us-east-1",
        }))
        .unwrap();
        entry.password_source = source;
        assert_eq!(entry.driver, Driver::Postgres);
        entry
    }

    #[test]
    fn builds_aws_token_arguments() {
        let mut iam = entry(PasswordSource::AwsIam);
        assert_eq!(
            aws_args(&iam).join(" "),
            "rds generate-db-auth-token --hostname db.abc.us-east-1.rds.amazonaws.com --port 5432 --username app --region us-east-1"
        );
        iam.aws_profile = "prod-sso".into();
        assert!(aws_args(&iam).join(" ").ends_with("--region us-east-1 --profile prod-sso"));
    }

    #[tokio::test]
    async fn uses_what_the_command_prints() {
        let mut cmd = entry(PasswordSource::Command);
        cmd.password_command = "printf '  token-for-%s@%s:%s\\n' \"$RECON_DB_USER\" \"$RECON_DB_HOST\" \"$RECON_DB_PORT\"".into();
        assert_eq!(fetch(&cmd).await.unwrap(), "token-for-app@db.abc.us-east-1.rds.amazonaws.com:5432");
    }

    #[tokio::test]
    async fn reports_failures_and_empty_output() {
        let mut cmd = entry(PasswordSource::Command);
        cmd.password_command = "echo 'run aws sso login' >&2; exit 3".into();
        assert_eq!(fetch(&cmd).await.unwrap_err(), "The password command failed: run aws sso login");
        cmd.password_command = "printf '  \\n'".into();
        assert!(fetch(&cmd).await.unwrap_err().contains("printed nothing"));
        assert!(fetch(&entry(PasswordSource::Password)).await.is_err());
    }
}
