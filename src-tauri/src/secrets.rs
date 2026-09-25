use keyring::Entry;

const SERVICE: &str = "com.recon.app";

fn entry(connection_id: &str) -> Result<Entry, String> {
    Entry::new(SERVICE, connection_id)
        .map_err(|err| format!("Could not open the Keychain entry: {err}"))
}

pub fn get(connection_id: &str) -> Result<Option<String>, String> {
    match entry(connection_id)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(format!("Could not read the password from the Keychain: {err}")),
    }
}

pub fn set(connection_id: &str, password: &str) -> Result<(), String> {
    entry(connection_id)?
        .set_password(password)
        .map_err(|err| format!("Could not save the password to the Keychain: {err}"))
}

pub fn delete(connection_id: &str) -> Result<(), String> {
    match entry(connection_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(format!("Could not remove the password from the Keychain: {err}")),
    }
}

pub fn ssh_account(connection_id: &str) -> String {
    format!("{connection_id}:ssh")
}

pub fn delete_all(connection_id: &str) -> Result<(), String> {
    delete(connection_id)?;
    delete(&ssh_account(connection_id))
}
