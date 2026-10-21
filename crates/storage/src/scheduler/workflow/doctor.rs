//! Bounded read-only cross-database Workflow diagnostics.

use super::*;
use crate::control_db::ControlDb;
use crate::workflows::{
    WorkflowOperation, WorkflowOperationKind, WorkflowOperationResult, WorkflowRefState,
    WorkflowRepository,
};
use serde::Serialize;

/// Secret-free sampled authority and retained history facts.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowDatabaseInspection {
    /// Instances whose two immutable identities disagree or whose control row is absent.
    pub identity_mismatches: u64,
    /// Nonterminal control rows missing a typed artifact/definition reference.
    pub referrer_mismatches: u64,
    /// Histories whose descriptor, frontier, or byte counter is invalid.
    pub history_mismatches: u64,
    /// Recoverable creates not yet finalized in both databases.
    pub pending_creations: u64,
    /// Durable terminal instances awaiting control reference release.
    pub pending_releases: u64,
    /// Restart intents awaiting their exact scheduler/control convergence.
    pub pending_restarts: u64,
    /// Purge intents that still own the old external identity and artifact references.
    pub pending_purges: u64,
    /// Proven deletions whose control side is already absent, ready for receipt sweeping.
    pub pending_receipt_sweeps: u64,
    /// Terminal capability-two instances with healthy retained references.
    pub retained_instances: u64,
    /// Invalid operation results, deletion receipts or escaped transaction contexts.
    pub operation_mismatches: u64,
    /// Number of sampled scheduler histories.
    pub inspected_instances: u64,
    /// A full page was returned; operators must not interpret this as exhaustive validation.
    pub sampled: bool,
}

impl WorkflowDatabaseInspection {
    /// Whether an inspected persisted invariant failed; pending sagas are not corruption.
    #[must_use]
    pub const fn is_valid(&self) -> bool {
        self.identity_mismatches == 0
            && self.referrer_mismatches == 0
            && self.history_mismatches == 0
            && self.operation_mismatches == 0
    }
}

/// Inspect committed WAL-aware authority without migrations, recovery, checkpoints, or repairs.
/// SQLite may maintain ordinary WAL coordination sidecars, but database contents are read-only.
pub fn inspect_workflow_databases(
    control_path: &std::path::Path,
    scheduler_path: &std::path::Path,
    busy_timeout_ms: u64,
    limit: u32,
) -> Result<WorkflowDatabaseInspection, PlatformError> {
    bounded(limit)?;
    let control = ControlDb::open_readonly_wal_aware(control_path, busy_timeout_ms)?;
    let path = crate::control_db::leaf_nofollow_path(scheduler_path)?;
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(sql_error)?;
    connection
        .busy_timeout(std::time::Duration::from_millis(busy_timeout_ms))
        .map_err(sql_error)?;
    connection
        .pragma_update(None, "query_only", "ON")
        .map_err(sql_error)?;
    let instance_id = crate::identity::inspect_stored(&control)?.instance_id;
    let scheduler_id: String = connection
        .query_row(
            "SELECT instance_id FROM scheduler_identity WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if scheduler_id != instance_id.as_str() {
        return Err(error(ErrorCode::WorkflowInvariantViolation));
    }
    let scheduler = SchedulerStore {
        connection: std::sync::Mutex::new(connection),
        wake: std::sync::Arc::new(crate::scheduler::SchedulerWakeSignal::default()),
        instance_id,
    };
    let repository = WorkflowRepository::new(&control);
    let reservations = repository.live_reservations(None, limit)?;
    let ids = scheduler.workflow_instance_ids(None, limit)?;
    let receipts = scheduler.workflow_gc_receipts(None, limit)?;
    let mut result = WorkflowDatabaseInspection {
        sampled: reservations.len() == limit as usize
            || ids.len() == limit as usize
            || receipts.len() == limit as usize,
        ..WorkflowDatabaseInspection::default()
    };
    match durable_progress::sample_operation_progress(&*scheduler.lock()?, limit) {
        Ok(sampled) => result.sampled |= sampled,
        Err(failure) if failure.code() == ErrorCode::WorkflowInvariantViolation => {
            result.operation_mismatches += 1;
        }
        Err(failure) => return Err(failure),
    }
    for reservation in reservations {
        let identity = &reservation.identity;
        let instance = scheduler.workflow_instance(identity.instance_id)?;
        if let Some(operation) = repository.instance_operation(identity.instance_id)? {
            match operation.kind() {
                WorkflowOperationKind::Restart => result.pending_restarts += 1,
                WorkflowOperationKind::Purge => result.pending_purges += 1,
            }
            if !operation_matches(&scheduler, &operation, instance.as_ref())? {
                result.identity_mismatches += 1;
            }
        } else {
            match instance {
                Some(instance) if instance.identity != *identity => result.identity_mismatches += 1,
                Some(instance) if instance.state.is_terminal() => {
                    if reservation.state == WorkflowRefState::Retained {
                        result.retained_instances += 1;
                    } else {
                        result.pending_releases += 1;
                    }
                }
                Some(_) | None if reservation.state == WorkflowRefState::Creating => {
                    result.pending_creations += 1;
                }
                Some(_) if reservation.state != WorkflowRefState::Live => {
                    result.identity_mismatches += 1;
                }
                Some(_) => {}
                None => result.identity_mismatches += 1,
            }
        }
        if !repository.instance_referrers_intact(identity)? {
            result.referrer_mismatches += 1;
        }
    }
    for id in ids {
        let instance = scheduler
            .workflow_instance(id)?
            .ok_or_else(|| error(ErrorCode::WorkflowInvariantViolation))?;
        if let Some(operation) = repository.instance_operation(id)? {
            if !operation_matches(&scheduler, &operation, Some(&instance))? {
                result.identity_mismatches += 1;
            }
        } else {
            match repository.reservation(id)? {
                Some(reservation)
                    if reservation.identity == instance.identity
                        && (instance.state.is_terminal()
                            || matches!(
                                reservation.state,
                                WorkflowRefState::Creating | WorkflowRefState::Live
                            )) => {}
                _ => result.identity_mismatches += 1,
            }
        }
        match scheduler.verify_workflow_history(id) {
            Ok(()) => {}
            Err(error) if error.code() == ErrorCode::WorkflowInvariantViolation => {
                result.history_mismatches += 1;
            }
            Err(error) => return Err(error),
        }
        result.inspected_instances += 1;
    }
    for receipt in receipts {
        match repository.acknowledge_workflow_gc(&receipt) {
            Ok(_) => result.pending_receipt_sweeps += 1,
            Err(failure) if failure.code() == ErrorCode::WorkflowInstanceBusy => {}
            Err(failure) => return Err(failure),
        }
    }
    Ok(result)
}

fn operation_matches(
    scheduler: &SchedulerStore,
    operation: &WorkflowOperation,
    instance: Option<&WorkflowInstanceRecord>,
) -> Result<bool, PlatformError> {
    match scheduler.workflow_operation_result(operation)? {
        Some(WorkflowOperationResult::Applied(_)) => {
            if operation.kind() == WorkflowOperationKind::Purge {
                return Ok(instance.is_none());
            }
            let mut expected = operation.identity().clone();
            expected.instance_generation = operation.target_generation();
            Ok(instance.is_some_and(|instance| instance.identity == expected))
        }
        None | Some(WorkflowOperationResult::Rejected(_)) => {
            Ok(instance.is_some_and(|instance| instance.identity == *operation.identity()))
        }
    }
}


/// One dry-run fact per reconcile branch, no writes.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileInstanceFacts {
    pub instance_id: String,
    pub ref_state: String,
    pub external_instance_id: String,
    pub definition_id: String,
    pub scheduler_state: Option<String>,
    pub pending_operation: Option<String>,
    pub identity_mismatch: bool,
    pub scheduler_digest_mismatch: bool,
    pub history_error: Option<String>,
    pub referrers_intact: Option<bool>,
    pub branch_outcome: String,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReconcileDiagnosis {
    pub definitions: Vec<serde_json::Value>,
    pub checked_at_ms: i64,
    pub samples: Vec<serde_json::Value>,
    pub reports: Vec<ReconcileInstanceFacts>,
    pub scheduler_without_control_row: Vec<String>,
}

/// Walk the exact `WorkflowController::reconcile` branch order, read-only, and report
/// per instance which branch would abort. Never writes.
pub fn inspect_workflow_reconcile_failures(
    control_path: &std::path::Path,
    scheduler_path: &std::path::Path,
    now_ms: i64,
) -> Result<ReconcileDiagnosis, PlatformError> {
    let control = ControlDb::open_readonly_wal_aware(control_path, 30_000)?;
    let connection = Connection::open_with_flags(
        crate::control_db::leaf_nofollow_path(scheduler_path)?,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(sql_error)?;
    let _ = connection.pragma_update(None, "query_only", "ON");
    let scheduler = SchedulerStore {
        connection: std::sync::Mutex::new(connection),
        wake: std::sync::Arc::new(crate::scheduler::SchedulerWakeSignal::default()),
        instance_id: crate::identity::inspect_stored(&control)?.instance_id,
    };
    let repository = WorkflowRepository::new(&control);
    let mut diagnosis = ReconcileDiagnosis {
        checked_at_ms: now_ms,
        ..ReconcileDiagnosis::default()
    };
    let definitions = control.with_read(|conn| {
        let mut statement = conn
            .prepare("SELECT id,name,state,availability,availability_code FROM workflow_definitions")
            .map_err(sql_error)?;
        let mut rows = statement.query([]).map_err(sql_error)?;
        let mut collected = Vec::new();
        while let Some(row) = rows.next().map_err(sql_error)? {
            let id: String = row.get(0).map_err(sql_error)?;
            let name: String = row.get(1).map_err(sql_error)?;
            let state: String = row.get(2).map_err(sql_error)?;
            let availability: String = row.get(3).map_err(sql_error)?;
            let code: Option<String> = row.get(4).map_err(sql_error)?;
            collected.push(serde_json::json!({
                "id": id,
                "name": name,
                "state": state,
                "availability": availability,
                "code": code,
            }));
        }
        Ok(collected)
    })?;
    diagnosis.definitions = definitions;
    for reservation in repository.live_reservations(None, 1000)? {
        let identity = &reservation.identity;
        let pending = repository.instance_operation(identity.instance_id)?;
        let instance = scheduler.workflow_instance(identity.instance_id)?;
        let mut fact = ReconcileInstanceFacts {
            instance_id: identity.instance_id.to_string(),
            ref_state: format!("{:?}", reservation.state),
            external_instance_id: identity.external_instance_id.clone(),
            definition_id: identity.target.definition_id.to_string(),
            scheduler_state: instance.as_ref().map(|i| format!("{:?}", i.state)),
            pending_operation: pending.as_ref().map(|o| format!("{:?}", o.kind())),
            identity_mismatch: instance
                .as_ref()
                .is_some_and(|i| i.identity != *identity),
            scheduler_digest_mismatch: false,
            history_error: None,
            referrers_intact: None,
            branch_outcome: String::new(),
        };
        if let Some(record) = instance.as_ref() {
            fact.scheduler_digest_mismatch =
                crate::workflows::helpers::version_digest(&record.identity.target)?
                    != record.identity.target.descriptor_sha256;
            if let Err(error) = scheduler.verify_workflow_history(identity.instance_id) {
                fact.history_error = Some(error.code().as_str().to_string());
            }
        }
        fact.referrers_intact = Some(
            repository
                .instance_referrers_intact(identity)
                .unwrap_or(false),
        );
        if diagnosis.reports.len() < 3 {
            if let Some(record) = instance.as_ref() {
                let stored = hex::encode(record.identity.target.descriptor_sha256);
                let recomputed = crate::workflows::helpers::version_digest(&record.identity.target)
                    .map(hex::encode)
                    .unwrap_or_else(|_| "digest-error".to_string());
                let descriptor = serde_json::json!({
                    "schemaVersion": 2,
                    "instanceId": record.identity.target.instance_id,
                    "definitionId": record.identity.target.definition_id,
                    "workflowVersionId": record.identity.target.workflow_version_id,
                    "workerId": record.identity.target.worker_id,
                    "workerVersionId": record.identity.target.worker_version_id,
                    "workerCodeSha256": hex::encode(record.identity.target.worker_code_sha256),
                    "className": record.identity.target.class_name,
                    "loaderSchemaVersion": record.identity.target.loader_schema_version,
                    "capabilityVersion": record.identity.target.capability_version,
                });
                diagnosis.samples.push(serde_json::json!({
                    "instanceId": fact.instance_id,
                    "stored": stored,
                    "recomputed": recomputed,
                    "descriptor": descriptor,
                }));
            }
        }
        fact.branch_outcome = if fact.pending_operation.is_some() {
            "skip:pending-operation".to_string()
        } else if fact.identity_mismatch {
            "corrupt:identity".to_string()
        } else if instance.is_none() {
            if matches!(reservation.state, WorkflowRefState::Creating) {
                "saga:pending-creation".to_string()
            } else {
                "corrupt:scheduler-missing".to_string()
            }
        } else if instance.as_ref().is_some_and(|i| i.state.is_terminal()) {
            "retain".to_string()
        } else if !matches!(reservation.state, WorkflowRefState::Live) {
            "corrupt:ref-state".to_string()
        } else {
            "verify-history".to_string()
        };
        diagnosis.reports.push(fact);
    }
    for id in scheduler.workflow_instance_ids(None, 1000)? {
        if scheduler.workflow_instance(id)?.is_some()
            && !diagnosis
                .reports
                .iter()
                .any(|f| f.instance_id == id.to_string())
        {
            diagnosis.scheduler_without_control_row.push(id.to_string());
        }
    }
    Ok(diagnosis)
}
