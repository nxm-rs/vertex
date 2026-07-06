//! Startup sweep for tables absent from the registry.

use std::collections::BTreeSet;

use super::{Database, DatabaseError};

/// What the sweep does with a table present on disk but absent from the registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnknownTablePolicy {
    /// Log the table and keep it. The safe default: a table missing from the
    /// current registry may be a not-yet-deployed feature or an older version
    /// mid-migration, so dropping it would destroy live data.
    #[default]
    Log,
    /// Log and drop the table. Destructive; correct only once the table is a
    /// confirmed retired schema that no deployed consumer will read again.
    Vacuum,
}

/// Outcome of a registry sweep.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Tables present on disk but absent from the registry.
    pub unknown: Vec<String>,
    /// Subset of `unknown` that was dropped (always empty under
    /// [`UnknownTablePolicy::Log`]).
    pub vacuumed: Vec<String>,
}

/// Compare the database's physical tables against `registry` and log or vacuum
/// any table not registered.
///
/// `registry` is read at call time (pass the same slice handed to table
/// initialization), so tables added by other consumers are swept without
/// changing this function. A registered table is never dropped.
pub fn sweep_unknown_tables<DB: Database>(
    db: &DB,
    registry: &[&str],
    policy: UnknownTablePolicy,
) -> Result<SweepReport, DatabaseError> {
    let Some(present) = db.table_names()? else {
        return Ok(SweepReport::default());
    };
    let registered: BTreeSet<&str> = registry.iter().copied().collect();
    let mut report = SweepReport::default();
    for name in present {
        if registered.contains(name.as_str()) {
            continue;
        }
        match policy {
            UnknownTablePolicy::Log => {
                tracing::warn!(
                    table = %name,
                    "table absent from registry retained; vacuum only once it is a confirmed retired schema",
                );
            }
            UnknownTablePolicy::Vacuum => {
                tracing::warn!(table = %name, "vacuuming table absent from registry");
                if db.drop_table(&name)? {
                    report.vacuumed.push(name.clone());
                }
            }
        }
        report.unknown.push(name);
    }
    Ok(report)
}
