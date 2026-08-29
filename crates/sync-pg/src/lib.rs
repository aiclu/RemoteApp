//! Optional, user-hosted PostgreSQL synchronization.
//!
//! The client never ships a synchronization service. A configured database is treated as one
//! private vault. Profile payloads are encrypted before they leave the device; the database stores
//! only opaque bytes plus revision metadata needed for optimistic conflict detection.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

pub const SCHEMA_NAME: &str = "remoteapp";
pub const CURRENT_SCHEMA_VERSION: i32 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SyncRecord {
    pub profile_id: Uuid,
    pub revision: i64,
    pub updated_at_ms: i64,
    pub updated_by_device: Uuid,
    pub deleted: bool,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
}

impl SyncRecord {
    pub fn validate(&self) -> Result<(), SyncError> {
        if self.revision < 1 {
            return Err(SyncError::InvalidRecord("revision must be positive".into()));
        }
        if self.nonce.len() != 24 {
            return Err(SyncError::InvalidRecord("nonce must be 24 bytes".into()));
        }
        if self.ciphertext.len() < 16 {
            return Err(SyncError::InvalidRecord("ciphertext is too short".into()));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MergeResult {
    NoChange,
    AppliedRemote(SyncRecord),
    MissingRemote {
        local: SyncRecord,
    },
    Conflict {
        local: SyncRecord,
        remote: SyncRecord,
    },
}

#[must_use]
pub fn compare_versions(local: &SyncRecord, remote: &SyncRecord) -> MergeResult {
    if local.profile_id != remote.profile_id {
        return MergeResult::Conflict {
            local: local.clone(),
            remote: remote.clone(),
        };
    }
    if local.revision == remote.revision && local.ciphertext == remote.ciphertext {
        MergeResult::NoChange
    } else if local.revision < remote.revision {
        MergeResult::AppliedRemote(remote.clone())
    } else {
        MergeResult::Conflict {
            local: local.clone(),
            remote: remote.clone(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SyncError {
    #[error("invalid sync record: {0}")]
    InvalidRecord(String),
    #[error("sync store is unavailable")]
    Unavailable,
    #[error("sync store operation failed: {0}")]
    Store(String),
    #[error("TLS configuration failed: {0}")]
    Tls(String),
    #[error("unsupported synchronization operation: {0}")]
    Unsupported(String),
}

#[derive(Debug, Default)]
pub struct MemorySyncStore {
    records: BTreeMap<Uuid, SyncRecord>,
}

impl MemorySyncStore {
    #[must_use]
    pub fn records(&self) -> impl Iterator<Item = &SyncRecord> {
        self.records.values()
    }

    pub fn insert(&mut self, record: SyncRecord) -> Result<(), SyncError> {
        record.validate()?;
        if let Some(existing) = self.records.get(&record.profile_id)
            && record.revision <= existing.revision
        {
            return Err(SyncError::Store("record revision is not newer".into()));
        }
        self.records.insert(record.profile_id, record);
        Ok(())
    }

    pub fn get(&self, profile_id: Uuid) -> Option<&SyncRecord> {
        self.records.get(&profile_id)
    }

    pub fn apply_local(
        &mut self,
        mut record: SyncRecord,
        expected_revision: Option<i64>,
    ) -> Result<MergeResult, SyncError> {
        record.validate()?;
        match self.records.get(&record.profile_id) {
            Some(remote) => {
                if expected_revision != Some(remote.revision) {
                    return Ok(MergeResult::Conflict {
                        local: record,
                        remote: remote.clone(),
                    });
                }
                record.revision = remote.revision.saturating_add(1);
            }
            None if expected_revision.is_some() => {
                return Ok(MergeResult::MissingRemote { local: record });
            }
            None => {}
        }
        self.records.insert(record.profile_id, record.clone());
        Ok(MergeResult::AppliedRemote(record))
    }
}

#[cfg_attr(not(feature = "postgres"), allow(dead_code))]
#[derive(Clone)]
pub struct DatabasePassword(String);

impl DatabasePassword {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[cfg(feature = "postgres")]
    #[must_use]
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for DatabasePassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted database password>")
    }
}

#[derive(Clone, Debug)]
pub struct PostgresConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: DatabasePassword,
}

impl PostgresConfig {
    pub fn validate(&self) -> Result<(), SyncError> {
        if self.host.trim().is_empty()
            || self.database.trim().is_empty()
            || self.username.trim().is_empty()
        {
            return Err(SyncError::InvalidRecord(
                "PostgreSQL host, database, and username are required".into(),
            ));
        }
        if self.port == 0 {
            return Err(SyncError::InvalidRecord(
                "PostgreSQL port must be non-zero".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "postgres")]
pub mod postgres {
    use std::{io::Cursor, sync::Arc};

    use rustls::{
        ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, SignatureScheme,
        client::{
            WebPkiServerVerifier,
            danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
        },
        pki_types::{CertificateDer, ServerName, UnixTime},
    };
    use sha2::{Digest, Sha256};
    use tokio::sync::Mutex;
    use tokio_postgres::{Client, Config as PgConfig, Row, types::ToSql};
    use tokio_postgres_rustls::MakeRustlsConnect;
    use tracing::error;

    use super::{CURRENT_SCHEMA_VERSION, PostgresConfig, SyncError, SyncRecord};

    const MIGRATION_SQL: &str = r#"
CREATE SCHEMA IF NOT EXISTS remoteapp;

CREATE TABLE IF NOT EXISTS remoteapp.schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS remoteapp.vault_metadata (
    singleton BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (singleton),
    format_version SMALLINT NOT NULL,
    salt BYTEA NOT NULL,
    wrapped_key BYTEA NOT NULL,
    created_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS remoteapp.connection_profiles (
    profile_id UUID PRIMARY KEY,
    revision BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    updated_by_device UUID NOT NULL,
    deleted BOOLEAN NOT NULL DEFAULT FALSE,
    nonce BYTEA NOT NULL,
    ciphertext BYTEA NOT NULL
);

CREATE INDEX IF NOT EXISTS connection_profiles_updated_at_idx
    ON remoteapp.connection_profiles (updated_at_ms);
"#;

    #[derive(Clone, Debug)]
    pub enum TlsTrust {
        WebPkiRoots,
        CustomCaPem(Vec<u8>),
        Sha256Fingerprint([u8; 32]),
    }

    pub fn rustls_config(trust: &TlsTrust) -> Result<ClientConfig, SyncError> {
        let mut roots = RootCertStore::empty();
        match trust {
            TlsTrust::WebPkiRoots => {
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
            TlsTrust::CustomCaPem(pem) => {
                let mut reader = Cursor::new(pem);
                for certificate in rustls_pemfile::certs(&mut reader) {
                    let certificate =
                        certificate.map_err(|error| SyncError::Tls(error.to_string()))?;
                    roots
                        .add(certificate)
                        .map_err(|error| SyncError::Tls(error.to_string()))?;
                }
            }
            TlsTrust::Sha256Fingerprint(expected) => {
                let mut signature_roots = RootCertStore::empty();
                signature_roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let delegate = WebPkiServerVerifier::builder(Arc::new(signature_roots))
                    .build()
                    .map_err(|error| SyncError::Tls(error.to_string()))?;
                let verifier = FingerprintVerifier {
                    expected: *expected,
                    delegate,
                };
                return Ok(ClientConfig::builder()
                    .dangerous()
                    .with_custom_certificate_verifier(Arc::new(verifier))
                    .with_no_client_auth());
            }
        }
        Ok(ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth())
    }

    #[derive(Debug)]
    struct FingerprintVerifier {
        expected: [u8; 32],
        delegate: Arc<WebPkiServerVerifier>,
    }

    impl ServerCertVerifier for FingerprintVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, TlsError> {
            let actual: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
            if actual == self.expected {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(TlsError::General(
                    "PostgreSQL certificate fingerprint mismatch".into(),
                ))
            }
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            self.delegate.verify_tls12_signature(message, cert, dss)
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, TlsError> {
            self.delegate.verify_tls13_signature(message, cert, dss)
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.delegate.supported_verify_schemes()
        }
    }

    pub struct PostgresStore {
        client: Mutex<Client>,
        connection_task: tokio::task::JoinHandle<()>,
    }

    impl std::fmt::Debug for PostgresStore {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("PostgresStore").finish_non_exhaustive()
        }
    }

    impl PostgresStore {
        pub async fn connect(config: &PostgresConfig, trust: &TlsTrust) -> Result<Self, SyncError> {
            config.validate()?;
            let tls = MakeRustlsConnect::new(rustls_config(trust)?);
            let mut pg = PgConfig::new();
            pg.host(&config.host)
                .port(config.port)
                .dbname(&config.database)
                .user(&config.username)
                .password(config.password.expose())
                .ssl_mode(tokio_postgres::config::SslMode::Require);
            let (client, connection) = pg
                .connect(tls)
                .await
                .map_err(|error| SyncError::Store(error.to_string()))?;
            let connection_task = tokio::spawn(async move {
                if let Err(error) = connection.await {
                    error!(error = %error, "PostgreSQL connection ended");
                }
            });
            let store = Self {
                client: Mutex::new(client),
                connection_task,
            };
            store.migrate().await?;
            Ok(store)
        }

        pub async fn migrate(&self) -> Result<(), SyncError> {
            let mut client = self.client.lock().await;
            let transaction = client
                .transaction()
                .await
                .map_err(|error| SyncError::Store(error.to_string()))?;
            transaction
                .batch_execute(MIGRATION_SQL)
                .await
                .map_err(|error| SyncError::Store(error.to_string()))?;
            transaction
                .execute(
                    "INSERT INTO remoteapp.schema_migrations (version, applied_at_ms) VALUES ($1, $2) ON CONFLICT (version) DO NOTHING",
                    &[
                        &CURRENT_SCHEMA_VERSION as &(dyn ToSql + Sync),
                        &now_ms() as &(dyn ToSql + Sync),
                    ],
                )
                .await
                .map_err(|error| SyncError::Store(error.to_string()))?;
            transaction
                .commit()
                .await
                .map_err(|error| SyncError::Store(error.to_string()))
        }

        pub async fn list(&self) -> Result<Vec<SyncRecord>, SyncError> {
            let client = self.client.lock().await;
            let rows = client
                .query(
                    "SELECT profile_id, revision, updated_at_ms, updated_by_device, deleted, nonce, ciphertext FROM remoteapp.connection_profiles ORDER BY updated_at_ms, profile_id",
                    &[],
                )
                .await
                .map_err(|error| SyncError::Store(error.to_string()))?;
            rows.into_iter().map(row_to_record).collect()
        }

        pub async fn get(&self, profile_id: uuid::Uuid) -> Result<Option<SyncRecord>, SyncError> {
            let client = self.client.lock().await;
            let row = client
                .query_opt(
                    "SELECT profile_id, revision, updated_at_ms, updated_by_device, deleted, nonce, ciphertext FROM remoteapp.connection_profiles WHERE profile_id = $1",
                    &[&profile_id],
                )
                .await
                .map_err(|error| SyncError::Store(error.to_string()))?;
            row.map(row_to_record).transpose()
        }

        pub async fn put_if_revision(
            &self,
            record: &SyncRecord,
            expected_revision: Option<i64>,
        ) -> Result<bool, SyncError> {
            record.validate()?;
            let client = self.client.lock().await;
            let count = match expected_revision {
                Some(expected) => client
                    .execute(
                        "UPDATE remoteapp.connection_profiles SET revision = $2, updated_at_ms = $3, updated_by_device = $4, deleted = $5, nonce = $6, ciphertext = $7 WHERE profile_id = $1 AND revision = $8",
                        &[
                            &record.profile_id as &(dyn ToSql + Sync),
                            &record.revision,
                            &record.updated_at_ms,
                            &record.updated_by_device,
                            &record.deleted,
                            &record.nonce,
                            &record.ciphertext,
                            &expected,
                        ],
                    )
                    .await
                    .map_err(|error| SyncError::Store(error.to_string()))?,
                None => client
                    .execute(
                        "INSERT INTO remoteapp.connection_profiles (profile_id, revision, updated_at_ms, updated_by_device, deleted, nonce, ciphertext) VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (profile_id) DO NOTHING",
                        &[
                            &record.profile_id as &(dyn ToSql + Sync),
                            &record.revision,
                            &record.updated_at_ms,
                            &record.updated_by_device,
                            &record.deleted,
                            &record.nonce,
                            &record.ciphertext,
                        ],
                    )
                    .await
                    .map_err(|error| SyncError::Store(error.to_string()))?,
            };
            Ok(count == 1)
        }
    }

    impl Drop for PostgresStore {
        fn drop(&mut self) {
            self.connection_task.abort();
        }
    }

    fn row_to_record(row: Row) -> Result<SyncRecord, SyncError> {
        let record = SyncRecord {
            profile_id: row
                .try_get(0)
                .map_err(|error| SyncError::Store(error.to_string()))?,
            revision: row
                .try_get(1)
                .map_err(|error| SyncError::Store(error.to_string()))?,
            updated_at_ms: row
                .try_get(2)
                .map_err(|error| SyncError::Store(error.to_string()))?,
            updated_by_device: row
                .try_get(3)
                .map_err(|error| SyncError::Store(error.to_string()))?,
            deleted: row
                .try_get(4)
                .map_err(|error| SyncError::Store(error.to_string()))?,
            nonce: row
                .try_get(5)
                .map_err(|error| SyncError::Store(error.to_string()))?,
            ciphertext: row
                .try_get(6)
                .map_err(|error| SyncError::Store(error.to_string()))?,
        };
        record.validate()?;
        Ok(record)
    }

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|duration| i64::try_from(duration.as_millis()).ok())
            .unwrap_or(i64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(revision: i64) -> SyncRecord {
        SyncRecord {
            profile_id: Uuid::from_u128(1),
            revision,
            updated_at_ms: revision,
            updated_by_device: Uuid::from_u128(2),
            deleted: false,
            nonce: vec![0; 24],
            ciphertext: vec![0; 16],
        }
    }

    #[test]
    fn newer_remote_version_is_applied() {
        assert!(matches!(
            compare_versions(&record(1), &record(2)),
            MergeResult::AppliedRemote(_)
        ));
    }

    #[test]
    fn equal_version_with_different_payload_is_a_conflict() {
        let mut remote = record(1);
        remote.ciphertext[0] = 1;
        assert!(matches!(
            compare_versions(&record(1), &remote),
            MergeResult::Conflict { .. }
        ));
    }

    #[test]
    fn missing_remote_version_is_not_fabricated_as_a_conflict() {
        let mut store = MemorySyncStore::default();
        let result = store.apply_local(record(1), Some(1)).unwrap();
        assert!(matches!(result, MergeResult::MissingRemote { .. }));
    }

    #[test]
    fn memory_store_rejects_stale_revision() {
        let mut store = MemorySyncStore::default();
        store.insert(record(2)).unwrap();
        assert!(store.insert(record(1)).is_err());
    }

    #[test]
    fn database_password_debug_is_redacted() {
        let password = DatabasePassword::new("secret");
        assert!(!format!("{password:?}").contains("secret"));
    }
}
