//! Persistence in SQLite (`~/.local/share/rc-manager/rc-manager.db3`).

use crate::model::*;
use crate::vault::{self, Vault};
use anyhow::{anyhow, bail, Result};
use rusqlite::{params, Connection as Db, OptionalExtension};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value BLOB);
CREATE TABLE IF NOT EXISTS folders (
    id TEXT PRIMARY KEY,
    parent TEXT,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    sorting INTEGER NOT NULL DEFAULT 0,
    expanded INTEGER NOT NULL DEFAULT 1,
    credential TEXT
);
CREATE TABLE IF NOT EXISTS connections (
    id TEXT PRIMARY KEY,
    parent TEXT,
    name TEXT NOT NULL,
    protocol TEXT NOT NULL,
    host TEXT NOT NULL DEFAULT '',
    port INTEGER NOT NULL DEFAULT 0,
    description TEXT NOT NULL DEFAULT '',
    credential TEXT NOT NULL DEFAULT 'none',
    options TEXT NOT NULL DEFAULT '{}',
    mac TEXT NOT NULL DEFAULT '',
    sorting INTEGER NOT NULL DEFAULT 0,
    last_connected TEXT
);
CREATE TABLE IF NOT EXISTS credentials (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    username TEXT NOT NULL DEFAULT '',
    domain TEXT NOT NULL DEFAULT '',
    secret BLOB
);
-- Server certificates the user chose to trust: SHA-256 fingerprint per "host:port".
CREATE TABLE IF NOT EXISTS trusted_certs (
    host_port TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    added TEXT NOT NULL DEFAULT (datetime('now'))
);
"#;

pub struct Store {
    db: Db,
    dir: PathBuf,
    vault: Option<Vault>,
}

#[derive(Default)]
pub struct Data {
    pub folders: Vec<Folder>,
    pub connections: Vec<Connection>,
    pub credentials: Vec<Credential>,
}

fn uuid_opt(s: Option<String>) -> Option<Uuid> {
    s.and_then(|s| Uuid::parse_str(&s).ok())
}

fn cred_ref_to_str(c: CredentialRef) -> String {
    match c {
        CredentialRef::None => "none".into(),
        CredentialRef::Inherit => "inherit".into(),
        CredentialRef::Id(id) => id.to_string(),
    }
}

fn cred_ref_from_str(s: &str) -> CredentialRef {
    match s {
        "inherit" => CredentialRef::Inherit,
        _ => Uuid::parse_str(s).map(CredentialRef::Id).unwrap_or_default(),
    }
}

impl Store {
    pub fn default_dir() -> PathBuf {
        dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("rc-manager")
    }

    /// Moves the data of the first versions (named "RDP Manager", directory `rdp-manager`)
    /// to the new location once. Also carries eframe's window settings, which live there too.
    pub fn migrate_legacy_dir() -> Result<()> {
        let Some(base) = dirs::data_dir() else { return Ok(()) };
        let (old, new) = (base.join("rdp-manager"), base.join("rc-manager"));
        if old.is_dir() && !new.exists() {
            std::fs::rename(&old, &new)?;
        }
        Ok(())
    }

    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("rc-manager.db3");
        let old = dir.join("rdp-manager.db3");
        if !path.exists() && old.exists() {
            // Database of the versions named "RDP Manager"; move it together with WAL/SHM files.
            for suffix in ["", "-wal", "-shm"] {
                let from = PathBuf::from(format!("{}{suffix}", old.display()));
                if from.exists() {
                    std::fs::rename(&from, PathBuf::from(format!("{}{suffix}", path.display())))?;
                }
            }
        }
        let db = Db::open(&path)?;
        // Host names, user names and descriptions are stored in plain text; keep them
        // (and the key file) away from other local users. SQLite gives -wal/-shm the same mode.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "foreign_keys", "ON")?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db, dir: dir.to_path_buf(), vault: None })
    }

    fn meta(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    fn set_meta(&self, key: &str, value: Option<&[u8]>) -> Result<()> {
        match value {
            Some(v) => self.db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, v])?,
            None => self.db.execute("DELETE FROM meta WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    pub fn has_master_password(&self) -> Result<bool> {
        Ok(self.meta("kdf_salt")?.is_some())
    }

    /// Unlocks the vault. Without a master password `password` is ignored.
    pub fn unlock(&mut self, password: Option<&str>) -> Result<()> {
        let vault = match self.meta("kdf_salt")? {
            Some(salt) => {
                let v = Vault::from_password(password.unwrap_or(""), &salt)?;
                let verifier = self.meta("verifier")?.ok_or_else(|| anyhow!("Verifier is missing"))?;
                if !v.check_verifier(&verifier) {
                    bail!("Wrong master password");
                }
                v
            }
            None => Vault::from_key_file(&self.dir.join("vault.key"))?,
        };
        self.vault = Some(vault);
        Ok(())
    }

    /// Forgets the key; `unlock` is needed again before credentials can be read.
    pub fn lock(&mut self) {
        self.vault = None;
    }

    /// Data directory (database, key file, FreeRDP's private configuration).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn vault(&self) -> Result<&Vault> {
        self.vault.as_ref().ok_or_else(|| anyhow!("The vault is locked"))
    }

    /// Sets, changes or removes the master password and re-encrypts all passwords.
    /// Everything happens in one transaction: a failure leaves the old key and data intact.
    pub fn set_master_password(&mut self, new_password: Option<&str>) -> Result<()> {
        let creds = self.load_credentials()?;
        let key_file = self.dir.join("vault.key");
        let (new_vault, salt) = match new_password.filter(|p| !p.is_empty()) {
            Some(pw) => {
                let salt = vault::random_salt();
                (Vault::from_password(pw, &salt)?, Some(salt))
            }
            None => (Vault::from_key_file(&key_file)?, None),
        };
        let old_vault = self.vault.replace(new_vault);
        let result = (|| -> Result<()> {
            let tx = self.db.unchecked_transaction()?;
            match &salt {
                Some(salt) => {
                    self.set_meta("kdf_salt", Some(salt))?;
                    self.set_meta("verifier", Some(&self.vault()?.verifier()))?;
                }
                None => {
                    self.set_meta("kdf_salt", None)?;
                    self.set_meta("verifier", None)?;
                }
            }
            for c in &creds {
                self.save_credential(c)?;
            }
            tx.commit()?;
            Ok(())
        })();
        if let Err(e) = result {
            self.vault = old_vault;
            return Err(e);
        }
        if salt.is_some() {
            // The old local key could decrypt older copies of the database; it is no longer needed.
            std::fs::remove_file(&key_file).ok();
        }
        Ok(())
    }

    pub fn load(&self) -> Result<Data> {
        Ok(Data { folders: self.load_folders()?, connections: self.load_connections()?, credentials: self.load_credentials()? })
    }

    fn load_folders(&self) -> Result<Vec<Folder>> {
        let mut stmt = self.db.prepare("SELECT id, parent, name, description, sorting, expanded, credential FROM folders")?;
        let rows = stmt.query_map([], |r| {
            Ok(Folder {
                id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                parent: uuid_opt(r.get(1)?),
                name: r.get(2)?,
                description: r.get(3)?,
                sorting: r.get(4)?,
                expanded: r.get::<_, i64>(5)? != 0,
                credential: uuid_opt(r.get(6)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn load_connections(&self) -> Result<Vec<Connection>> {
        let mut stmt = self.db.prepare(
            "SELECT id, parent, name, protocol, host, port, description, credential, options, mac, sorting, last_connected FROM connections",
        )?;
        let rows = stmt.query_map([], |r| {
            let options: String = r.get(8)?;
            Ok(Connection {
                id: Uuid::parse_str(&r.get::<_, String>(0)?).unwrap_or_default(),
                parent: uuid_opt(r.get(1)?),
                name: r.get(2)?,
                protocol: Protocol::from_key(&r.get::<_, String>(3)?).unwrap_or(Protocol::Rdp),
                host: r.get(4)?,
                port: r.get::<_, i64>(5)?.clamp(0, 65535) as u16,
                description: r.get(6)?,
                credential: cred_ref_from_str(&r.get::<_, String>(7)?),
                options: serde_json::from_str(&options).unwrap_or_default(),
                mac: r.get(9)?,
                sorting: r.get(10)?,
                last_connected: r.get(11)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn load_credentials(&self) -> Result<Vec<Credential>> {
        let vault = self.vault()?;
        let mut stmt = self.db.prepare("SELECT id, name, username, domain, secret FROM credentials")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?, r.get::<_, Option<Vec<u8>>>(4)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, username, domain, secret) = row?;
            let password = match secret {
                Some(s) if !s.is_empty() => vault.decrypt(&s)?,
                _ => String::new(),
            };
            out.push(Credential { id: Uuid::parse_str(&id).unwrap_or_default(), name, username, domain, password });
        }
        Ok(out)
    }

    pub fn save_folder(&self, f: &Folder) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO folders (id, parent, name, description, sorting, expanded, credential) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![f.id.to_string(), f.parent.map(|p| p.to_string()), f.name, f.description, f.sorting, f.expanded as i64, f.credential.map(|c| c.to_string())],
        )?;
        Ok(())
    }

    pub fn delete_folder(&self, id: Uuid) -> Result<()> {
        self.db.execute("DELETE FROM folders WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    pub fn save_connection(&self, c: &Connection) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO connections (id, parent, name, protocol, host, port, description, credential, options, mac, sorting, last_connected)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                c.id.to_string(),
                c.parent.map(|p| p.to_string()),
                c.name,
                c.protocol.key(),
                c.host,
                c.port as i64,
                c.description,
                cred_ref_to_str(c.credential),
                serde_json::to_string(&c.options)?,
                c.mac,
                c.sorting,
                c.last_connected
            ],
        )?;
        Ok(())
    }

    pub fn delete_connection(&self, id: Uuid) -> Result<()> {
        self.db.execute("DELETE FROM connections WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }

    pub fn save_credential(&self, c: &Credential) -> Result<()> {
        let secret = if c.password.is_empty() { None } else { Some(self.vault()?.encrypt(&c.password)) };
        self.db.execute(
            "INSERT OR REPLACE INTO credentials (id, name, username, domain, secret) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![c.id.to_string(), c.name, c.username, c.domain, secret],
        )?;
        Ok(())
    }

    /// SHA-256 fingerprint (lower-case hex) of the certificate trusted for `host:port`.
    pub fn trusted_fingerprint(&self, host_port: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT fingerprint FROM trusted_certs WHERE host_port = ?1", [host_port.to_ascii_lowercase()], |r| r.get(0))
            .optional()?)
    }

    pub fn trust_certificate(&self, host_port: &str, fingerprint: &str) -> Result<()> {
        self.db.execute(
            "INSERT OR REPLACE INTO trusted_certs (host_port, fingerprint) VALUES (?1, ?2)",
            params![host_port.to_ascii_lowercase(), fingerprint],
        )?;
        Ok(())
    }

    pub fn delete_credential(&self, id: Uuid) -> Result<()> {
        let s = id.to_string();
        self.db.execute("DELETE FROM credentials WHERE id = ?1", [&s])?;
        self.db.execute("UPDATE connections SET credential = 'none' WHERE credential = ?1", [&s])?;
        self.db.execute("UPDATE folders SET credential = NULL WHERE credential = ?1", [&s])?;
        Ok(())
    }

    /// Writes many objects in one transaction (import).
    pub fn save_all(&self, data: &Data) -> Result<()> {
        let tx = self.db.unchecked_transaction()?;
        for f in &data.folders {
            self.save_folder(f)?;
        }
        for c in &data.credentials {
            self.save_credential(c)?;
        }
        for c in &data.connections {
            self.save_connection(c)?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rcm-test-{name}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn migrates_database_of_old_name() {
        let dir = temp_dir("migrate");
        let f = Folder::new("Old", None);
        {
            let mut s = Store::open(&dir).unwrap();
            s.unlock(None).unwrap();
            s.save_folder(&f).unwrap();
        }
        for suffix in ["", "-wal", "-shm"] {
            let new = dir.join(format!("rc-manager.db3{suffix}"));
            if new.exists() {
                std::fs::rename(&new, dir.join(format!("rdp-manager.db3{suffix}"))).unwrap();
            }
        }
        let mut s = Store::open(&dir).unwrap();
        s.unlock(None).unwrap();
        assert_eq!(s.load().unwrap().folders, vec![f]);
        assert!(!dir.join("rdp-manager.db3").exists());
        drop(s);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn crud_and_master_password() {
        let dir = temp_dir("store");
        let mut s = Store::open(&dir).unwrap();
        assert!(!s.has_master_password().unwrap());
        s.unlock(None).unwrap();

        let f = Folder::new("Customers", None);
        let mut cred = Credential::new("Admin");
        cred.username = "administrator".into();
        cred.password = "S3cret!".into();
        let mut c = Connection::new("Server 1", Protocol::Rdp, Some(f.id));
        c.host = "srv1.example.org".into();
        c.options.rdp.resolution = "1280x720".into();
        s.save_folder(&f).unwrap();
        s.save_credential(&cred).unwrap();
        s.save_connection(&c).unwrap();

        let d = s.load().unwrap();
        assert_eq!(d.folders, vec![f.clone()]);
        assert_eq!(d.connections, vec![c.clone()]);
        assert_eq!(d.credentials, vec![cred.clone()]);

        s.set_master_password(Some("master")).unwrap();
        drop(s);
        let mut s = Store::open(&dir).unwrap();
        assert!(s.has_master_password().unwrap());
        assert!(s.unlock(Some("wrong")).is_err());
        s.unlock(Some("master")).unwrap();
        assert_eq!(s.load().unwrap().credentials[0].password, "S3cret!");
        assert!(!dir.join("vault.key").exists(), "old local key must be removed");
        s.lock();
        assert!(s.load().is_err(), "locked store must not return credentials");
        s.unlock(Some("master")).unwrap();

        s.set_master_password(None).unwrap();
        drop(s);
        let mut s = Store::open(&dir).unwrap();
        s.unlock(None).unwrap();
        assert_eq!(s.load().unwrap().credentials[0].password, "S3cret!");

        assert_eq!(s.trusted_fingerprint("Srv:3389").unwrap(), None);
        s.trust_certificate("Srv:3389", "ab12").unwrap();
        assert_eq!(s.trusted_fingerprint("srv:3389").unwrap().as_deref(), Some("ab12"));

        s.delete_credential(cred.id).unwrap();
        assert_eq!(s.load().unwrap().connections[0].credential, CredentialRef::Inherit);
        std::fs::remove_dir_all(dir).ok();
    }
}
