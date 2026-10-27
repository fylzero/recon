use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::sync::Mutex;

use super::ssh_config;
use crate::console::hide_window;
use crate::models::{ConnectionEntry, PasswordSource};
use crate::prompts::Prompter;

const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const SSO_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
/// What the AWS CLI says when an SSO profile needs `aws sso login`, lowercased.
const SSO_EXPIRED_MARKERS: [&str; 3] = [
    "error when retrieving token from sso",
    "error loading sso token",
    "sso session associated with this profile",
];
const SSO_NEEDS_RECONNECT: &str =
    "Your AWS SSO sign-in has expired. Click Reconnect to sign in again.";
const SSO_INSTRUCTIONS: &str = "Your AWS SSO sign-in has expired. Finish signing in to AWS in your browser, and Recon will continue on its own.";

/// One `aws sso login` at a time, so connections sharing a profile open one browser tab.
static SSO_LOGIN: Mutex<()> = Mutex::const_new(());
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
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines[lines.len().saturating_sub(ERROR_TAIL_LINES)..].join(" ")
}

pub(super) fn aws_command(args: impl IntoIterator<Item = String>) -> Command {
    let mut command = Command::new("aws");
    command.args(args);
    command
}

/**
 * Runs a helper tool with the login shell's PATH and returns what it printed.
 * Failures name the tool through `label` and include the end of its stderr.
 */
pub(super) async fn run_tool(command: &mut Command, label: &str) -> Result<Vec<u8>, String> {
    run_tool_within(command, label, FETCH_TIMEOUT).await
}

async fn run_tool_within(
    command: &mut Command,
    label: &str,
    timeout: Duration,
) -> Result<Vec<u8>, String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let run = hide_window(command)
        .env("PATH", ssh_config::login_path().await)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = match tokio::time::timeout(timeout, run).await {
        Err(_) => {
            return Err(format!(
                "{label} didn't finish within {} seconds.",
                timeout.as_secs()
            ))
        }
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound && program == "aws" => {
            return Err("The AWS CLI (aws) wasn't found on your shell's PATH. Install it to use AWS features.".into());
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
    Ok(output.stdout)
}

fn parse_profiles(stdout: &[u8]) -> Vec<String> {
    let mut profiles: Vec<String> = String::from_utf8_lossy(stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect();
    profiles.sort();
    profiles.dedup();
    profiles
}

/// Profile names from the AWS CLI's config and credentials files, or none if the CLI isn't usable.
pub async fn list_aws_profiles() -> Vec<String> {
    let mut command = aws_command(["configure".to_string(), "list-profiles".into()]);
    match run_tool(&mut command, "The AWS CLI").await {
        Ok(stdout) => parse_profiles(&stdout),
        Err(_) => Vec::new(),
    }
}

/**
 * Gets the password for an entry whose password isn't typed or saved. Pass
 * the saved entry, not an SSH tunnel's local one: RDS signs the token for
 * the real host and port.
 */
pub async fn fetch(entry: &ConnectionEntry) -> Result<String, String> {
    let (mut command, label) = match entry.password_source {
        PasswordSource::Password => {
            return Err("This connection doesn't fetch its password.".into())
        }
        PasswordSource::AwsIam => (aws_command(aws_args(entry)), "The AWS CLI"),
        PasswordSource::Command => {
            let mut command = ssh_config::shell_command(&entry.password_command);
            command
                .env("RECON_DB_HOST", &entry.host)
                .env("RECON_DB_PORT", entry.port.to_string())
                .env("RECON_DB_USER", &entry.user);
            (command, "The password command")
        }
    };
    let stdout = run_tool(&mut command, label).await?;
    let password = String::from_utf8_lossy(&stdout).trim().to_string();
    if password.is_empty() {
        return Err(format!("{label} printed nothing to use as the password."));
    }
    Ok(password)
}

fn needs_sso_login(entry: &ConnectionEntry, err: &str) -> bool {
    let err = err.to_lowercase();
    entry.password_source == PasswordSource::AwsIam
        && SSO_EXPIRED_MARKERS
            .iter()
            .any(|marker| err.contains(marker))
}

async fn sso_login(profile: &str) -> Result<(), String> {
    let mut args = vec!["sso".to_string(), "login".into()];
    if !profile.is_empty() {
        args.push("--profile".into());
        args.push(profile.to_string());
    }
    let mut command = aws_command(args);
    run_tool_within(&mut command, "aws sso login", SSO_LOGIN_TIMEOUT)
        .await
        .map(drop)
}

/**
 * Like `fetch`, but when an AWS SSO sign-in has expired, runs `aws sso login`
 * and tries again. The browser only opens for a prompter that can ask, so
 * automatic reconnects never pop it up on their own.
 */
pub async fn fetch_signing_in(
    entry: &ConnectionEntry,
    prompter: &Prompter,
) -> Result<String, String> {
    match fetch(entry).await {
        Err(err) if needs_sso_login(entry, &err) => {}
        outcome => return outcome,
    }
    if !prompter.can_ask() {
        return Err(SSO_NEEDS_RECONNECT.into());
    }
    let _turn = SSO_LOGIN.lock().await;
    // Another connection may have signed in while this one waited its turn.
    match fetch(entry).await {
        Err(err) if needs_sso_login(entry, &err) => {}
        outcome => return outcome,
    }
    prompter
        .wait_for(SSO_INSTRUCTIONS, sso_login(&entry.aws_profile))
        .await?;
    fetch(entry).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::Driver;

    const HOST: &str = "db.example.com";

    fn entry(source: PasswordSource) -> ConnectionEntry {
        let mut entry: ConnectionEntry = serde_json::from_value(serde_json::json!({
            "name": "rds", "driver": "postgres", "host": HOST,
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
            format!("rds generate-db-auth-token --hostname {HOST} --port 5432 --username app --region us-east-1")
        );
        iam.aws_profile = "prod-sso".into();
        assert!(aws_args(&iam)
            .join(" ")
            .ends_with("--region us-east-1 --profile prod-sso"));
    }

    #[test]
    fn parses_profile_list() {
        assert_eq!(
            parse_profiles(b"prod-sso\n\n  default \nprod-sso\nci\n"),
            vec!["ci", "default", "prod-sso"]
        );
        assert!(parse_profiles(b"").is_empty());
    }

    #[test]
    fn spots_expired_sso_sign_ins() {
        let iam = entry(PasswordSource::AwsIam);
        for err in [
            "The AWS CLI failed: Error when retrieving token from sso: Token has expired and refresh failed",
            "The AWS CLI failed: Error loading SSO Token: Token for prod does not exist",
            "The AWS CLI failed: The SSO session associated with this profile has expired or is otherwise invalid.",
        ] {
            assert!(needs_sso_login(&iam, err), "{err}");
        }
        assert!(!needs_sso_login(
            &iam,
            "The AWS CLI failed: Unable to locate credentials."
        ));
        let cmd = entry(PasswordSource::Command);
        assert!(!needs_sso_login(
            &cmd,
            "Error loading SSO Token: Token for prod does not exist"
        ));
    }

    #[cfg(unix)]
    const PRINTS_TOKEN: &str =
        "printf '  token-for-%s@%s:%s\\n' \"$RECON_DB_USER\" \"$RECON_DB_HOST\" \"$RECON_DB_PORT\"";
    #[cfg(windows)]
    const PRINTS_TOKEN: &str = "echo   token-for-%RECON_DB_USER%@%RECON_DB_HOST%:%RECON_DB_PORT%";
    #[cfg(unix)]
    const FAILS: &str = "echo 'run aws sso login' >&2; exit 3";
    #[cfg(windows)]
    const FAILS: &str = "echo run aws sso login 1>&2 & exit /b 3";
    #[cfg(unix)]
    const PRINTS_NOTHING: &str = "printf '  \\n'";
    #[cfg(windows)]
    const PRINTS_NOTHING: &str = "rem";

    #[tokio::test]
    async fn uses_what_the_command_prints() {
        let mut cmd = entry(PasswordSource::Command);
        cmd.password_command = PRINTS_TOKEN.into();
        assert_eq!(
            fetch(&cmd).await.unwrap(),
            format!("token-for-app@{HOST}:5432")
        );
    }

    #[tokio::test]
    async fn reports_failures_and_empty_output() {
        let mut cmd = entry(PasswordSource::Command);
        cmd.password_command = FAILS.into();
        assert_eq!(
            fetch(&cmd).await.unwrap_err(),
            "The password command failed: run aws sso login"
        );
        cmd.password_command = PRINTS_NOTHING.into();
        assert!(fetch(&cmd).await.unwrap_err().contains("printed nothing"));
        assert!(fetch(&entry(PasswordSource::Password)).await.is_err());
    }
}
