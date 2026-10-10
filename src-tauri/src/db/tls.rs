use tokio::process::Command;

use super::credentials::{aws_command, run_tool};
use super::ssh_config::expand_home;
use crate::models::ConnectionEntry;

const CURL: &str = "/usr/bin/curl";
pub const HTTP_NOT_ALLOWED: &str =
    "Certificates can't be downloaded over http://, since anyone on the network could swap them. Use https://.";

/// Where a certificate or key comes from.
#[derive(Debug, PartialEq, Eq)]
pub enum Location<'a> {
    File(&'a str),
    Https(&'a str),
    S3(&'a str),
}

impl<'a> Location<'a> {
    pub fn parse(value: &'a str) -> Result<Self, String> {
        let lower = value.to_ascii_lowercase();
        if lower.starts_with("http://") {
            return Err(HTTP_NOT_ALLOWED.into());
        }
        if lower.starts_with("https://") {
            return Ok(Location::Https(value));
        }
        if let Some(rest) = lower.strip_prefix("s3://") {
            return match rest.split_once('/') {
                Some((bucket, key)) if !bucket.is_empty() && !key.is_empty() => Ok(Location::S3(value)),
                _ => Err(format!("{value} isn't a full S3 location. Use s3://bucket/path/to/file.pem.")),
            };
        }
        Ok(Location::File(value))
    }
}

pub fn is_s3(value: &str) -> bool {
    matches!(Location::parse(value), Ok(Location::S3(_)))
}

/// PEM data for the connection's TLS settings, read once per connect.
#[derive(Debug, Clone, Default)]
pub struct TlsMaterial {
    pub ca: Option<Vec<u8>>,
    pub cert: Option<Vec<u8>>,
    pub key: Option<Vec<u8>>,
}

fn check_pem(bytes: Vec<u8>, what: &str, value: &str) -> Result<Vec<u8>, String> {
    if bytes.windows(10).any(|window| window == b"-----BEGIN") {
        Ok(bytes)
    } else {
        Err(format!("The {what} from {value} isn't a PEM file."))
    }
}

async fn read(entry: &ConnectionEntry, value: &str, what: &str) -> Result<Option<Vec<u8>>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let bytes = match Location::parse(value)? {
        Location::File(path) => tokio::fs::read(expand_home(path))
            .await
            .map_err(|err| format!("Could not read the {what} {path}: {err}"))?,
        Location::Https(url) => {
            let mut command = Command::new(CURL);
            command.args(["-fsSL", "--max-time", "30", url]);
            run_tool(&mut command, &format!("Downloading the {what}")).await?
        }
        Location::S3(uri) => {
            let mut args = vec!["s3".to_string(), "cp".into(), uri.to_string(), "-".into()];
            if !entry.aws_profile.is_empty() {
                args.extend(["--profile".to_string(), entry.aws_profile.clone()]);
            }
            if !entry.aws_region.is_empty() {
                args.extend(["--region".to_string(), entry.aws_region.clone()]);
            }
            run_tool(&mut aws_command(args), &format!("Reading the {what} from S3")).await?
        }
    };
    check_pem(bytes, what, value).map(Some)
}

/**
 * Reads the CA certificate, client certificate, and client key, from files,
 * https URLs, or S3. They stay in memory, so keys from S3 never touch disk.
 */
pub async fn load(entry: &ConnectionEntry) -> Result<TlsMaterial, String> {
    Ok(TlsMaterial {
        ca: read(entry, &entry.ssl_ca_path, "CA certificate").await?,
        cert: read(entry, &entry.ssl_cert_path, "client certificate").await?,
        key: read(entry, &entry.ssl_key_path, "client key").await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_locations() {
        assert_eq!(Location::parse("~/certs/ca.pem"), Ok(Location::File("~/certs/ca.pem")));
        assert_eq!(
            Location::parse("https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem"),
            Ok(Location::Https("https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem"))
        );
        assert_eq!(Location::parse("s3://team-certs/db/ca.pem"), Ok(Location::S3("s3://team-certs/db/ca.pem")));
        assert_eq!(Location::parse("HTTP://example.com/ca.pem").unwrap_err(), HTTP_NOT_ALLOWED);
        assert!(Location::parse("s3://team-certs").is_err());
        assert!(Location::parse("s3://team-certs/").is_err());
        assert!(Location::parse("s3:///ca.pem").is_err());
        assert!(is_s3("s3://b/k") && !is_s3("/tmp/ca.pem"));
    }

    #[tokio::test]
    async fn loads_local_pem_files_and_rejects_others() {
        let dir = std::env::temp_dir().join(format!("recon-tls-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let pem = dir.join("ca.pem");
        let junk = dir.join("junk.txt");
        std::fs::write(&pem, "-----BEGIN CERTIFICATE-----\nabc\n-----END CERTIFICATE-----\n").unwrap();
        std::fs::write(&junk, "<html>not found</html>").unwrap();

        let mut entry: ConnectionEntry =
            serde_json::from_value(serde_json::json!({"name": "db", "driver": "postgres"})).unwrap();
        entry.ssl_ca_path = pem.to_string_lossy().into_owned();
        let material = load(&entry).await.unwrap();
        assert!(material.ca.is_some() && material.cert.is_none() && material.key.is_none());

        entry.ssl_ca_path = junk.to_string_lossy().into_owned();
        assert!(load(&entry).await.unwrap_err().contains("isn't a PEM file"));
        entry.ssl_ca_path = dir.join("missing.pem").to_string_lossy().into_owned();
        assert!(load(&entry).await.unwrap_err().starts_with("Could not read the CA certificate"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
