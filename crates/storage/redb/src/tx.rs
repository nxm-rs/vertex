//! Transaction implementations wrapping redb transactions.

use std::collections::HashSet;
use std::mem::ManuallyDrop;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use metrics::{counter, histogram};
use redb::{ReadableTable, ReadableTableMetadata, TableDefinition};
use vertex_storage::{DatabaseError, DatabaseErrorInfo, DbTx, DbTxMut, Decode, Encode, Table};

use crate::metrics::{mode, operation};

/// Dynamically create a redb TableDefinition from a table name.
///
/// All tables use `&[u8]` for both keys and values — the vertex-storage
/// Encode/Decode traits handle keys; postcard+zstd handles values.
fn table_def(name: &str) -> TableDefinition<'_, &[u8], &[u8]> {
    TableDefinition::new(name)
}

/// Intern a table name string to get a `&'static str`.
///
/// redb's `TableDefinition::new` requires `&'static str` for table creation.
/// This function leaks the string on first use per unique name, then returns
/// the cached reference on subsequent calls. Safe because table names are a
/// small finite set initialized once at startup.
fn intern_table_name(name: &str) -> &'static str {
    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let set = INTERNED.get_or_init(|| Mutex::new(HashSet::new()));
    let mut guard = set.lock().unwrap();
    if let Some(&existing) = guard.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
    guard.insert(leaked);
    leaked
}

/// Deserialize a value from raw bytes, decompressing first if the table uses compression.
fn decode_value<T: Table>(raw: &[u8]) -> Result<T::Value, DatabaseError> {
    if T::COMPRESS_VALUES {
        let bytes = zstd::decode_all(raw)
            .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                format!("zstd decompress {}", T::NAME), e)))?;
        postcard::from_bytes(&bytes)
    } else {
        postcard::from_bytes(raw)
    }.map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
        format!("deserialize {}", T::NAME), e)))
}

/// Record a completed DB operation's metrics.
fn record_op(table: &'static str, op: &'static str, outcome: &'static str, elapsed: f64) {
    counter!("db_operations_total", "table" => table, "operation" => op, "outcome" => outcome)
        .increment(1);
    histogram!("db_operation_duration_seconds", "table" => table, "operation" => op)
        .record(elapsed);
}

/// Shared read operations for both read-only and read-write transactions.
///
/// Both `RedbReadTx` and `RedbWriteTx` have an `inner` field whose
/// `open_table` returns a type implementing `ReadableTable + ReadableTableMetadata`.
macro_rules! impl_db_tx_reads {
    () => {
        fn get<T: Table>(&self, key: T::Key) -> Result<Option<T::Value>, DatabaseError> {
            let start = Instant::now();
            let _span = tracing::trace_span!("db_get", table = T::NAME).entered();

            let def = table_def(T::NAME);
            let table = self.inner.open_table(def)
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("open table {}", T::NAME), e)))?;
            let encoded = key.encode();
            let result = match table.get(encoded.as_ref())
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("get from {}", T::NAME), e)))?
            {
                Some(guard) => Ok(Some(decode_value::<T>(guard.value())?)),
                None => Ok(None),
            };

            record_op(T::NAME, operation::GET, "success", start.elapsed().as_secs_f64());
            result
        }

        fn entries<T: Table>(&self) -> Result<Vec<(T::Key, T::Value)>, DatabaseError> {
            let start = Instant::now();
            let _span = tracing::trace_span!("db_entries", table = T::NAME).entered();

            let def = table_def(T::NAME);
            let table = self.inner.open_table(def)
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("open table {}", T::NAME), e)))?;
            let len = table.len()
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("len {}", T::NAME), e)))?;
            let mut result = Vec::with_capacity(len as usize);
            for entry in table.iter()
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("iter {}", T::NAME), e)))?
            {
                let (k, v) = entry
                    .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                        format!("read entry from {}", T::NAME), e)))?;
                let key = T::Key::decode(k.value())?;
                let value = decode_value::<T>(v.value())?;
                result.push((key, value));
            }

            record_op(T::NAME, operation::ENTRIES, "success", start.elapsed().as_secs_f64());
            Ok(result)
        }

        fn keys<T: Table>(&self) -> Result<Vec<T::Key>, DatabaseError> {
            let start = Instant::now();
            let _span = tracing::trace_span!("db_keys", table = T::NAME).entered();

            let def = table_def(T::NAME);
            let table = self.inner.open_table(def)
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("open table {}", T::NAME), e)))?;
            let len = table.len()
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("len {}", T::NAME), e)))?;
            let mut result = Vec::with_capacity(len as usize);
            for entry in table.iter()
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("iter {}", T::NAME), e)))?
            {
                let (k, _v) = entry
                    .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                        format!("read key from {}", T::NAME), e)))?;
                result.push(T::Key::decode(k.value())?);
            }

            record_op(T::NAME, operation::KEYS, "success", start.elapsed().as_secs_f64());
            Ok(result)
        }

        fn count<T: Table>(&self) -> Result<usize, DatabaseError> {
            let start = Instant::now();
            let _span = tracing::trace_span!("db_count", table = T::NAME).entered();

            let def = table_def(T::NAME);
            let table = self.inner.open_table(def)
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("open table {}", T::NAME), e)))?;
            let len = table.len()
                .map_err(|e| DatabaseError::Read(DatabaseErrorInfo::with_source(
                    format!("len {}", T::NAME), e)))?;

            record_op(T::NAME, operation::COUNT, "success", start.elapsed().as_secs_f64());
            Ok(len as usize)
        }
    };
}

/// Read-only transaction wrapping `redb::ReadTransaction`.
pub struct RedbReadTx {
    inner: redb::ReadTransaction,
    start: Instant,
}

impl RedbReadTx {
    pub(crate) fn new(inner: redb::ReadTransaction) -> Self {
        Self { inner, start: Instant::now() }
    }
}

impl DbTx for RedbReadTx {
    impl_db_tx_reads!();
}

impl Drop for RedbReadTx {
    fn drop(&mut self) {
        histogram!("db_tx_duration_seconds", "mode" => mode::READ)
            .record(self.start.elapsed().as_secs_f64());
    }
}

/// Read-write transaction wrapping `redb::WriteTransaction`.
///
/// Uses `ManuallyDrop` so `commit()` can move out the inner transaction
/// while still recording tx duration on drop.
pub struct RedbWriteTx {
    inner: ManuallyDrop<redb::WriteTransaction>,
    start: Instant,
    committed: bool,
}

impl RedbWriteTx {
    pub(crate) fn new(inner: redb::WriteTransaction) -> Self {
        Self {
            inner: ManuallyDrop::new(inner),
            start: Instant::now(),
            committed: false,
        }
    }
}

impl DbTx for RedbWriteTx {
    impl_db_tx_reads!();
}

impl Drop for RedbWriteTx {
    fn drop(&mut self) {
        histogram!("db_tx_duration_seconds", "mode" => mode::WRITE)
            .record(self.start.elapsed().as_secs_f64());
        if !self.committed {
            // SAFETY: Only dropped once, and `commit()` sets committed=true
            // before this runs, so we only drop here if commit wasn't called.
            unsafe { ManuallyDrop::drop(&mut self.inner) };
        }
    }
}

impl DbTxMut for RedbWriteTx {
    fn commit(mut self) -> Result<(), DatabaseError> {
        let start = Instant::now();
        let _span = tracing::trace_span!("db_commit").entered();

        // SAFETY: We set committed=true so Drop won't double-drop.
        self.committed = true;
        let inner = unsafe { ManuallyDrop::take(&mut self.inner) };
        let result = inner.commit()
            .map_err(DatabaseError::commit_err);

        let outcome = if result.is_ok() { "success" } else { "failure" };
        counter!("db_operations_total", "table" => "", "operation" => operation::COMMIT, "outcome" => outcome)
            .increment(1);
        histogram!("db_tx_commit_duration_seconds").record(start.elapsed().as_secs_f64());

        result
    }

    fn put<T: Table>(&self, key: T::Key, value: T::Value) -> Result<(), DatabaseError> {
        let start = Instant::now();
        let _span = tracing::trace_span!("db_put", table = T::NAME).entered();

        let def = table_def(T::NAME);
        let mut table = self.inner.open_table(def)
            .map_err(|e| DatabaseError::write(T::NAME, 0, 0, format!("open table: {e}")))?;
        let encoded_key = key.encode();
        let serialized = postcard::to_allocvec(&value)
            .map_err(|e| DatabaseError::write(T::NAME, 0, 0, format!("serialize: {e}")))?;
        let stored = if T::COMPRESS_VALUES {
            zstd::encode_all(serialized.as_slice(), 3)
                .map_err(|e| DatabaseError::write(T::NAME, 0, 0, format!("zstd compress: {e}")))?
        } else {
            serialized
        };
        let key_bytes = encoded_key.as_ref();
        table.insert(key_bytes, stored.as_slice())
            .map_err(|e| DatabaseError::write_err(T::NAME, key_bytes.len(), stored.len(), e))?;

        record_op(T::NAME, operation::PUT, "success", start.elapsed().as_secs_f64());
        Ok(())
    }

    fn delete<T: Table>(&self, key: T::Key) -> Result<bool, DatabaseError> {
        let start = Instant::now();
        let _span = tracing::trace_span!("db_delete", table = T::NAME).entered();

        let def = table_def(T::NAME);
        let mut table = self.inner.open_table(def)
            .map_err(|e| DatabaseError::Delete(DatabaseErrorInfo::with_source(
                format!("open table {}", T::NAME), e)))?;
        let encoded = key.encode();
        let removed = table.remove(encoded.as_ref())
            .map_err(DatabaseError::delete_err)?;

        record_op(T::NAME, operation::DELETE, "success", start.elapsed().as_secs_f64());
        Ok(removed.is_some())
    }

    fn clear<T: Table>(&self) -> Result<(), DatabaseError> {
        let start = Instant::now();
        let _span = tracing::trace_span!("db_clear", table = T::NAME).entered();

        let def = table_def(T::NAME);
        let mut table = self.inner.open_table(def)
            .map_err(|e| DatabaseError::Delete(DatabaseErrorInfo::with_source(
                format!("open table {}", T::NAME), e)))?;
        table.retain(|_, _| false)
            .map_err(|e| DatabaseError::Delete(DatabaseErrorInfo::with_source(
                format!("clear {}", T::NAME), e)))?;

        record_op(T::NAME, operation::CLEAR, "success", start.elapsed().as_secs_f64());
        Ok(())
    }

    fn ensure_table(&self, name: &str) -> Result<(), DatabaseError> {
        let name_static = intern_table_name(name);
        let def: TableDefinition<'static, &[u8], &[u8]> = TableDefinition::new(name_static);
        let _ = self.inner.open_table(def)
            .map_err(|e| DatabaseError::CreateTable(DatabaseErrorInfo::with_source(
                format!("create table {name}"), e)))?;
        Ok(())
    }
}
