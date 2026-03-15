//! redb backend for vertex-storage.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use redb::backends::InMemoryBackend;
use vertex_storage::{Database, DatabaseError, DatabaseErrorInfo, DbTxMut};

pub mod metrics;
pub mod stats;
mod tx;

pub use tx::{RedbReadTx, RedbWriteTx};

/// redb-backed database implementing the `Database` trait.
pub struct RedbDatabase {
    inner: redb::Database,
    /// Path on disk, if backed by a file (None for in-memory).
    path: Option<PathBuf>,
}

impl RedbDatabase {
    /// Create or open a database at the given path.
    pub fn create(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let p = path.as_ref().to_path_buf();
        let inner = redb::Database::create(path)
            .map_err(DatabaseError::open_err)?;
        Ok(Self { inner, path: Some(p) })
    }

    /// Open an existing database (fails if it doesn't exist).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DatabaseError> {
        let p = path.as_ref().to_path_buf();
        let inner = redb::Database::open(path)
            .map_err(DatabaseError::open_err)?;
        Ok(Self { inner, path: Some(p) })
    }

    /// Create an in-memory database (no persistence).
    pub fn in_memory() -> Result<Self, DatabaseError> {
        let inner = redb::Database::builder()
            .create_with_backend(InMemoryBackend::new())
            .map_err(DatabaseError::open_err)?;
        Ok(Self { inner, path: None })
    }

    /// Access the underlying redb database.
    pub fn inner(&self) -> &redb::Database {
        &self.inner
    }

    /// File path on disk, if this database is file-backed.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Initialize tables by name, creating them if they don't exist.
    pub fn init_tables(&self, names: &[&str]) -> Result<(), DatabaseError> {
        self.update(|tx| {
            for name in names {
                tx.ensure_table(name)?;
            }
            Ok(())
        })
    }

    /// Wrap in an Arc for shared ownership.
    pub fn into_arc(self) -> Arc<Self> {
        Arc::new(self)
    }
}

impl Database for RedbDatabase {
    type TX = RedbReadTx;
    type TXMut = RedbWriteTx;

    fn tx(&self) -> Result<Self::TX, DatabaseError> {
        let inner = self.inner.begin_read()
            .map_err(|e| DatabaseError::InitTx(DatabaseErrorInfo::with_source("begin read tx", e)))?;
        Ok(RedbReadTx::new(inner))
    }

    fn tx_mut(&self) -> Result<Self::TXMut, DatabaseError> {
        let inner = self.inner.begin_write()
            .map_err(|e| DatabaseError::InitTx(DatabaseErrorInfo::with_source("begin write tx", e)))?;
        Ok(RedbWriteTx::new(inner))
    }
}

/// Open or create a database based on a configuration.
pub fn open_database(
    path: Option<&Path>,
    memory_only: bool,
) -> Result<Arc<RedbDatabase>, DatabaseError> {
    match path {
        Some(path) if !memory_only => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| DatabaseError::Open(DatabaseErrorInfo::with_source(
                        format!("create dir {}", parent.display()), e)))?;
            }
            tracing::info!(path = %path.display(), "Opening database");
            RedbDatabase::create(path).map(RedbDatabase::into_arc)
        }
        _ => {
            tracing::info!("Opening in-memory database");
            RedbDatabase::in_memory().map(RedbDatabase::into_arc)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vertex_storage::*;

    // A simple test key/value type.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
    struct TestKey(u32);

    impl Encode for TestKey {
        type Encoded = [u8; 4];
        fn encode(self) -> Self::Encoded {
            self.0.to_be_bytes()
        }
    }

    impl Decode for TestKey {
        fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
            let bytes: [u8; 4] = value.try_into().map_err(|_| DatabaseError::Decode)?;
            Ok(Self(u32::from_be_bytes(bytes)))
        }
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct TestValue(String);

    table!(TestTable, "test", TestKey, TestValue);

    struct TestTables;
    impl Tables for TestTables {
        const NAMES: &'static [&'static str] = &["test"];
    }

    fn setup() -> Arc<RedbDatabase> {
        let db = RedbDatabase::in_memory().unwrap();
        db.init_tables(TestTables::NAMES).unwrap();
        db.into_arc()
    }

    #[test]
    fn test_put_get() {
        let db = setup();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("hello".into()))?;
            Ok(())
        }).unwrap();

        let result = db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap();
        assert_eq!(result, Some(TestValue("hello".into())));
    }

    #[test]
    fn test_get_missing() {
        let db = setup();
        let result = db.view(|tx| tx.get::<TestTable>(TestKey(99))).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_delete() {
        let db = setup();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("hello".into()))?;
            Ok(())
        }).unwrap();

        let existed = db.update(|tx| tx.delete::<TestTable>(TestKey(1))).unwrap();
        assert!(existed);

        let result = db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_delete_nonexistent() {
        let db = setup();
        let existed = db.update(|tx| tx.delete::<TestTable>(TestKey(99))).unwrap();
        assert!(!existed);
    }

    #[test]
    fn test_clear() {
        let db = setup();
        db.update(|tx| {
            for i in 0..5 {
                tx.put::<TestTable>(TestKey(i), TestValue(format!("val{i}")))?;
            }
            Ok(())
        }).unwrap();

        assert_eq!(db.view(|tx| tx.count::<TestTable>()).unwrap(), 5);

        db.update(|tx| tx.clear::<TestTable>()).unwrap();
        assert_eq!(db.view(|tx| tx.count::<TestTable>()).unwrap(), 0);
    }

    #[test]
    fn test_entries() {
        let db = setup();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(2), TestValue("b".into()))?;
            tx.put::<TestTable>(TestKey(1), TestValue("a".into()))?;
            tx.put::<TestTable>(TestKey(3), TestValue("c".into()))?;
            Ok(())
        }).unwrap();

        let entries = db.view(|tx| tx.entries::<TestTable>()).unwrap();
        assert_eq!(entries.len(), 3);
        // redb stores entries sorted by key bytes (big-endian u32)
        assert_eq!(entries[0].0, TestKey(1));
        assert_eq!(entries[1].0, TestKey(2));
        assert_eq!(entries[2].0, TestKey(3));
    }

    #[test]
    fn test_overwrite() {
        let db = setup();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("first".into()))?;
            Ok(())
        }).unwrap();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("second".into()))?;
            Ok(())
        }).unwrap();

        let result = db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap();
        assert_eq!(result, Some(TestValue("second".into())));
    }

    #[test]
    fn test_persistence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.redb");

        // Write data
        {
            let db = RedbDatabase::create(&path).unwrap();
            db.init_tables(TestTables::NAMES).unwrap();
            db.update(|tx| {
                tx.put::<TestTable>(TestKey(42), TestValue("persisted".into()))?;
                Ok(())
            }).unwrap();
        }

        // Re-open and read
        {
            let db = RedbDatabase::open(&path).unwrap();
            let result = db.view(|tx| tx.get::<TestTable>(TestKey(42))).unwrap();
            assert_eq!(result, Some(TestValue("persisted".into())));
        }
    }

    #[test]
    fn test_count() {
        let db = setup();
        assert_eq!(db.view(|tx| tx.count::<TestTable>()).unwrap(), 0);

        db.update(|tx| {
            for i in 0..10 {
                tx.put::<TestTable>(TestKey(i), TestValue(format!("v{i}")))?;
            }
            Ok(())
        }).unwrap();

        assert_eq!(db.view(|tx| tx.count::<TestTable>()).unwrap(), 10);
    }

    #[test]
    fn test_open_database_memory() {
        let db = open_database(None, true).unwrap();
        db.init_tables(TestTables::NAMES).unwrap();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("mem".into()))?;
            Ok(())
        }).unwrap();
        let val = db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap();
        assert_eq!(val, Some(TestValue("mem".into())));
    }

    #[test]
    fn test_open_database_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir").join("test.redb");
        let db = open_database(Some(&path), false).unwrap();
        db.init_tables(TestTables::NAMES).unwrap();
        db.update(|tx| {
            tx.put::<TestTable>(TestKey(1), TestValue("file".into()))?;
            Ok(())
        }).unwrap();
        let val = db.view(|tx| tx.get::<TestTable>(TestKey(1))).unwrap();
        assert_eq!(val, Some(TestValue("file".into())));
    }
}

#[cfg(test)]
mod index_tests {
    use super::*;
    use vertex_storage::*;

    // -- Synthetic test types --

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
    struct UserId(u32);

    impl Encode for UserId {
        type Encoded = [u8; 4];
        fn encode(self) -> Self::Encoded {
            self.0.to_be_bytes()
        }
    }

    impl Decode for UserId {
        fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
            let bytes: [u8; 4] = value.try_into().map_err(|_| DatabaseError::Decode)?;
            Ok(Self(u32::from_be_bytes(bytes)))
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
    struct Email(String);

    impl Encode for Email {
        type Encoded = Vec<u8>;
        fn encode(self) -> Self::Encoded {
            self.0.into_bytes()
        }
    }

    impl Decode for Email {
        fn decode(value: &[u8]) -> Result<Self, DatabaseError> {
            String::from_utf8(value.to_vec())
                .map(Email)
                .map_err(|_| DatabaseError::Decode)
        }
    }

    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct UserRecord {
        name: String,
        email: Email,
    }

    table!(UserTable, "users", UserId, UserRecord);
    index!(EmailIndex, "users_by_email", Email, UserTable, |user| user.email.clone());

    fn setup() -> Arc<RedbDatabase> {
        let db = RedbDatabase::in_memory().unwrap();
        db.init_tables(&[UserTable::NAME, EmailIndex::NAME]).unwrap();
        db.into_arc()
    }

    #[test]
    fn test_put_indexed_and_get_via() {
        let db = setup();
        let user = UserRecord { name: "Alice".into(), email: Email("alice@test.com".into()) };

        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(UserId(1), user.clone())?;
            Ok(())
        }).unwrap();

        // Look up by primary key
        let by_pk = db.view(|tx| tx.get::<UserTable>(UserId(1))).unwrap();
        assert_eq!(by_pk, Some(user.clone()));

        // Look up via secondary index
        let by_email = db.view(|tx| tx.get_via::<EmailIndex>(Email("alice@test.com".into()))).unwrap();
        assert_eq!(by_email, Some(user));
    }

    #[test]
    fn test_get_via_missing() {
        let db = setup();
        let result = db.view(|tx| tx.get_via::<EmailIndex>(Email("nobody@test.com".into()))).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_put_indexed_update_same_index_key() {
        let db = setup();
        let user = UserRecord { name: "Alice".into(), email: Email("alice@test.com".into()) };

        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(UserId(1), user)?;
            Ok(())
        }).unwrap();

        // Update name but keep same email
        let updated = UserRecord { name: "Alice B.".into(), email: Email("alice@test.com".into()) };
        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(UserId(1), updated.clone())?;
            Ok(())
        }).unwrap();

        let by_email = db.view(|tx| tx.get_via::<EmailIndex>(Email("alice@test.com".into()))).unwrap();
        assert_eq!(by_email.unwrap().name, "Alice B.");

        // Only one entry in each table
        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 1);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 1);
    }

    #[test]
    fn test_put_indexed_update_changed_index_key() {
        let db = setup();
        let user = UserRecord { name: "Alice".into(), email: Email("old@test.com".into()) };

        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(UserId(1), user)?;
            Ok(())
        }).unwrap();

        // Update with new email
        let updated = UserRecord { name: "Alice".into(), email: Email("new@test.com".into()) };
        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(UserId(1), updated.clone())?;
            Ok(())
        }).unwrap();

        // Old email should not resolve
        let old = db.view(|tx| tx.get_via::<EmailIndex>(Email("old@test.com".into()))).unwrap();
        assert_eq!(old, None);

        // New email should resolve
        let new = db.view(|tx| tx.get_via::<EmailIndex>(Email("new@test.com".into()))).unwrap();
        assert_eq!(new, Some(updated));

        // Still one entry in each table
        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 1);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 1);
    }

    #[test]
    fn test_delete_indexed() {
        let db = setup();
        let user = UserRecord { name: "Alice".into(), email: Email("alice@test.com".into()) };

        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(UserId(1), user)?;
            Ok(())
        }).unwrap();

        let existed = db.update(|tx| tx.delete_indexed::<EmailIndex>(UserId(1))).unwrap();
        assert!(existed);

        // Both tables empty
        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 0);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 0);

        // Look up returns None
        let result = db.view(|tx| tx.get_via::<EmailIndex>(Email("alice@test.com".into()))).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_delete_indexed_nonexistent() {
        let db = setup();
        let existed = db.update(|tx| tx.delete_indexed::<EmailIndex>(UserId(99))).unwrap();
        assert!(!existed);
    }

    #[test]
    fn test_clear_indexed() {
        let db = setup();

        db.update(|tx| {
            for i in 0..5 {
                let user = UserRecord {
                    name: format!("User{i}"),
                    email: Email(format!("user{i}@test.com")),
                };
                tx.put_indexed::<EmailIndex>(UserId(i), user)?;
            }
            Ok(())
        }).unwrap();

        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 5);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 5);

        db.update(|tx| tx.clear_indexed::<EmailIndex>()).unwrap();

        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 0);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 0);
    }

    #[test]
    fn test_multiple_records_indexed() {
        let db = setup();

        db.update(|tx| {
            tx.put_indexed::<EmailIndex>(
                UserId(1),
                UserRecord { name: "Alice".into(), email: Email("alice@test.com".into()) },
            )?;
            tx.put_indexed::<EmailIndex>(
                UserId(2),
                UserRecord { name: "Bob".into(), email: Email("bob@test.com".into()) },
            )?;
            tx.put_indexed::<EmailIndex>(
                UserId(3),
                UserRecord { name: "Carol".into(), email: Email("carol@test.com".into()) },
            )?;
            Ok(())
        }).unwrap();

        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 3);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 3);

        // Each email resolves to the correct user
        let alice = db.view(|tx| tx.get_via::<EmailIndex>(Email("alice@test.com".into()))).unwrap().unwrap();
        assert_eq!(alice.name, "Alice");

        let bob = db.view(|tx| tx.get_via::<EmailIndex>(Email("bob@test.com".into()))).unwrap().unwrap();
        assert_eq!(bob.name, "Bob");

        // Delete one, others unaffected
        db.update(|tx| tx.delete_indexed::<EmailIndex>(UserId(2))).unwrap();
        assert_eq!(db.view(|tx| tx.count::<UserTable>()).unwrap(), 2);
        assert_eq!(db.view(|tx| tx.count::<EmailIndex>()).unwrap(), 2);

        assert!(db.view(|tx| tx.get_via::<EmailIndex>(Email("bob@test.com".into()))).unwrap().is_none());
        assert!(db.view(|tx| tx.get_via::<EmailIndex>(Email("alice@test.com".into()))).unwrap().is_some());
    }
}
