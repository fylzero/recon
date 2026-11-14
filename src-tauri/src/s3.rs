use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::console::hide_std_window;
use crate::db::credentials::{aws_command, last_lines, run_tool, run_tool_within, sso_expired};
use crate::db::ssh_config;
use crate::models::S3Location;

const LABEL: &str = "The AWS CLI";
pub const SSO_EXPIRED: &str = "Your AWS SSO sign-in has expired.";
const NOT_FOUND: &str =
    "The AWS CLI (aws) wasn't found on your shell's PATH. Install it to use Amazon S3.";
/// S3 lowercases user metadata keys, so this is how `head-object` returns it too.
const SKIPPED_KEY: &str = "recon-skipped";
/// S3 allows 2 KB of user metadata in all, keys included.
const SKIPPED_BUDGET: usize = 1800;
const LIST_LIMIT: u32 = 1000;
/// Replacing metadata copies the whole object, which takes a while for a large backup.
const COPY_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct S3File {
    pub key: String,
    pub size: u64,
    pub modified: String,
}

/// Who the AWS CLI signs in as with a profile.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsIdentity {
    pub account: String,
    pub arn: String,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct S3Listing {
    pub prefix: String,
    pub folders: Vec<String>,
    pub files: Vec<S3File>,
    /// There were more than `LIST_LIMIT` entries, and only the first ones are listed.
    pub truncated: bool,
}

/**
 * Trims a location and fills in what can be inferred. A bucket pasted as
 * `s3://bucket/folder` is split into the bucket and its folder.
 */
pub fn sanitize(mut location: S3Location) -> Result<S3Location, String> {
    location.name = location.name.trim().to_string();
    location.profile = location.profile.trim().to_string();
    location.region = location.region.trim().to_string();
    let pasted = location.bucket.trim();
    let pasted = pasted.strip_prefix("s3://").unwrap_or(pasted);
    let (bucket, folder) = pasted.split_once('/').unwrap_or((pasted, ""));
    if bucket.is_empty() {
        return Err("Enter a bucket name.".into());
    }
    let prefix = if location.prefix.trim().is_empty() {
        folder_prefix(folder)
    } else {
        folder_prefix(&location.prefix)
    };
    location.bucket = bucket.to_string();
    location.prefix = prefix;
    location.endpoint = location.endpoint.trim().trim_end_matches('/').to_string();
    if !location.endpoint.is_empty() && !location.endpoint.contains("://") {
        location.endpoint = format!("https://{}", location.endpoint);
    }
    location.encryption = match location.encryption.trim() {
        "AES256" => "AES256".into(),
        "aws:kms" => "aws:kms".into(),
        _ => String::new(),
    };
    location.kms_key_id = if location.encryption == "aws:kms" {
        location.kms_key_id.trim().to_string()
    } else {
        String::new()
    };
    if location.name.is_empty() {
        location.name = format!("{}/{}", location.bucket, location.prefix)
            .trim_end_matches('/')
            .to_string();
    }
    Ok(location)
}

/// A folder inside a bucket: no leading slash, and a trailing one unless it's the bucket itself.
pub fn folder_prefix(prefix: &str) -> String {
    let prefix = prefix.trim().trim_start_matches('/');
    if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

pub fn url(bucket: &str, key: &str) -> String {
    format!("s3://{bucket}/{key}")
}

/// The file name at the end of `key`.
pub fn key_name(key: &str) -> String {
    key.rsplit('/').next().unwrap_or(key).to_string()
}

/// The profile, region, and endpoint arguments every command takes.
fn scope_args(location: &S3Location) -> Vec<String> {
    let mut args = Vec::new();
    for (flag, value) in [
        ("--profile", &location.profile),
        ("--region", &location.region),
        ("--endpoint-url", &location.endpoint),
    ] {
        if !value.is_empty() {
            args.push(flag.to_string());
            args.push(value.clone());
        }
    }
    args
}

fn encryption_args(location: &S3Location) -> Vec<String> {
    let mut args = Vec::new();
    if !location.encryption.is_empty() {
        args.push("--sse".to_string());
        args.push(location.encryption.clone());
    }
    if location.encryption == "aws:kms" && !location.kms_key_id.is_empty() {
        args.push("--sse-kms-key-id".to_string());
        args.push(location.kms_key_id.clone());
    }
    args
}

/// Turns an expired SSO sign-in into the message the dialogs offer a Sign in button for.
fn explain(err: String) -> String {
    if sso_expired(&err) {
        format!("{SSO_EXPIRED} Sign in to AWS, then try again.")
    } else {
        err
    }
}

async fn run(location: &S3Location, args: &[&str]) -> Result<Vec<u8>, String> {
    let args = args
        .iter()
        .map(|arg| arg.to_string())
        .chain(scope_args(location));
    run_tool(&mut aws_command(args), LABEL)
        .await
        .map_err(explain)
}

fn parse_json(stdout: &[u8]) -> Result<Value, String> {
    if stdout.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Null);
    }
    serde_json::from_slice(stdout)
        .map_err(|err| format!("{LABEL} printed something Recon couldn't read: {err}"))
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

pub async fn list_buckets(location: &S3Location) -> Result<Vec<String>, String> {
    let stdout = run(
        location,
        &[
            "s3api",
            "list-buckets",
            "--query",
            "Buckets[].Name",
            "--output",
            "json",
        ],
    )
    .await?;
    let mut names: Vec<String> = parse_json(&stdout)?
        .as_array()
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    Ok(names)
}

fn parse_listing(prefix: &str, value: &Value) -> S3Listing {
    let mut folders: Vec<String> = value["CommonPrefixes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|folder| text(folder, "Prefix"))
        .filter(|folder| !folder.is_empty())
        .collect();
    folders.sort();
    let mut files: Vec<S3File> = value["Contents"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|file| S3File {
            key: text(file, "Key"),
            size: file["Size"].as_u64().unwrap_or(0),
            modified: text(file, "LastModified"),
        })
        // Consoles make a folder by saving an empty object named after it.
        .filter(|file| !file.key.is_empty() && file.key != prefix && !file.key.ends_with('/'))
        .collect();
    files.sort_by(|a, b| a.key.cmp(&b.key));
    S3Listing {
        prefix: prefix.to_string(),
        folders,
        files,
        truncated: value["NextToken"].is_string(),
    }
}

/// The folders and files directly inside `prefix`.
pub async fn list(location: &S3Location, prefix: &str) -> Result<S3Listing, String> {
    let prefix = folder_prefix(prefix);
    let limit = LIST_LIMIT.to_string();
    let stdout = run(
        location,
        &[
            "s3api",
            "list-objects-v2",
            "--bucket",
            &location.bucket,
            "--prefix",
            &prefix,
            "--delimiter",
            "/",
            "--max-items",
            &limit,
            "--output",
            "json",
        ],
    )
    .await?;
    Ok(parse_listing(&prefix, &parse_json(&stdout)?))
}

/// Lists one entry, which needs the same permission browsing does.
pub async fn test(location: &S3Location) -> Result<String, String> {
    run(
        location,
        &[
            "s3api",
            "list-objects-v2",
            "--bucket",
            &location.bucket,
            "--prefix",
            &location.prefix,
            "--max-items",
            "1",
            "--output",
            "json",
        ],
    )
    .await?;
    Ok(format!(
        "Recon can list {}.",
        url(&location.bucket, &location.prefix)
    ))
}

/// The account and user or role the profile's credentials belong to. Needs no permissions.
pub async fn identity(profile: &str, region: &str) -> Result<AwsIdentity, String> {
    let scope = S3Location {
        profile: profile.trim().to_string(),
        region: region.trim().to_string(),
        ..Default::default()
    };
    let stdout = run(&scope, &["sts", "get-caller-identity", "--output", "json"]).await?;
    let value = parse_json(&stdout)?;
    Ok(AwsIdentity {
        account: text(&value, "Account"),
        arn: text(&value, "Arn"),
    })
}

pub async fn sign_in(profile: &str) -> Result<(), String> {
    crate::db::credentials::sign_in(profile.trim()).await
}

/// Percent-encodes `skipped` as one line of ASCII, keeping as many entries as fit in metadata.
fn encode_skipped(skipped: &[String]) -> String {
    fn encode(text: &str) -> String {
        let mut out = String::new();
        for byte in text.bytes() {
            if byte.is_ascii_alphanumeric()
                || matches!(byte, b' ' | b'-' | b'_' | b'.' | b'(' | b')')
            {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
        out
    }
    let mut kept: Vec<String> = Vec::new();
    for (index, item) in skipped.iter().enumerate() {
        let left = skipped.len() - index;
        let next = encode(item);
        let more = encode(&format!("and {left} more, listed in the backup's comments"));
        let used: usize = kept.iter().map(|entry| entry.len() + 3).sum();
        let room_for_more = if left > 1 { more.len() + 3 } else { 0 };
        if used + next.len() + room_for_more > SKIPPED_BUDGET {
            kept.push(more);
            break;
        }
        kept.push(next);
    }
    kept.join("%0A")
}

fn decode_skipped(value: &str) -> Vec<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let hex = bytes
            .get(index + 1..index + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[index], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                index += 3;
            }
            (byte, _) => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out)
        .split('\n')
        .filter(|item| !item.is_empty())
        .map(String::from)
        .collect()
}

/// An object to read, with what `head-object` said about it when the run started.
#[derive(Debug, Clone)]
pub struct Object {
    pub location: S3Location,
    pub key: String,
    pub size: u64,
    /// What a Recon backup left out, from its metadata.
    pub skipped: Vec<String>,
    path_env: String,
}

struct Head {
    size: u64,
    etag: String,
    skipped: Vec<String>,
}

async fn head(location: &S3Location, key: &str) -> Result<Head, String> {
    let stdout = run(
        location,
        &[
            "s3api",
            "head-object",
            "--bucket",
            &location.bucket,
            "--key",
            key,
            "--output",
            "json",
        ],
    )
    .await
    .map_err(|err| {
        if err.contains("Not Found") || err.contains("(404)") {
            format!("{} doesn't exist.", url(&location.bucket, key))
        } else {
            err
        }
    })?;
    let value = parse_json(&stdout)?;
    Ok(Head {
        size: value["ContentLength"].as_u64().unwrap_or(0),
        etag: text(&value, "ETag"),
        skipped: value["Metadata"][SKIPPED_KEY]
            .as_str()
            .map(decode_skipped)
            .unwrap_or_default(),
    })
}

impl Object {
    pub async fn resolve(location: S3Location, key: String) -> Result<Self, String> {
        let location = sanitize(location)?;
        let found = head(&location, &key).await?;
        Ok(Object {
            location,
            key,
            size: found.size,
            skipped: found.skipped,
            path_env: ssh_config::login_path().await.to_string(),
        })
    }

    pub fn name(&self) -> String {
        key_name(&self.key)
    }

    pub fn url(&self) -> String {
        url(&self.location.bucket, &self.key)
    }

    /// Changes when the object does, so a resumed run never skips ahead in a different file.
    pub async fn fingerprint(&self) -> Result<String, String> {
        let found = head(&self.location, &self.key).await?;
        Ok(format!("s3:{}:{}", found.size, found.etag))
    }

    /// Streams the object from the start, never writing it to disk.
    pub fn download(&self) -> Result<Download, String> {
        let mut command = Command::new("aws");
        command
            .args(["s3", "cp", &self.url(), "-", "--only-show-errors"])
            .args(scope_args(&self.location));
        let mut process = spawn(&mut command, &self.path_env, false)?;
        let stdout = process
            .child
            .stdout
            .take()
            .ok_or("The AWS CLI didn't start.")?;
        Ok(Download {
            process,
            stdout,
            finished: false,
        })
    }
}

/// A running `aws` command, killed if it's dropped before it finished.
struct Process {
    child: Child,
    stderr: Option<JoinHandle<Vec<u8>>>,
    finished: bool,
}

fn spawn(command: &mut Command, path_env: &str, input: bool) -> Result<Process, String> {
    hide_std_window(command)
        .env("PATH", path_env)
        .stdin(if input { Stdio::piped() } else { Stdio::null() })
        .stdout(if input { Stdio::null() } else { Stdio::piped() })
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|err| {
        if err.kind() == io::ErrorKind::NotFound {
            NOT_FOUND.to_string()
        } else {
            format!("{LABEL} couldn't start: {err}")
        }
    })?;
    let stderr = child.stderr.take().map(|mut stderr| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes);
            bytes
        })
    });
    Ok(Process {
        child,
        stderr,
        finished: false,
    })
}

impl Process {
    /// Waits for the command to exit, and fails with the end of its stderr unless it succeeded.
    fn wait(&mut self) -> io::Result<()> {
        let status = self.child.wait()?;
        self.finished = true;
        let stderr = self
            .stderr
            .take()
            .and_then(|thread| thread.join().ok())
            .unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        let tail = last_lines(&stderr);
        let message = if tail.is_empty() {
            format!("{LABEL} failed ({status}).")
        } else {
            format!("{LABEL} failed: {tail}")
        };
        Err(io::Error::other(explain(message)))
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// An object's bytes as `aws s3 cp` prints them. The end of the stream is an error if the download failed.
pub struct Download {
    process: Process,
    stdout: ChildStdout,
    finished: bool,
}

impl Read for Download {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.stdout.read(buf)?;
        if read == 0 && !buf.is_empty() && !self.finished {
            self.finished = true;
            self.process.wait()?;
        }
        Ok(read)
    }
}

/**
 * Streams into `aws s3 cp - s3://...`. Dropping it without `finish` kills
 * the upload, so a failed or cancelled backup never replaces the object.
 */
pub struct Upload {
    process: Process,
    stdin: Option<ChildStdin>,
    written: u64,
}

pub async fn upload(location: &S3Location, key: &str) -> Result<Upload, String> {
    let path_env = ssh_config::login_path().await;
    let mut command = Command::new("aws");
    command
        .args([
            "s3",
            "cp",
            "-",
            &url(&location.bucket, key),
            "--only-show-errors",
        ])
        .args(encryption_args(location))
        .args(scope_args(location));
    let mut process = spawn(&mut command, path_env, true)?;
    let stdin = process.child.stdin.take();
    Ok(Upload {
        process,
        stdin,
        written: 0,
    })
}

impl Upload {
    /// Ends the stream and waits for the upload to finish. Returns how many bytes it sent.
    pub fn finish(mut self) -> Result<u64, String> {
        drop(self.stdin.take());
        self.process.wait().map_err(|err| err.to_string())?;
        Ok(self.written)
    }

    /// A write failed because the CLI stopped reading, so its own error says why.
    fn failure(&mut self, err: io::Error) -> io::Error {
        if err.kind() != io::ErrorKind::BrokenPipe {
            return err;
        }
        drop(self.stdin.take());
        match self.process.wait() {
            Err(failed) => failed,
            Ok(()) => err,
        }
    }
}

impl Write for Upload {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(io::Error::from(io::ErrorKind::BrokenPipe));
        };
        match stdin.write(buf) {
            Ok(written) => {
                self.written += written as u64;
                Ok(written)
            }
            Err(err) => Err(self.failure(err)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.stdin.as_mut().map(Write::flush) {
            Some(Err(err)) => Err(self.failure(err)),
            _ => Ok(()),
        }
    }
}

/**
 * Stores what a backup left out as metadata on its object. The header is
 * uploaded before the dump knows, so this copies the object onto itself
 * with the metadata, which S3 does without sending it through Recon.
 */
pub async fn record_skipped(
    location: &S3Location,
    key: &str,
    skipped: &[String],
) -> Result<(), String> {
    let target = url(&location.bucket, key);
    let metadata = serde_json::json!({ SKIPPED_KEY: encode_skipped(skipped) }).to_string();
    let args = [
        "s3",
        "cp",
        &target,
        &target,
        "--metadata-directive",
        "REPLACE",
        "--metadata",
        &metadata,
        "--only-show-errors",
    ]
    .into_iter()
    .map(String::from)
    .chain(encryption_args(location))
    .chain(scope_args(location));
    run_tool_within(&mut aws_command(args), LABEL, COPY_TIMEOUT)
        .await
        .map(drop)
        .map_err(explain)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(bucket: &str) -> S3Location {
        S3Location {
            bucket: bucket.into(),
            ..Default::default()
        }
    }

    #[test]
    fn sanitizes_locations() {
        let pasted = sanitize(location(" s3://backups/prod/db ")).unwrap();
        assert_eq!(pasted.bucket, "backups");
        assert_eq!(pasted.prefix, "prod/db/");
        assert_eq!(pasted.name, "backups/prod/db");

        let mut kms = location("b");
        kms.prefix = "/nightly".into();
        kms.encryption = "aws:kms".into();
        kms.kms_key_id = " alias/backups ".into();
        kms.endpoint = "minio.local:9000/".into();
        let kms = sanitize(kms).unwrap();
        assert_eq!(kms.prefix, "nightly/");
        assert_eq!(kms.kms_key_id, "alias/backups");
        assert_eq!(kms.endpoint, "https://minio.local:9000");

        let mut plain = location("b");
        plain.encryption = "rot13".into();
        plain.kms_key_id = "unused".into();
        let plain = sanitize(plain).unwrap();
        assert_eq!(
            (plain.encryption.as_str(), plain.kms_key_id.as_str()),
            ("", "")
        );
        assert_eq!(plain.name, "b");

        assert!(sanitize(location("  ")).is_err());
    }

    #[test]
    fn builds_cli_arguments() {
        let mut target = location("b");
        target.profile = "prod-sso".into();
        target.endpoint = "https://minio.local".into();
        target.encryption = "aws:kms".into();
        target.kms_key_id = "alias/backups".into();
        assert_eq!(
            scope_args(&target),
            [
                "--profile",
                "prod-sso",
                "--endpoint-url",
                "https://minio.local"
            ]
        );
        assert_eq!(
            encryption_args(&target),
            ["--sse", "aws:kms", "--sse-kms-key-id", "alias/backups"]
        );
    }

    #[test]
    fn skipped_items_round_trip_through_metadata() {
        let skipped = vec![
            "reports.totals (no permission to read its definition)".to_string(),
            "café, ünïcode".to_string(),
        ];
        let encoded = encode_skipped(&skipped);
        assert!(encoded.is_ascii());
        assert!(!encoded.contains(','));
        assert_eq!(decode_skipped(&encoded), skipped);

        let many: Vec<String> = (0..200)
            .map(|index| format!("schema.routine_{index} (no permission to read its definition)"))
            .collect();
        let encoded = encode_skipped(&many);
        assert!(encoded.len() <= SKIPPED_BUDGET);
        let decoded = decode_skipped(&encoded);
        assert_eq!(decoded[0], many[0]);
        assert!(decoded.last().unwrap().starts_with("and "));
        assert!(decoded
            .last()
            .unwrap()
            .ends_with("more, listed in the backup's comments"));
    }

    /**
     * Streams through a real endpoint with the AWS CLI, into a bucket named
     * `recon-test`: `RECON_TEST_S3_ENDPOINT=http://127.0.0.1:5055 cargo test -- --ignored`.
     */
    #[tokio::test]
    #[ignore]
    async fn live_streams_backups_through_s3() {
        let Ok(endpoint) = std::env::var("RECON_TEST_S3_ENDPOINT") else {
            return;
        };
        let mut target = location("recon-test");
        target.endpoint = endpoint;
        let target = sanitize(target).unwrap();

        let mut dropped = upload(&target, "dropped.sql").await.unwrap();
        dropped.write_all(b"SELECT 1;\n").unwrap();
        drop(dropped);
        let missing = Object::resolve(target.clone(), "dropped.sql".into()).await;
        assert!(missing.unwrap_err().ends_with("doesn't exist."));

        let mut sent = upload(&target, "nightly/app.sql").await.unwrap();
        sent.write_all(b"SELECT 1;\n").unwrap();
        assert_eq!(sent.finish().unwrap(), 10);
        let skipped = vec!["reports.totals (no permission to read its definition)".to_string()];
        record_skipped(&target, "nightly/app.sql", &skipped)
            .await
            .unwrap();

        let object = Object::resolve(target.clone(), "nightly/app.sql".into())
            .await
            .unwrap();
        assert_eq!((object.size, object.skipped.clone()), (10, skipped));
        let mut text = String::new();
        object
            .download()
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert_eq!(text, "SELECT 1;\n");
        assert!(object.fingerprint().await.unwrap().starts_with("s3:10:"));

        let gone = Object {
            key: "nightly/gone.sql".into(),
            ..object.clone()
        };
        let mut text = String::new();
        let failed = gone.download().unwrap().read_to_string(&mut text);
        assert!(failed
            .unwrap_err()
            .to_string()
            .starts_with("The AWS CLI failed"));

        assert!(list(&target, "")
            .await
            .unwrap()
            .folders
            .contains(&"nightly/".to_string()));
        let inside = list(&target, "nightly").await.unwrap();
        let keys: Vec<&str> = inside.files.iter().map(|file| file.key.as_str()).collect();
        assert_eq!(keys, ["nightly/app.sql"]);
        assert!(test(&target).await.is_ok());
        assert!(list_buckets(&target)
            .await
            .unwrap()
            .contains(&"recon-test".to_string()));
    }

    #[test]
    fn parses_a_listing() {
        let value = serde_json::json!({
            "CommonPrefixes": [{ "Prefix": "prod/old/" }],
            "Contents": [
                { "Key": "prod/", "Size": 0, "LastModified": "2026-11-01T00:00:00Z" },
                { "Key": "prod/b.sql.gz", "Size": 20, "LastModified": "2026-11-02T00:00:00Z" },
                { "Key": "prod/a.sql.gz", "Size": 10, "LastModified": "2026-11-03T00:00:00Z" }
            ],
            "NextToken": "abc"
        });
        let listing = parse_listing("prod/", &value);
        assert_eq!(listing.folders, ["prod/old/"]);
        let keys: Vec<&str> = listing.files.iter().map(|file| file.key.as_str()).collect();
        assert_eq!(keys, ["prod/a.sql.gz", "prod/b.sql.gz"]);
        assert!(listing.truncated);
        assert!(parse_listing("", &Value::Null).files.is_empty());
    }
}
