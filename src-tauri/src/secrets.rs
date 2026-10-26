use keyring::Entry;

const SERVICE: &str = "com.recon.app";

#[cfg(target_os = "macos")]
const STORE: &str = "Keychain";
#[cfg(not(target_os = "macos"))]
const STORE: &str = "system keyring";

fn entry(connection_id: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, connection_id)
        .map_err(|err| format!("Could not open the {STORE} entry: {err}"))
}

/**
 * A Linux desktop without a Secret Service has nothing saved, so reads and
 * deletes succeed there and only saving a password reports the error. A locked
 * or dismissed keyring is NoStorageAccess and still surfaces.
 */
#[cfg(target_os = "macos")]
fn store_missing(_err: &keyring::Error) -> bool {
    false
}

#[cfg(not(target_os = "macos"))]
fn store_missing(err: &keyring::Error) -> bool {
    matches!(err, keyring::Error::PlatformFailure(_))
}

pub fn get(connection_id: &str) -> Result<Option<String>, String> {
    match entry(connection_id)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) if store_missing(&err) => Ok(None),
        Err(err) => Err(format!(
            "Could not read the password from the {STORE}: {err}"
        )),
    }
}

pub fn set(connection_id: &str, password: &str) -> Result<(), String> {
    entry(connection_id)?
        .set_password(password)
        .map_err(|err| format!("Could not save the password to the {STORE}: {err}"))
}

pub fn delete(connection_id: &str) -> Result<(), String> {
    match entry(connection_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) if store_missing(&err) => Ok(()),
        Err(err) => Err(format!(
            "Could not remove the password from the {STORE}: {err}"
        )),
    }
}

pub fn ssh_account(connection_id: &str) -> String {
    format!("{connection_id}:ssh")
}

pub fn ssh_password_account(connection_id: &str) -> String {
    format!("{connection_id}:ssh-password")
}

pub fn delete_all(connection_id: &str) -> Result<(), String> {
    delete(connection_id)?;
    delete(&ssh_account(connection_id))?;
    delete(&ssh_password_account(connection_id))
}
