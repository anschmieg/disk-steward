use std::env;
use std::io::{self, Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod store;

/// Shutdown evidence can only be issued after process exit and reader
/// completion have both been confirmed by bounded polling and the reader
/// has joined successfully.
pub(crate) struct StopReceipt {
    token: String,
    pid: u32,
}
impl StopReceipt {
    fn confirmed(token: &str, pid: u32) -> Self {
        Self {
            token: token.to_owned(),
            pid,
        }
    }
    pub(crate) fn token(&self) -> &str {
        &self.token
    }
    pub(crate) fn pid(&self) -> u32 {
        self.pid
    }
}

const OUTPUT_LIMIT: u64 = 64 * 1024;
const OUTER_DEADLINE: Duration = Duration::from_secs(1);
const READER_UNCONFIRMED: &str =
    "stdout reader completion or join unconfirmed; reader handle may be detached";
const FLOOD_BYTES: usize = 1024 * 1024;
const CHILD_HOLD: Duration = Duration::from_secs(10);
const READ_CHUNK: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Case {
    Flood,
    Stall,
    Cancel,
}

impl Case {
    fn name(self) -> &'static str {
        match self {
            Self::Flood => "flood",
            Self::Stall => "stall",
            Self::Cancel => "cancel",
        }
    }

    fn child_mode(self) -> &'static str {
        match self {
            Self::Cancel => "stall",
            Self::Flood => "flood",
            Self::Stall => "stall",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StopReason {
    OutputLimit,
    Deadline,
    Canceled,
    ChildExited,
    ReaderError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReaderEvent {
    Ready,
    Finished { bytes: u64, failed: bool },
}

#[derive(Debug)]
struct Report {
    case: Case,
    stop_reason: StopReason,
    bytes_read: u64,
    elapsed: Duration,
    child_exit: ExitStatus,
    reader_joined: bool,
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = if args.first().map(String::as_str) == Some("--child") {
        run_child(&args[1..])
    } else {
        run_parent(&args)
    };
    if let Err(error) = result {
        eprintln!("worker probe error: {error}");
        std::process::exit(1);
    }
}

fn run_parent(args: &[String]) -> Result<(), String> {
    match args {
        [] => {
            println!("Rust subprocess supervision probe (dry run; use --run to start fixed synthetic children)");
            println!("Cases: bounded stdout flood, one-second stall deadline, and explicit parent cancellation.");
            Ok(())
        }
        [flag] if flag == "--run-store" => {
            let store = store::OwnedStore::create()
                .map_err(|error| format!("create owned store: {error:?}"))?;
            let state = store
                .inspect_state()
                .map_err(|error| format!("inspect owned store: {error:?}"))?;
            println!(
                "store_state={state:?} token={} cleanup=not_authorized",
                store.token()
            );
            Ok(())
        }
        [flag] if flag == "--run-store-payload-fixture" => {
            let store = store::OwnedStore::create()
                .map_err(|_| "create payload fixture store failed".to_owned())?;
            let report = store.write_payload_fixture()
                .map_err(|_| "payload fixture incomplete; retain store for reconciliation".to_owned())?;
            println!("payload_written={} payload_limit={} cap_reached={} state=NeedsReconciliation cleanup=not_authorized",
                report.written_bytes, report.limit_bytes, report.cap_reached);
            Ok(())
        }
        [flag] if flag == "--run-store-worker" => run_store_worker(),
        [flag] if flag == "--run-store-reinspect" => run_store_reinspect(),
        [flag] if flag == "--run-indexed-store-worker" => run_indexed_store_worker(IndexedWorkerMode::Stall),
        [flag] if flag == "--run-indexed-store-handoff" => run_indexed_store_worker(IndexedWorkerMode::DescriptorOnly),
        [flag] if flag == "--run-indexed-store-payload" => run_indexed_store_worker(IndexedWorkerMode::Payload),
        [flag] if flag == "--run-indexed-store-payload-partial" => run_indexed_store_worker(IndexedWorkerMode::PartialPayload),
        [flag, confirm] if flag == "--run-store-cleanup" && confirm == "--confirm-own-store-cleanup" => run_store_worker_with_cleanup(true),
        [flag] if flag == "--run-store-cleanup" => Err("refusing store cleanup without --confirm-own-store-cleanup".into()),
        [flag] if flag == "--run" => {
            for case in [Case::Flood, Case::Stall, Case::Cancel] {
                let report = supervise(case)?;
                let expected = match case {
                    Case::Flood => (StopReason::OutputLimit, OUTPUT_LIMIT + 1),
                    Case::Stall => (StopReason::Deadline, 1),
                    Case::Cancel => (StopReason::Canceled, 1),
                };
                if (report.stop_reason, report.bytes_read) != expected {
                    return Err(format!(
                        "case={} produced stop={:?} bytes={}, expected stop={:?} bytes={}",
                        report.case.name(),
                        report.stop_reason,
                        report.bytes_read,
                        expected.0,
                        expected.1
                    ));
                }
                println!(
                    "case={} stop={:?} bytes={} elapsed_ms={} child_exit={} reader_joined={}",
                    report.case.name(),
                    report.stop_reason,
                    report.bytes_read,
                    report.elapsed.as_millis(),
                    report.child_exit,
                    report.reader_joined
                );
            }
            Ok(())
        }
        _ => Err("accepted arguments: none, --run, --run-store, --run-store-payload-fixture, --run-store-worker, --run-store-reinspect, --run-indexed-store-worker, --run-indexed-store-handoff, --run-indexed-store-payload, --run-indexed-store-payload-partial, or --run-store-cleanup --confirm-own-store-cleanup".into()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IndexedWorkerMode {
    Stall,
    DescriptorOnly,
    Payload,
    PartialPayload,
}

fn run_indexed_store_worker(mode: IndexedWorkerMode) -> Result<(), String> {
    let descriptor_handoff = mode != IndexedWorkerMode::Stall;
    use verification_budget::{BudgetController, JobState, Limits};
    use verification_store_registry::{
        RegistryError, StoreState as RState, VerificationStoreRegistry,
    };
    const FIXTURE_ID: &str = "0123456789abcdef0123456789abcdef";
    let mut store = store::OwnedStore::create().map_err(|e| format!("create store: {e}"))?;
    let registry_path = store
        .control_registry_path()
        .map_err(|e| format!("validate generated store parent: {e}"))?;
    let (parent, root) = store
        .control_identity()
        .map_err(|e| format!("validate store identity: {e}"))?;
    let mut registry = VerificationStoreRegistry::open(&registry_path)
        .map_err(|e| format!("open private registry: {e}"))?;
    registry
        .register(FIXTURE_ID, parent.0, parent.1, root.0, root.1)
        .map_err(|e| format!("register store: {e}"))?;
    let free_bytes = store
        .sample_free_space_bytes()
        .map_err(|_| "sample owned-store volume free space refused".to_owned())?;
    let mut admission_budget = BudgetController::new(Limits {
        headroom_bytes: 1024 * 1024,
    });
    let attempt_started = Instant::now();
    let admission = verification_admission::admit_registered_store(
        &mut admission_budget,
        &mut registry,
        FIXTURE_ID,
        store::WORKER_PAYLOAD_RESERVATION,
        free_bytes,
    )
    .map_err(|_| "coordinated pre-handoff admission refused".to_owned())?;
    let admitted_record = admission.record.clone();
    let mut registry_slot = Some(registry);
    let attempt_result = guard_indexed_attempt(&mut registry_slot, &admitted_record, |slot| {
        macro_rules! registry_mut {
            () => {
                slot.as_mut().ok_or_else(|| {
                    "registry lease unavailable inside admitted attempt".to_owned()
                })?
            };
        }
        macro_rules! registry_ref {
            () => {
                slot.as_ref().ok_or_else(|| {
                    "registry lease unavailable inside admitted attempt".to_owned()
                })?
            };
        }
        if admission.record.state != RState::Registered
            || admission.record.prior_state.is_some()
            || admission.record.reserved_bytes != store::WORKER_PAYLOAD_RESERVATION
            || (
                admission.record.parent_dev,
                admission.record.parent_ino,
                admission.record.root_dev,
                admission.record.root_ino,
            ) != (parent.0, parent.1, root.0, root.1)
            || admission_budget
                .snapshot(admission.job_id)
                .map(|job| job.state != JobState::Running || job.usage.stored_bytes != 0)
                .unwrap_or(true)
        {
            return Err("coordinated pre-handoff admission record mismatch".into());
        }
        if matches!(
            mode,
            IndexedWorkerMode::Payload | IndexedWorkerMode::PartialPayload
        ) {
            store
                .bind_worker_payload_reservation()
                .map_err(|_| "bind worker payload reservation refused".to_owned())?;
        }
        let exe = env::current_exe().map_err(|e| format!("locate own executable: {e}"))?;
        let child_stdin = if descriptor_handoff {
            let root = store
                .worker_stdin_once()
                .map_err(|_| "prepare store descriptor handoff refused".to_owned())?;
            Stdio::from(root)
        } else {
            Stdio::null()
        };
        let child_mode = match mode {
            IndexedWorkerMode::Stall => "stall",
            IndexedWorkerMode::DescriptorOnly => "store-handoff",
            IndexedWorkerMode::Payload => "store-payload",
            IndexedWorkerMode::PartialPayload => "store-payload-partial",
        };
        let mut child = Command::new(exe)
            .env_clear()
            .arg("--child")
            .arg(child_mode)
            .stdin(child_stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("start fixed child: {e}"))?;
        let pid = child.id();
        if let Err(e) = store.bind_worker(pid) {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err(format!("bind worker: {e}")),
                Err(s) => Err(format!("bind worker: {e}; cleanup failed: {s}")),
            };
        }
        let stdout = match child.stdout.take() {
            Some(v) => v,
            None => {
                return match stop_and_reap(&mut child) {
                    Ok(()) => Err("child stdout unavailable".into()),
                    Err(e) => Err(format!("child stdout unavailable; cleanup failed: {e}")),
                }
            }
        };
        let (tx, rx) = mpsc::sync_channel(2);
        let reader = match thread::Builder::new()
            .name("indexed-store-stdout".into())
            .spawn(move || read_bounded(stdout, tx))
        {
            Ok(v) => v,
            Err(e) => {
                return match stop_and_reap(&mut child) {
                    Ok(()) => Err(format!("start reader: {e}")),
                    Err(s) => Err(format!("start reader: {e}; cleanup failed: {s}")),
                }
            }
        };
        if mode == IndexedWorkerMode::PartialPayload {
            let failure_deadline = Instant::now() + OUTER_DEADLINE;
            let receive_time = failure_deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(receive_time) {
                Ok(ReaderEvent::Finished {
                    bytes: 0,
                    failed: false,
                }) => {}
                Ok(event) => {
                    return finish_after_error(
                        child,
                        reader,
                        rx,
                        format!("partial payload child emitted unexpected stdout event: {event:?}"),
                    )
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return finish_after_error(
                        child,
                        reader,
                        rx,
                        "partial payload child did not finish before deadline".into(),
                    )
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return finish_after_error(
                        child,
                        reader,
                        rx,
                        "partial payload reader stopped without a result".into(),
                    )
                }
            }
            let status = match wait_for_child_exit(&mut child, failure_deadline) {
                Ok(status) => status,
                Err(error) => return finish_after_error(child, reader, rx, error),
            };
            if let Err(error) = stop_and_reap(&mut child) {
                let reader_done = bounded_reader_join(reader);
                return Err(format!(
                    "partial payload child shutdown unconfirmed: {error}; reader completion confirmed={reader_done} (false means its handle may be detached)"
                ));
            }
            if !bounded_reader_join(reader) {
                return Err(format!("{READER_UNCONFIRMED}; stop receipt withheld"));
            }
            if status.code() != Some(86) {
                return Err(
                    "partial payload child did not report the fixed injected write failure".into(),
                );
            }
            let receipt = StopReceipt::confirmed(store.token(), pid);
            store
                .record_stopped(&receipt)
                .map_err(|error| format!("record interrupted worker stop: {error}"))?;
            let partial = store
                .inspect_stopped_worker_partial_payload(&receipt)
                .map_err(|_| "retained partial payload validation refused".to_owned())?;
            if partial.written_bytes != store::PARTIAL_PAYLOAD_FIXTURE_BYTES
                || store.inspect_stopped_worker_payload(&receipt).is_ok()
            {
                return Err(
                    "partial payload observation or complete-payload refusal mismatch".into(),
                );
            }
            let latched = registry_mut!()
                .require_reconciliation(FIXTURE_ID, RState::Registered)
                .map_err(|error| format!("latch interrupted registry attempt: {error}"))?;
            if latched.state != RState::NeedsReconciliation
                || latched.prior_state != Some(RState::Registered)
                || latched.reserved_bytes != store::WORKER_PAYLOAD_RESERVATION
                || (
                    latched.parent_dev,
                    latched.parent_ino,
                    latched.root_dev,
                    latched.root_ino,
                ) != (parent.0, parent.1, root.0, root.1)
            {
                return Err(
                    "live reconciliation latch changed store identity or reservation".into(),
                );
            }
            match registry_mut!().reserve(
                FIXTURE_ID,
                store::WORKER_PAYLOAD_RESERVATION,
                store::WORKER_PAYLOAD_RESERVATION * 2,
            ) {
                Err(RegistryError::ReconciliationRequired) => {}
                Err(other) => {
                    return Err(format!(
                        "partial retry refused for unexpected reason: {other}"
                    ))
                }
                Ok(_) => return Err("partial retry unexpectedly admitted more storage".into()),
            }
            match registry_mut!().transition(
                FIXTURE_ID,
                RState::NeedsReconciliation,
                RState::Active,
            ) {
                Err(RegistryError::InvalidTransition) => {}
                Err(other) => {
                    return Err(format!(
                        "latched attempt transition refused unexpectedly: {other}"
                    ))
                }
                Ok(_) => return Err("incomplete payload unexpectedly became active".into()),
            }
            let footprint = store
                .observe_stopped_partial_fixture_footprint(registry_ref!(), &receipt)
                .map_err(|_| "partial stopped fixture footprint observation refused".to_owned())?;
            if footprint.object_count != 6 || footprint.observed_bytes < partial.high_water_bytes {
                return Err("partial fixture snapshot count or payload coverage mismatch".into());
            }
            let free_after = store
                .sample_free_space_bytes()
                .map_err(|_| "sample stopped partial fixture free space refused".to_owned())?;
            let accounted = settle_parent_store_budget(
                &mut admission_budget,
                admission.job_id,
                footprint.observed_bytes,
                free_after,
                elapsed_millis(attempt_started),
            )
            .map_err(|_| "parent partial-fixture accounting refused".to_owned())?;
            if accounted != footprint.observed_bytes
                || footprint.observed_bytes > store::WORKER_PAYLOAD_RESERVATION
                || admission_budget
                    .snapshot(admission.job_id)
                    .map(|job| {
                        job.usage.stored_bytes != footprint.observed_bytes
                            || job.state
                                != (JobState::Failed {
                                    reason: verification_budget::BlockReason::StoreIncomplete,
                                    worker_stopped: true,
                                })
                    })
                    .unwrap_or(true)
            {
                return Err("partial fixture accounting or reservation mismatch".into());
            }
            let live_state = store
                .inspect_state()
                .map_err(|error| format!("inspect interrupted store: {error}"))?;
            drop(slot.take());
            drop(store);
            let mut reopened = VerificationStoreRegistry::open(&registry_path)
                .map_err(|error| format!("reopen interrupted registry: {error}"))?;
            let record = reopened
                .get(FIXTURE_ID)
                .map_err(|error| format!("read interrupted registry: {error}"))?
                .ok_or("interrupted registry row disappeared")?;
            if record.reserved_bytes != store::WORKER_PAYLOAD_RESERVATION
                || record.state != RState::NeedsReconciliation
                || record.prior_state != Some(RState::Registered)
                || (
                    record.parent_dev,
                    record.parent_ino,
                    record.root_dev,
                    record.root_ino,
                ) != (parent.0, parent.1, root.0, root.1)
            {
                return Err("interrupted registry identity/state mismatch".into());
            }
            match reopened.reserve(
                FIXTURE_ID,
                store::WORKER_PAYLOAD_RESERVATION,
                store::WORKER_PAYLOAD_RESERVATION * 2,
            ) {
                Err(RegistryError::ReconciliationRequired) => {}
                Err(other) => {
                    return Err(format!(
                        "partial retry refused for unexpected reason: {other}"
                    ))
                }
                Ok(_) => return Err("partial retry unexpectedly admitted more storage".into()),
            }
            match reopened.transition(FIXTURE_ID, RState::NeedsReconciliation, RState::Active) {
                Err(RegistryError::InvalidTransition) => {}
                Err(other) => {
                    return Err(format!(
                        "partial retry lifecycle refused unexpectedly: {other}"
                    ))
                }
                Ok(_) => return Err("incomplete payload unexpectedly became active".into()),
            }
            println!("worker_joined=true coordinated_admission=true store_stopped=true partial_payload_validated=true partial_payload_written={} partial_payload_high_water_bytes={} parent_observed_accounted_bytes={accounted} parent_accounting_scope=fixed_six_objects parent_elapsed_checked=true parent_budget_incomplete=true parent_worker_stopped=true complete_payload_refused=true reconciliation_latched=true reopened_reconciliation=true prior_registered=true reserved_bytes={} reservation_retry_refused=true fixture_objects={} fixture_observed_bytes={} fixture_snapshot=before_registry_reopen store_state={live_state:?} child_exit={status} cleanup=not_authorized", partial.written_bytes, partial.high_water_bytes, store::WORKER_PAYLOAD_RESERVATION, footprint.object_count, footprint.observed_bytes);
            return Ok(());
        }
        if let Err(e) = await_readiness(&rx, Instant::now() + OUTER_DEADLINE) {
            return finish_after_error(child, reader, rx, e);
        }
        if let Err(e) = registry_mut!().transition(FIXTURE_ID, RState::Registered, RState::Active) {
            return finish_after_error(child, reader, rx, format!("activate store: {e}"));
        }
        if let Err(e) = stop_and_reap(&mut child) {
            let reader_done = bounded_reader_join(reader);
            return Err(format!("child shutdown unconfirmed: {e}; reader completion confirmed={reader_done} (false means its handle may be detached)"));
        }
        if !bounded_reader_join(reader) {
            return Err(format!("{READER_UNCONFIRMED}; stop receipt withheld"));
        }
        let status = child
            .try_wait()
            .map_err(|e| format!("confirm child exit: {e}"))?
            .ok_or("child exit missing")?;
        let (bytes, failed) = drain_reader_result(&rx)?;
        if failed || bytes != 1 {
            return Err("unexpected worker output; stop receipt withheld".into());
        }
        let receipt = StopReceipt::confirmed(store.token(), pid);
        store
            .record_stopped(&receipt)
            .map_err(|e| format!("record stopped: {e}"))?;
        registry_mut!()
            .transition(FIXTURE_ID, RState::Active, RState::Stopped)
            .map_err(|e| format!("record registry stop: {e}"))?;
        let (_, live_state) = latch_stopped_registry_attempt(
            registry_mut!(),
            FIXTURE_ID,
            store::WORKER_PAYLOAD_RESERVATION,
            parent,
            root,
            || {
                store
                    .inspect_state()
                    .map_err(|_| "inspect stopped store refused".to_owned())
            },
        )?;
        let payload_observation = if mode == IndexedWorkerMode::Payload {
            let observation = store
                .inspect_stopped_worker_payload(&receipt)
                .map_err(|_| "stopped worker payload validation refused".to_owned())?;
            if observation.written_bytes != 32 * 1024 {
                return Err("worker payload exceeds fixed admission".into());
            }
            Some(observation)
        } else {
            None
        };
        let fixture_footprint = match mode {
            IndexedWorkerMode::Payload => Some(
                store
                    .observe_stopped_fixture_footprint(registry_ref!(), &receipt)
                    .map_err(|_| {
                        "complete stopped fixture footprint observation refused".to_owned()
                    })?,
            ),
            IndexedWorkerMode::PartialPayload => Some(
                store
                    .observe_stopped_partial_fixture_footprint(registry_ref!(), &receipt)
                    .map_err(|_| {
                        "partial stopped fixture footprint observation refused".to_owned()
                    })?,
            ),
            IndexedWorkerMode::Stall | IndexedWorkerMode::DescriptorOnly => Some(
                store
                    .observe_stopped_control_fixture_footprint(registry_ref!(), &receipt)
                    .map_err(|_| {
                        "stopped control fixture footprint observation refused".to_owned()
                    })?,
            ),
        };
        let parent_observed_accounted_bytes = if let Some(footprint) = fixture_footprint {
            let free_after = store
                .sample_free_space_bytes()
                .map_err(|_| "sample stopped fixture free space refused".to_owned())?;
            let expected_objects = if payload_observation.is_some() { 6 } else { 5 };
            if footprint.object_count != expected_objects {
                return Err("fixed fixture footprint object count mismatch".into());
            }
            if payload_observation
                .map(|payload| footprint.observed_bytes < payload.high_water_bytes)
                .unwrap_or(false)
            {
                return Err("fixed fixture observation is below payload high-water".into());
            }
            let accounted = settle_parent_store_budget(
                &mut admission_budget,
                admission.job_id,
                footprint.observed_bytes,
                free_after,
                elapsed_millis(attempt_started),
            )
            .map_err(|_| "parent fixture accounting refused".to_owned())?;
            if accounted != footprint.observed_bytes
                || footprint.observed_bytes > store::WORKER_PAYLOAD_RESERVATION
                || admission_budget
                    .snapshot(admission.job_id)
                    .map(|job| {
                        job.usage.stored_bytes != footprint.observed_bytes
                            || job.state
                                != (JobState::Failed {
                                    reason: verification_budget::BlockReason::StoreIncomplete,
                                    worker_stopped: true,
                                })
                    })
                    .unwrap_or(true)
            {
                return Err("fixture accounting or reservation mismatch".into());
            }
            Some(accounted)
        } else {
            None
        };
        drop(slot.take());
        drop(store);
        let mut reopened = VerificationStoreRegistry::open(&registry_path)
            .map_err(|e| format!("reopen private registry: {e}"))?;
        let record = reopened
            .get(FIXTURE_ID)
            .map_err(|e| format!("read reopened registry: {e}"))?
            .ok_or("registry row disappeared")?;
        if record.reserved_bytes != store::WORKER_PAYLOAD_RESERVATION
            || record.state != RState::NeedsReconciliation
            || record.prior_state != Some(RState::Stopped)
            || (
                record.parent_dev,
                record.parent_ino,
                record.root_dev,
                record.root_ino,
            ) != (parent.0, parent.1, root.0, root.1)
        {
            return Err("reopened registry identity/state mismatch".into());
        }
        match reopened.reserve(
            FIXTURE_ID,
            store::WORKER_PAYLOAD_RESERVATION,
            store::WORKER_PAYLOAD_RESERVATION * 2,
        ) {
            Err(RegistryError::ReconciliationRequired) => {}
            Err(other) => {
                return Err(format!(
                    "reservation refusal was not reconciliation: {other}"
                ))
            }
            Ok(_) => return Err("reopened registry unexpectedly admitted reservation".into()),
        }
        match reopened.transition(FIXTURE_ID, RState::NeedsReconciliation, RState::Active) {
            Err(RegistryError::InvalidTransition) => {}
            Err(other) => {
                return Err(format!(
                    "retry refusal was not the expected lifecycle gate: {other}"
                ))
            }
            Ok(_) => return Err("reopened reconciliation row unexpectedly became active".into()),
        }
        if let Some(observation) = payload_observation {
            let written = observation.written_bytes;
            let high_water = observation.high_water_bytes;
            let footprint =
                fixture_footprint.ok_or("complete payload footprint observation missing")?;
            let accounted =
                parent_observed_accounted_bytes.ok_or("parent fixture accounting missing")?;
            println!("worker_joined=true coordinated_admission=true registered_stopped=true reconciliation_latched=true reopened_reconciliation=true prior_stopped=true reserved_bytes={} reservation_retry_refused=true descriptor_handoff=true payload_validated=true payload_written={written} payload_high_water_bytes={high_water} parent_observed_accounted_bytes={accounted} parent_accounting_scope=fixed_six_objects parent_elapsed_checked=true parent_budget_incomplete=true parent_worker_stopped=true fixture_objects={} fixture_observed_bytes={} fixture_snapshot=before_registry_reopen store_state={live_state:?} child_exit={status} cleanup=not_authorized", store::WORKER_PAYLOAD_RESERVATION, footprint.object_count, footprint.observed_bytes);
        } else if let Some(footprint) = fixture_footprint {
            let accounted = parent_observed_accounted_bytes
                .ok_or("parent control fixture accounting missing")?;
            let handoff = if descriptor_handoff {
                " descriptor_handoff=true"
            } else {
                ""
            };
            println!("worker_joined=true coordinated_admission=true registered_stopped=true reconciliation_latched=true reopened_reconciliation=true prior_stopped=true reserved_bytes={} reservation_retry_refused=true{handoff} parent_observed_accounted_bytes={accounted} parent_accounting_scope=fixed_five_objects parent_elapsed_checked=true parent_budget_incomplete=true parent_worker_stopped=true fixture_objects={} fixture_observed_bytes={} fixture_snapshot=before_registry_reopen store_state={live_state:?} child_exit={status} cleanup=not_authorized", store::WORKER_PAYLOAD_RESERVATION, footprint.object_count, footprint.observed_bytes);
        } else {
            return Err("stopped fixture accounting observation missing".into());
        }
        Ok(())
    });
    finalize_admitted_attempt_result(&mut admission_budget, admission.job_id, attempt_result)
}

fn finalize_admitted_attempt_result<T>(
    budget: &mut verification_budget::BudgetController,
    job_id: verification_budget::JobId,
    result: Result<T, String>,
) -> Result<T, String> {
    use verification_budget::{BlockReason, JobState};

    let original = match result {
        Ok(value) => return Ok(value),
        Err(original) => original,
    };
    let Ok(snapshot) = budget.snapshot(job_id) else {
        return Err(format!(
            "{original}; could not establish StoreIncomplete worker_stopped=false: budget job unavailable"
        ));
    };
    if snapshot.id != job_id {
        return Err(format!(
            "{original}; could not establish StoreIncomplete worker_stopped=false: budget job identity mismatch"
        ));
    }
    let usage = snapshot.usage;
    match snapshot.state {
        JobState::Running => {
            if budget.fail(job_id, BlockReason::StoreIncomplete).is_err() {
                return Err(format!(
                    "{original}; could not establish StoreIncomplete worker_stopped=false"
                ));
            }
            match budget.snapshot(job_id) {
                Ok(after)
                    if after.id == job_id
                        && after.usage == usage
                        && after.state
                            == (JobState::Failed {
                                reason: BlockReason::StoreIncomplete,
                                worker_stopped: false,
                            }) =>
                {
                    Err(original)
                }
                _ => Err(format!(
                    "{original}; could not establish StoreIncomplete worker_stopped=false without changing budget usage"
                )),
            }
        }
        JobState::Failed { .. } => Err(original),
        JobState::Canceled {
            cleanup_confirmed: false,
            ..
        } => Err(original),
        JobState::Canceled {
            cleanup_confirmed: true,
            ..
        }
        | JobState::Completed { .. } => Err(format!(
            "{original}; could not establish StoreIncomplete worker_stopped=false: budget state does not permit incomplete transition"
        )),
    }
}

fn guard_indexed_attempt<T>(
    registry_slot: &mut Option<verification_store_registry::VerificationStoreRegistry>,
    expected: &verification_store_registry::StoreRecord,
    operation: impl FnOnce(
        &mut Option<verification_store_registry::VerificationStoreRegistry>,
    ) -> Result<T, String>,
) -> Result<T, String> {
    match operation(registry_slot) {
        Ok(value) => Ok(value),
        Err(original) => {
            let Some(registry) = registry_slot.as_mut() else {
                // Deliberate close points occur only after the successful live latch.
                return Err(original);
            };
            match latch_admitted_registry_row(registry, expected) {
                Ok(()) => Err(original),
                Err(()) => Err(format!(
                    "{original}; live registry reconciliation latch could not be established"
                )),
            }
        }
    }
}

fn latch_admitted_registry_row(
    registry: &mut verification_store_registry::VerificationStoreRegistry,
    expected: &verification_store_registry::StoreRecord,
) -> Result<(), ()> {
    use verification_store_registry::StoreState;

    let current = registry
        .get(&expected.store_id)
        .map_err(|_| ())?
        .ok_or(())?;
    if current.store_id != expected.store_id
        || current.parent_dev != expected.parent_dev
        || current.parent_ino != expected.parent_ino
        || current.root_dev != expected.root_dev
        || current.root_ino != expected.root_ino
        || current.reserved_bytes != expected.reserved_bytes
    {
        return Err(());
    }
    if current.state == StoreState::NeedsReconciliation {
        if !matches!(
            current.prior_state,
            Some(StoreState::Registered | StoreState::Active | StoreState::Stopped)
        ) {
            return Err(());
        }
        return Ok(());
    }
    if !matches!(
        current.state,
        StoreState::Registered | StoreState::Active | StoreState::Stopped
    ) || current.prior_state.is_some()
    {
        return Err(());
    }
    let latched = registry
        .require_reconciliation(&current.store_id, current.state)
        .map_err(|_| ())?;
    if latched.store_id != expected.store_id
        || latched.parent_dev != expected.parent_dev
        || latched.parent_ino != expected.parent_ino
        || latched.root_dev != expected.root_dev
        || latched.root_ino != expected.root_ino
        || latched.reserved_bytes != expected.reserved_bytes
        || latched.state != StoreState::NeedsReconciliation
        || latched.prior_state != Some(current.state)
    {
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod admitted_error_latch_tests {
    use super::guard_indexed_attempt;
    use super::store::OwnedStore;
    use verification_store_registry::{
        RegistryError, StoreRecord, StoreState, VerificationStoreRegistry,
    };

    const RESERVATION: u64 = 128 * 1024;

    fn fixture(target: StoreState) -> (OwnedStore, VerificationStoreRegistry, StoreRecord) {
        const ID: &str = "aabbccddeeff00112233445566778899";
        let store = OwnedStore::create().unwrap();
        let path = store.control_registry_path().unwrap();
        let (parent, root) = store.control_identity().unwrap();
        let mut registry = VerificationStoreRegistry::open(path).unwrap();
        registry
            .register(ID, parent.0, parent.1, root.0, root.1)
            .unwrap();
        let expected = registry.reserve(ID, 0, RESERVATION).unwrap();
        if matches!(target, StoreState::Active | StoreState::Stopped) {
            registry
                .transition(ID, StoreState::Registered, StoreState::Active)
                .unwrap();
        }
        if target == StoreState::Stopped {
            registry
                .transition(ID, StoreState::Active, StoreState::Stopped)
                .unwrap();
        }
        if target == StoreState::NeedsReconciliation {
            registry
                .require_reconciliation(ID, StoreState::Registered)
                .unwrap();
        }
        (store, registry, expected)
    }

    #[test]
    fn injected_errors_latch_actual_registered_active_and_stopped_states() {
        for state in [
            StoreState::Registered,
            StoreState::Active,
            StoreState::Stopped,
        ] {
            let (_store, registry, expected) = fixture(state);
            let mut slot = Some(registry);
            assert_eq!(
                guard_indexed_attempt(&mut slot, &expected, |_| {
                    Err::<(), _>("injected post-admission failure".into())
                }),
                Err("injected post-admission failure".into())
            );
            let registry = slot.as_ref().unwrap();
            let row = registry.get(&expected.store_id).unwrap().unwrap();
            assert_eq!(row.state, StoreState::NeedsReconciliation);
            assert_eq!(row.prior_state, Some(state));
            assert_eq!(row.reserved_bytes, RESERVATION);
            assert_eq!(
                (row.parent_dev, row.parent_ino, row.root_dev, row.root_ino),
                (
                    expected.parent_dev,
                    expected.parent_ino,
                    expected.root_dev,
                    expected.root_ino
                )
            );
            assert!(matches!(
                slot.as_mut()
                    .unwrap()
                    .reserve(&expected.store_id, RESERVATION, RESERVATION + 1),
                Err(RegistryError::ReconciliationRequired)
            ));
            assert!(matches!(
                slot.as_mut().unwrap().transition(
                    &expected.store_id,
                    StoreState::NeedsReconciliation,
                    StoreState::Active
                ),
                Err(RegistryError::InvalidTransition)
            ));
        }
    }

    #[test]
    fn already_latched_error_is_preserved_without_changing_prior_state() {
        let (_store, registry, expected) = fixture(StoreState::NeedsReconciliation);
        let mut slot = Some(registry);
        assert_eq!(
            guard_indexed_attempt(&mut slot, &expected, |_| {
                Err::<(), _>("injected later failure".into())
            }),
            Err("injected later failure".into())
        );
        let row = slot
            .as_ref()
            .unwrap()
            .get(&expected.store_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.state, StoreState::NeedsReconciliation);
        assert_eq!(row.prior_state, Some(StoreState::Registered));
        assert_eq!(row.reserved_bytes, RESERVATION);
    }

    #[test]
    fn missing_or_mismatched_admitted_row_refuses_latch_without_mutation() {
        for mismatch in ["missing", "identity", "reservation"] {
            let (_store, registry, admitted) = fixture(StoreState::Registered);
            let mut expected = admitted.clone();
            match mismatch {
                "missing" => expected.store_id = "00112233445566778899aabbccddeeff".into(),
                "identity" => expected.root_ino = expected.root_ino.saturating_add(1),
                "reservation" => expected.reserved_bytes += 1,
                _ => unreachable!(),
            }
            let mut slot = Some(registry);
            let error = guard_indexed_attempt(&mut slot, &expected, |_| {
                Err::<(), _>("original launch failure".into())
            })
            .unwrap_err();
            assert!(
                error.starts_with("original launch failure; live registry reconciliation latch")
            );
            let registry = slot.as_ref().unwrap();
            let row = registry.get(&admitted.store_id).unwrap().unwrap();
            assert_eq!(row.state, StoreState::Registered);
            assert_eq!(row.prior_state, None);
            assert_eq!(row.reserved_bytes, RESERVATION);
        }
    }

    #[test]
    fn cleanup_states_are_not_adopted_or_reset_by_error_finalization() {
        for complete in [false, true] {
            let (_store, mut registry, expected) = fixture(StoreState::Stopped);
            registry
                .transition(
                    &expected.store_id,
                    StoreState::Stopped,
                    StoreState::CleanupPending,
                )
                .unwrap();
            if complete {
                registry
                    .transition(
                        &expected.store_id,
                        StoreState::CleanupPending,
                        StoreState::CleanupComplete,
                    )
                    .unwrap();
            }
            let retained = registry.get(&expected.store_id).unwrap().unwrap();
            let mut slot = Some(registry);
            let error = guard_indexed_attempt(&mut slot, &expected, |_| {
                Err::<(), _>("original lifecycle failure".into())
            })
            .unwrap_err();
            assert!(
                error.starts_with("original lifecycle failure; live registry reconciliation latch")
            );
            assert_eq!(
                slot.as_ref()
                    .unwrap()
                    .get(&expected.store_id)
                    .unwrap()
                    .unwrap(),
                retained
            );
        }
    }

    #[test]
    fn no_slot_keeps_original_error_and_success_does_not_latch() {
        let (_store, registry, expected) = fixture(StoreState::Registered);
        let mut closed = None;
        assert_eq!(
            guard_indexed_attempt(&mut closed, &expected, |_| {
                Err::<(), _>("original closed-lease error".into())
            }),
            Err("original closed-lease error".into())
        );

        let mut slot = Some(registry);
        assert_eq!(
            guard_indexed_attempt(&mut slot, &expected, |_| Ok::<_, String>(7)),
            Ok(7)
        );
        let row = slot
            .as_ref()
            .unwrap()
            .get(&expected.store_id)
            .unwrap()
            .unwrap();
        assert_eq!(row.state, StoreState::Registered);
        assert_eq!(row.prior_state, None);
    }
}

/// Charge the caller's validated stopped-worker byte observation to the parent
/// attempt. The elapsed time must come from the same monotonic clock started
/// immediately before coordinated admission. This grants no cleanup or
/// completion authority.
fn settle_parent_store_budget(
    budget: &mut verification_budget::BudgetController,
    job_id: verification_budget::JobId,
    observed_bytes: u64,
    free_bytes_after: u64,
    elapsed_millis: u64,
) -> Result<u64, verification_budget::BudgetError> {
    use verification_budget::{BlockReason, BudgetError, JobState};

    let snapshot = budget.snapshot(job_id)?;
    if snapshot.state != JobState::Running
        || snapshot.usage.stored_bytes != 0
        || observed_bytes == 0
    {
        return Err(BudgetError::Blocked(BlockReason::JobNotRunning));
    }

    if let Err(source) = budget.record_store(job_id, observed_bytes, free_bytes_after) {
        if matches!(
            budget.snapshot(job_id).map(|job| job.state),
            Ok(JobState::Failed {
                worker_stopped: false,
                ..
            })
        ) {
            budget.acknowledge_worker_stopped(job_id)?;
        }
        return Err(source);
    }

    if let Err(source) = budget.check_elapsed(job_id, elapsed_millis) {
        if matches!(
            budget.snapshot(job_id).map(|job| job.state),
            Ok(JobState::Failed {
                worker_stopped: false,
                ..
            })
        ) {
            budget.acknowledge_worker_stopped(job_id)?;
        }
        return Err(source);
    }
    budget.fail(job_id, BlockReason::StoreIncomplete)?;
    budget.acknowledge_worker_stopped(job_id)?;
    Ok(observed_bytes)
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

// The caller must already hold exact child shutdown evidence. Keep this private:
// a registry row alone cannot prove that an operating-system process stopped.
fn latch_stopped_registry_attempt<T>(
    registry: &mut verification_store_registry::VerificationStoreRegistry,
    id: &str,
    reservation: u64,
    parent: (u64, u64),
    root: (u64, u64),
    observe: impl FnOnce() -> Result<T, String>,
) -> Result<(verification_store_registry::StoreRecord, T), String> {
    use verification_store_registry::{RegistryError, StoreState};

    let record = registry
        .require_reconciliation(id, StoreState::Stopped)
        .map_err(|error| format!("latch stopped registry attempt: {error}"))?;
    if record.state != StoreState::NeedsReconciliation
        || record.prior_state != Some(StoreState::Stopped)
        || record.reserved_bytes != reservation
        || (
            record.parent_dev,
            record.parent_ino,
            record.root_dev,
            record.root_ino,
        ) != (parent.0, parent.1, root.0, root.1)
    {
        return Err("stopped registry reconciliation latch mismatch".into());
    }
    let proposed = reservation
        .checked_add(1)
        .ok_or("stopped reservation growth probe overflow")?;
    match registry.reserve(id, reservation, proposed) {
        Err(RegistryError::ReconciliationRequired) => {}
        Err(error) => {
            return Err(format!(
                "latched reservation retry refused unexpectedly: {error}"
            ))
        }
        Ok(_) => return Err("latched registry unexpectedly admitted storage growth".into()),
    }
    match registry.transition(id, StoreState::NeedsReconciliation, StoreState::Active) {
        Err(RegistryError::InvalidTransition) => {}
        Err(error) => {
            return Err(format!(
                "latched lifecycle retry refused unexpectedly: {error}"
            ))
        }
        Ok(_) => return Err("latched registry unexpectedly became active".into()),
    }
    let observed = observe()?;
    Ok((record, observed))
}

fn run_store_worker() -> Result<(), String> {
    run_store_worker_with_cleanup(false)
}

fn run_store_worker_with_cleanup(cleanup: bool) -> Result<(), String> {
    let mut store = store::OwnedStore::create().map_err(|e| format!("create owned store: {e}"))?;
    let executable = env::current_exe().map_err(|e| format!("locate own executable: {e}"))?;
    let mut child = Command::new(executable)
        .env_clear()
        .arg("--child")
        .arg("stall")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start fixed child: {e}"))?;
    let pid = child.id();
    if let Err(error) = store.bind_worker(pid) {
        return match stop_and_reap(&mut child) {
            Ok(()) => Err(format!("bind worker: {error}")),
            Err(stop) => Err(format!(
                "bind worker: {error}; child cleanup failed: {stop}"
            )),
        };
    }
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err("child stdout pipe unavailable".into()),
                Err(error) => Err(format!(
                    "child stdout pipe unavailable; cleanup failed: {error}"
                )),
            }
        }
    };
    let (sender, receiver) = mpsc::sync_channel(2);
    let reader = match thread::Builder::new()
        .name("store-worker-stdout".into())
        .spawn(move || read_bounded(stdout, sender))
    {
        Ok(reader) => reader,
        Err(error) => {
            return match stop_and_reap(&mut child) {
                Ok(()) => Err(format!("start bounded stdout reader: {error}")),
                Err(stop) => Err(format!(
                    "start bounded stdout reader: {error}; cleanup failed: {stop}"
                )),
            }
        }
    };
    let deadline = Instant::now() + OUTER_DEADLINE;
    if let Err(error) = await_readiness(&receiver, deadline) {
        return finish_after_error(child, reader, receiver, error);
    }
    if let Err(error) = stop_and_reap(&mut child) {
        let reader_done = bounded_reader_join(reader);
        return Err(format!("child shutdown unconfirmed: {error}; reader completion confirmed={reader_done} (false means its handle may be detached)"));
    }
    let joined = bounded_reader_join(reader);
    if !joined {
        return Err(format!("{READER_UNCONFIRMED}; shutdown receipt withheld"));
    }
    let status = child
        .try_wait()
        .map_err(|e| format!("confirm child exit: {e}"))?
        .ok_or_else(|| "child exit was not observable after wait".to_string())?;
    let (bytes, failed) = drain_reader_result(&receiver)?;
    if failed || bytes != 1 {
        return Err("unexpected store-worker reader result; shutdown receipt withheld".into());
    }
    let receipt = StopReceipt::confirmed(store.token(), pid);
    store
        .record_stopped(&receipt)
        .map_err(|e| format!("record stopped transition: {e}"))?;
    if cleanup {
        store
            .cleanup_after_stopped(&receipt)
            .map_err(|e| format!("cleanup stopped store: {e}"))?;
        println!("store_removed=true retained_parent_journal=cleanup.journal worker_pid={pid}");
        return Ok(());
    }
    let state = store
        .inspect_state()
        .map_err(|e| format!("inspect stopped store: {e}"))?;
    println!(
        "store_state={state:?} token={} worker_pid={} child_exit={} reader_joined=true bytes={} cleanup=not_authorized",
        store.token(),
        pid,
        status,
        bytes
    );
    Ok(())
}

fn run_child(args: &[String]) -> Result<(), String> {
    // Fixed probe check for the closed launch boundary; this is not process isolation.
    if env::var_os("DISK_STEWARD_WORKER_ENV_SENTINEL").is_some() {
        return Err("child environment sentinel present".into());
    }
    match args {
        [mode] if mode == "flood" => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(b"R").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
            let chunk = [b'F'; READ_CHUNK];
            let mut remaining = FLOOD_BYTES;
            while remaining > 0 {
                let count = remaining.min(chunk.len());
                stdout
                    .write_all(&chunk[..count])
                    .map_err(|e| e.to_string())?;
                remaining -= count;
            }
            stdout.flush().map_err(|e| e.to_string())?;
            thread::sleep(CHILD_HOLD);
            Ok(())
        }
        [mode] if mode == "stall" => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(b"R").map_err(|e| e.to_string())?;
            stdout.flush().map_err(|e| e.to_string())?;
            thread::sleep(CHILD_HOLD);
            Ok(())
        }
        [mode] if mode == "store-handoff" => {
            store::OwnedStore::validate_worker_stdin_handoff()
                .map_err(|_| "store handoff refused".to_owned())?;
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(b"R")
                .map_err(|_| "child readiness write failed")?;
            stdout.flush().map_err(|_| "child readiness flush failed")?;
            thread::sleep(CHILD_HOLD);
            Ok(())
        }
        [mode] if mode == "store-payload" => {
            let report = store::write_worker_stdin_payload()
                .map_err(|_| "worker payload incomplete; retain store".to_owned())?;
            if report.written_bytes != 32 * 1024 || !report.cap_reached {
                return Err("worker payload limit not established".into());
            }
            let mut stdout = io::stdout().lock();
            stdout
                .write_all(b"R")
                .map_err(|_| "child readiness write failed")?;
            stdout.flush().map_err(|_| "child readiness flush failed")?;
            thread::sleep(CHILD_HOLD);
            Ok(())
        }
        [mode] if mode == "store-payload-partial" => {
            if store::write_worker_stdin_partial_payload().is_err() {
                // Fixed synthetic failure exit after the shared writer charged
                // the observed partial allocation. Never signal readiness.
                std::process::exit(86);
            }
            Err("fixed partial payload probe unexpectedly completed".into())
        }
        [mode] if mode == "store-abandon" => run_store_abandon_child(),
        _ => Err("invalid private child mode".into()),
    }
}

fn run_store_abandon_child() -> Result<(), String> {
    let mut store = store::OwnedStore::create().map_err(|e| e.to_string())?;
    let pid = std::process::id();
    store.bind_worker(pid).map_err(|e| e.to_string())?;
    let record = store.abandon_record(pid).map_err(|e| e.to_string())?;
    let mut out = io::stdout().lock();
    out.write_all(record.as_bytes())
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    std::process::exit(86)
}

fn run_store_reinspect() -> Result<(), String> {
    let start = Instant::now();
    let deadline = start + OUTER_DEADLINE;
    let exe = env::current_exe().map_err(|e| e.to_string())?;
    let mut child = Command::new(exe)
        .env_clear()
        .arg("--child")
        .arg("store-abandon")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = child.id();
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let stopped = stop_and_reap(&mut child);
            return Err(format!("child stdout unavailable; stop/reap={stopped:?}"));
        }
    };
    let (tx, rx) = mpsc::sync_channel(1);
    let reader = thread::Builder::new()
        .name("abandoned-store-record".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(1025).read_to_end(&mut bytes);
            let _ = tx.send((bytes, result.is_err()));
        });
    let reader = match reader {
        Ok(reader) => reader,
        Err(e) => {
            let stopped = stop_and_reap(&mut child);
            return Err(format!("start record reader: {e}; stop/reap={stopped:?}"));
        }
    };
    let record = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()));
    let capture_expired = Instant::now() >= deadline;
    let stopped = stop_and_reap(&mut child);
    let joined = bounded_reader_join(reader);
    if stopped.is_err() || !joined {
        return Err(format!(
            "creator shutdown unconfirmed: {stopped:?}; reader_joined={joined} (false means completion/join unconfirmed and handle may be detached)"
        ));
    }
    let waited = child
        .try_wait()
        .map_err(|e| format!("confirm creator exit: {e}"))?
        .ok_or_else(|| "creator exit missing after wait".to_string())?;
    if capture_expired {
        return Err("creator record capture exceeded deadline; fixture retained".into());
    }
    let (bytes, failed) =
        record.map_err(|e| format!("creator record unavailable before deadline: {e}"))?;
    if failed || bytes.is_empty() || bytes.len() > 1024 {
        return Err("invalid or oversized child record".into());
    }
    let snapshot = parse_abandoned_record(&bytes)?;
    if snapshot.bound_pid != pid {
        return Err("child record PID mismatch".into());
    }
    // A complete validated record plus confirmed wait is sufficient even if the OS reports a signal.
    let state = store::OwnedStore::inspect_abandoned(&snapshot)
        .map_err(|e| format!("read-only store inspection refused: {e}"))?;
    println!("store_state={state:?} token={} creator_stopped=true cleanup=not_authorized child_exit={waited} reader_joined=true process_death_not_power_loss=true", snapshot.token);
    Ok(())
}

fn parse_abandoned_record(bytes: &[u8]) -> Result<store::AbandonedSnapshot, String> {
    if bytes.len() > 1024 {
        return Err("record exceeds limit".into());
    }
    let s = std::str::from_utf8(bytes).map_err(|_| "record is not ASCII")?;
    if !s.is_ascii() || !s.ends_with('\n') {
        return Err("malformed record".into());
    }
    let lines: Vec<&str> = s[..s.len() - 1].split('\n').collect();
    if lines.len() != 7 || lines[0] != "v1" {
        return Err("unsupported record shape or version".into());
    }
    let token = lines[1];
    if !store::valid_token(token) {
        return Err("invalid token".into());
    }
    fn number(s: &str) -> Result<u64, String> {
        if s.is_empty()
            || !s.bytes().all(|b| b.is_ascii_digit())
            || (s.len() > 1 && s.starts_with('0'))
        {
            return Err("noncanonical number".into());
        }
        s.parse().map_err(|_| "number overflow".into())
    }
    fn pair(s: &str) -> Result<(u64, u64), String> {
        let (a, b) = s.split_once(':').ok_or("bad identity")?;
        Ok((number(a)?, number(b)?))
    }
    let parent_id = pair(lines[2])?;
    let root_id = pair(lines[3])?;
    let journal_id = pair(lines[4])?;
    let bound_pid = u32::try_from(number(lines[5])?).map_err(|_| "bad PID")?;
    if bound_pid == 0 {
        return Err("bad PID".into());
    }
    // Extra line is not accepted; legacy shape reserves line 6 only if exact sentinel.
    if lines[6] != "end" {
        return Err("malformed record terminator".into());
    }
    Ok(store::AbandonedSnapshot {
        token: token.to_owned(),
        parent_id,
        root_id,
        journal_id,
        bound_pid,
        version: 1,
    })
}

fn supervise(case: Case) -> Result<Report, String> {
    let start = Instant::now();
    let deadline = start + OUTER_DEADLINE;
    let executable = env::current_exe().map_err(|e| format!("locate own executable: {e}"))?;
    let mut child = Command::new(executable)
        .env_clear()
        .arg("--child")
        .arg(case.child_mode())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("start fixed child: {e}"))?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let cleanup = stop_and_reap(&mut child);
            return match cleanup {
                Ok(()) => Err("child stdout pipe was not available".into()),
                Err(error) => Err(format!(
                    "child stdout pipe was not available; child cleanup failed: {error}"
                )),
            };
        }
    };
    let (sender, receiver) = mpsc::sync_channel(2);
    let reader = thread::Builder::new()
        .name(format!("worker-probe-stdout-{}", case.name()))
        .spawn(move || read_bounded(stdout, sender));
    let reader = match reader {
        Ok(reader) => reader,
        Err(error) => {
            let cleanup = stop_and_reap(&mut child);
            return match cleanup {
                Ok(()) => Err(format!("start bounded stdout reader: {error}")),
                Err(cleanup_error) => Err(format!(
                    "start bounded stdout reader: {error}; child cleanup failed: {cleanup_error}"
                )),
            };
        }
    };
    let supervision = if case == Case::Cancel {
        await_readiness(&receiver, deadline).map(|()| (StopReason::Canceled, None))
    } else {
        await_stop_reason(&mut child, &receiver, deadline)
    };
    let (stop_reason, completed_reader) = match supervision {
        Ok(result) => result,
        Err(error) => {
            return finish_after_error(child, reader, receiver, error);
        }
    };

    let mut cleanup_error = None;
    if let Err(error) = stop_and_reap(&mut child) {
        cleanup_error = Some(error);
    }
    let reader_joined = bounded_reader_join(reader);
    if !reader_joined {
        cleanup_error = Some(match cleanup_error {
            Some(error) => format!("{error}; {READER_UNCONFIRMED}"),
            None => READER_UNCONFIRMED.to_owned(),
        });
    }
    if let Some(error) = cleanup_error {
        return Err(format!("{error}; child cleanup was not fully confirmed"));
    }
    let (bytes_read, reader_failed) = completed_reader
        .or_else(|| drain_reader_result(&receiver).ok())
        .unwrap_or((OUTPUT_LIMIT.saturating_add(1), true));
    if reader_failed {
        return Err("stdout reader failed".into());
    }
    let child_exit = child
        .try_wait()
        .map_err(|e| format!("confirm child exit after wait: {e}"))?
        .ok_or_else(|| "child exit was not observable after wait".to_string())?;

    Ok(Report {
        case,
        stop_reason,
        bytes_read,
        elapsed: start.elapsed(),
        child_exit,
        reader_joined,
    })
}

fn await_readiness(receiver: &Receiver<ReaderEvent>, deadline: Instant) -> Result<(), String> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err("outer deadline expired before fixed child signaled readiness".into());
    }
    match receiver.recv_timeout(remaining) {
        Ok(ReaderEvent::Ready) => Ok(()),
        Ok(ReaderEvent::Finished { failed: true, .. }) => {
            Err("stdout reader failed before fixed child signaled readiness".into())
        }
        Ok(ReaderEvent::Finished { .. }) => {
            Err("stdout closed before fixed child signaled readiness".into())
        }
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err("outer deadline expired before fixed child signaled readiness".into())
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("stdout reader stopped without a result".into())
        }
    }
}

fn await_stop_reason(
    child: &mut Child,
    receiver: &Receiver<ReaderEvent>,
    deadline: Instant,
) -> Result<(StopReason, Option<(u64, bool)>), String> {
    let mut saw_ready = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return if saw_ready {
                Ok((StopReason::Deadline, None))
            } else {
                Err("outer deadline expired before fixed child signaled readiness".into())
            };
        }
        match receiver.recv_timeout(remaining) {
            Ok(ReaderEvent::Ready) => saw_ready = true,
            Ok(ReaderEvent::Finished {
                bytes: observed,
                failed,
            }) => {
                if failed {
                    return Ok((StopReason::ReaderError, Some((observed, failed))));
                }
                if observed > OUTPUT_LIMIT {
                    return Ok((StopReason::OutputLimit, Some((observed, failed))));
                }
                return if child
                    .try_wait()
                    .map_err(|e| format!("inspect child status: {e}"))?
                    .is_some()
                {
                    Ok((StopReason::ChildExited, Some((observed, failed))))
                } else {
                    Err("stdout closed while fixed child remained running".into())
                };
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                return if saw_ready {
                    Ok((StopReason::Deadline, None))
                } else {
                    Err("outer deadline expired before fixed child signaled readiness".into())
                };
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("stdout reader stopped without a result".into());
            }
        }
    }
}

fn read_bounded<R: Read>(mut reader: R, sender: SyncSender<ReaderEvent>) {
    let mut bytes = 0_u64;
    let mut signaled_ready = false;
    let mut failed = false;
    let mut interruptions = 0_u8;
    let mut buffer = [0_u8; READ_CHUNK];
    while bytes <= OUTPUT_LIMIT {
        let remaining = OUTPUT_LIMIT.saturating_add(1).saturating_sub(bytes) as usize;
        let wanted = remaining.min(buffer.len());
        match reader.read(&mut buffer[..wanted]) {
            Ok(0) => break,
            Ok(count) => {
                bytes = bytes.saturating_add(count as u64);
                if !signaled_ready {
                    if buffer[0] != b'R' {
                        failed = true;
                        break;
                    }
                    if sender.send(ReaderEvent::Ready).is_err() {
                        return;
                    }
                    signaled_ready = true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                interruptions = interruptions.saturating_add(1);
                if interruptions > 16 {
                    failed = true;
                    break;
                }
            }
            Err(_) => {
                failed = true;
                break;
            }
        }
    }
    let _ = sender.send(ReaderEvent::Finished { bytes, failed });
}

fn drain_reader_result(receiver: &Receiver<ReaderEvent>) -> Result<(u64, bool), String> {
    receiver
        .try_iter()
        .find_map(|event| match event {
            ReaderEvent::Finished { bytes, failed } => Some((bytes, failed)),
            ReaderEvent::Ready => None,
        })
        .ok_or_else(|| "stdout reader exited without a result".into())
}

fn stop_and_reap(child: &mut Child) -> Result<(), String> {
    stop_policy(
        child,
        |child| {
            child
                .try_wait()
                .map(|status| status.is_some())
                .map_err(|error| format!("query child status: {error}"))
        },
        |child| {
            child
                .kill()
                .map_err(|error| format!("kill fixed child: {error}"))
        },
        |child, deadline| {
            wait_for_child_exit(child, deadline)
                .map(|_| ())
                .map_err(|error| format!("confirm fixed child exit: {error}"))
        },
    )
}

fn stop_policy<T, Q, K, C>(target: &mut T, query: Q, kill: K, confirm: C) -> Result<(), String>
where
    Q: FnOnce(&mut T) -> Result<bool, String>,
    K: FnOnce(&mut T) -> Result<(), String>,
    C: FnOnce(&mut T, Instant) -> Result<(), String>,
{
    let query_error = match query(target) {
        Ok(true) => return Ok(()),
        Ok(false) => None,
        Err(error) => Some(error),
    };

    // Always attempt the fixed-target kill after a running or uncertain query.
    // A kill failure can race natural exit, so confirmation remains authoritative.
    let kill_error = kill(target).err();
    let deadline = Instant::now() + OUTER_DEADLINE;
    match confirm(target, deadline) {
        Ok(()) => Ok(()),
        Err(confirm_error) => {
            let mut diagnostics = Vec::new();
            if let Some(error) = query_error {
                diagnostics.push(error);
            }
            match kill_error {
                Some(error) => diagnostics.push(error),
                None => diagnostics.push("kill fixed child succeeded".to_owned()),
            }
            diagnostics.push(confirm_error);
            Err(diagnostics.join("; "))
        }
    }
}

fn bounded_reader_join(reader: JoinHandle<()>) -> bool {
    const MAX_POLLS: usize = 512;
    bounded_reader_join_until(reader, Instant::now() + OUTER_DEADLINE, MAX_POLLS)
}

fn bounded_reader_join_until(reader: JoinHandle<()>, deadline: Instant, max_polls: usize) -> bool {
    const POLL_INTERVAL: Duration = Duration::from_millis(2);
    for _ in 0..max_polls {
        if Instant::now() >= deadline {
            break;
        }
        if reader.is_finished() {
            if Instant::now() >= deadline {
                break;
            }
            return reader.join().is_ok();
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(POLL_INTERVAL));
    }
    false
}

fn wait_for_child_exit(child: &mut Child, deadline: Instant) -> Result<ExitStatus, String> {
    const MAX_POLLS: usize = 512;
    const POLL_INTERVAL: Duration = Duration::from_millis(2);
    for _ in 0..MAX_POLLS {
        if Instant::now() >= deadline {
            break;
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("poll fixed child exit: {error}"))?
        {
            if Instant::now() >= deadline {
                break;
            }
            return Ok(status);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(POLL_INTERVAL));
    }
    Err("fixed child exit was not observed before deadline".into())
}

fn finish_after_error<T>(
    mut child: Child,
    reader: JoinHandle<()>,
    receiver: Receiver<ReaderEvent>,
    error: String,
) -> Result<T, String> {
    let cleanup = stop_and_reap(&mut child);
    let _ = receiver.try_iter().count();
    let joined = bounded_reader_join(reader);
    match (cleanup, joined) {
        (Ok(()), true) => Err(error),
        (Err(cleanup_error), true) => Err(format!("{error}; cleanup failed: {cleanup_error}")),
        (Ok(()), false) => Err(format!("{error}; reader completion/join unconfirmed (reader handle may be detached); stop receipt withheld")),
        (Err(cleanup_error), false) => Err(format!(
            "{error}; cleanup failed: {cleanup_error}; reader completion/join unconfirmed (reader handle may be detached); stop receipt withheld"
        )),
    }
}

#[cfg(test)]
mod bounded_reader_tests {
    use super::{await_readiness, read_bounded, ReaderEvent, OUTPUT_LIMIT};
    use std::collections::VecDeque;
    use std::io::{self, Read};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[derive(Clone)]
    enum Action {
        Bytes(Vec<u8>),
        Interrupted,
        Eof,
    }

    struct ScriptedReader(VecDeque<Action>);

    impl ScriptedReader {
        fn new(actions: impl IntoIterator<Item = Action>) -> Self {
            Self(actions.into_iter().collect())
        }
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.0.pop_front().unwrap_or(Action::Eof) {
                Action::Bytes(mut bytes) => {
                    let count = bytes.len().min(buffer.len());
                    buffer[..count].copy_from_slice(&bytes[..count]);
                    if count < bytes.len() {
                        bytes.drain(..count);
                        self.0.push_front(Action::Bytes(bytes));
                    }
                    Ok(count)
                }
                Action::Interrupted => Err(io::Error::from(io::ErrorKind::Interrupted)),
                Action::Eof => Ok(0),
            }
        }
    }

    fn collect(actions: impl IntoIterator<Item = Action>) -> Vec<ReaderEvent> {
        let (sender, receiver) = mpsc::sync_channel(2);
        read_bounded(ScriptedReader::new(actions), sender);
        receiver.try_iter().collect()
    }

    #[test]
    fn readiness_requires_exact_first_r_and_counts_rejected_bytes() {
        let events = collect([Action::Bytes(vec![b'X', b'R']), Action::Eof]);
        assert_eq!(
            events,
            [ReaderEvent::Finished {
                bytes: 2,
                failed: true
            }]
        );
        let (sender, receiver) = mpsc::sync_channel(2);
        read_bounded(
            ScriptedReader::new([Action::Bytes(vec![b'X']), Action::Eof]),
            sender,
        );
        assert!(await_readiness(&receiver, Instant::now() + Duration::from_secs(1)).is_err());
    }

    #[test]
    fn readiness_accepts_empty_eof_only_as_completion_and_exact_r() {
        assert_eq!(
            collect([Action::Eof]),
            [ReaderEvent::Finished {
                bytes: 0,
                failed: false
            }]
        );
        assert_eq!(
            collect([Action::Bytes(vec![b'R']), Action::Eof]),
            [
                ReaderEvent::Ready,
                ReaderEvent::Finished {
                    bytes: 1,
                    failed: false
                }
            ]
        );
    }

    #[test]
    fn readiness_accepts_split_r_then_data_and_counts_all_bytes() {
        assert_eq!(
            collect([
                Action::Bytes(vec![b'R']),
                Action::Bytes(b"data".to_vec()),
                Action::Eof,
            ]),
            [
                ReaderEvent::Ready,
                ReaderEvent::Finished {
                    bytes: 5,
                    failed: false
                }
            ]
        );
    }

    #[test]
    fn interrupted_reads_have_a_cumulative_sixteen_retry_bound() {
        let mut permitted = vec![Action::Interrupted; 16];
        permitted.extend([Action::Bytes(vec![b'R']), Action::Eof]);
        assert_eq!(
            collect(permitted),
            [
                ReaderEvent::Ready,
                ReaderEvent::Finished {
                    bytes: 1,
                    failed: false
                }
            ]
        );

        let mut refused = vec![Action::Interrupted; 17];
        refused.extend([Action::Bytes(vec![b'R']), Action::Eof]);
        assert_eq!(
            collect(refused),
            [ReaderEvent::Finished {
                bytes: 0,
                failed: true
            }]
        );
    }

    #[test]
    fn interruptions_after_readiness_remain_cumulative_and_output_cap_is_exact() {
        let mut actions = vec![Action::Interrupted; 8];
        actions.push(Action::Bytes(vec![b'R', b'x']));
        actions.extend(vec![Action::Interrupted; 9]);
        assert_eq!(
            collect(actions),
            [
                ReaderEvent::Ready,
                ReaderEvent::Finished {
                    bytes: 2,
                    failed: true
                }
            ]
        );

        let mut allowed = vec![Action::Interrupted; 16];
        allowed.push(Action::Bytes(vec![b'R'; OUTPUT_LIMIT as usize + 1]));
        assert_eq!(
            collect(allowed),
            [
                ReaderEvent::Ready,
                ReaderEvent::Finished {
                    bytes: OUTPUT_LIMIT + 1,
                    failed: false
                }
            ]
        );
    }
}

#[cfg(test)]
mod store_worker_tests {
    use super::{latch_stopped_registry_attempt, store::OwnedStore, StopReceipt};

    #[test]
    fn store_binding_is_single_store_and_exact_pid() {
        let mut first = OwnedStore::create().unwrap();
        let mut second = OwnedStore::create().unwrap();
        assert!(first
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .is_err());
        assert!(first.bind_worker(0).is_err());
        first.bind_worker(41).unwrap();
        second.bind_worker(41).unwrap();
        assert!(first.bind_worker(42).is_err());
        assert!(second
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .is_err());
        assert!(first
            .record_stopped(&StopReceipt::confirmed(first.token(), 42))
            .is_err());
        first
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .unwrap();
        assert!(first
            .record_stopped(&StopReceipt::confirmed(first.token(), 41))
            .is_err());
        second
            .record_stopped(&StopReceipt::confirmed(second.token(), 41))
            .unwrap();
    }

    #[test]
    fn mismatched_identity_and_replayed_latch_never_invoke_observation() {
        use verification_store_registry::{StoreState, VerificationStoreRegistry};
        const ID: &str = "fedcba9876543210fedcba9876543210";
        const RESERVED: u64 = 128 * 1024;
        let store = OwnedStore::create().unwrap();
        let (parent, root) = store.control_identity().unwrap();
        let mut registry =
            VerificationStoreRegistry::open(store.control_registry_path().unwrap()).unwrap();
        registry
            .register(ID, parent.0, parent.1, root.0, root.1)
            .unwrap();
        registry.reserve(ID, 0, RESERVED).unwrap();
        registry
            .transition(ID, StoreState::Registered, StoreState::Active)
            .unwrap();
        registry
            .transition(ID, StoreState::Active, StoreState::Stopped)
            .unwrap();
        let mut called = false;
        let wrong_root = (root.0, root.1 ^ 1);
        let error =
            latch_stopped_registry_attempt(&mut registry, ID, RESERVED, parent, wrong_root, || {
                called = true;
                Ok(())
            })
            .unwrap_err();
        assert_eq!(error, "stopped registry reconciliation latch mismatch");
        assert!(!called);
        let retained = registry.get(ID).unwrap().unwrap();
        assert_eq!(retained.state, StoreState::NeedsReconciliation);
        assert_eq!(retained.prior_state, Some(StoreState::Stopped));
        assert_eq!(retained.reserved_bytes, RESERVED);
        assert!(
            latch_stopped_registry_attempt(&mut registry, ID, RESERVED, parent, root, || {
                called = true;
                Ok(())
            })
            .is_err()
        );
        assert!(!called);
        assert_eq!(registry.get(ID).unwrap().unwrap(), retained);
    }

    #[test]
    fn failed_post_stop_observation_leaves_live_registry_latched() {
        use verification_store_registry::{RegistryError, StoreState, VerificationStoreRegistry};

        const ID: &str = "fedcba9876543210fedcba9876543210";
        const RESERVATION: u64 = 128 * 1024;
        let store = OwnedStore::create().unwrap();
        let registry_path = store.control_registry_path().unwrap();
        let (parent, root) = store.control_identity().unwrap();
        let mut registry = VerificationStoreRegistry::open(&registry_path).unwrap();
        registry
            .register(ID, parent.0, parent.1, root.0, root.1)
            .unwrap();

        let mut called = false;
        assert!(latch_stopped_registry_attempt(
            &mut registry,
            ID,
            RESERVATION,
            parent,
            root,
            || {
                called = true;
                Ok(())
            },
        )
        .is_err());
        assert!(!called, "post-stop observation ran before the latch");

        registry.reserve(ID, 0, RESERVATION).unwrap();
        registry
            .transition(ID, StoreState::Registered, StoreState::Active)
            .unwrap();
        registry
            .transition(ID, StoreState::Active, StoreState::Stopped)
            .unwrap();
        let error =
            latch_stopped_registry_attempt(&mut registry, ID, RESERVATION, parent, root, || {
                called = true;
                Err::<(), _>("injected stopped observation refusal".into())
            })
            .unwrap_err();
        assert_eq!(error, "injected stopped observation refusal");
        assert!(called, "post-stop observation seam was not reached");

        let record = registry.get(ID).unwrap().unwrap();
        assert_eq!(record.state, StoreState::NeedsReconciliation);
        assert_eq!(record.prior_state, Some(StoreState::Stopped));
        assert_eq!(record.reserved_bytes, RESERVATION);
        assert_eq!(
            (
                record.parent_dev,
                record.parent_ino,
                record.root_dev,
                record.root_ino
            ),
            (parent.0, parent.1, root.0, root.1)
        );
        assert!(matches!(
            registry.reserve(ID, RESERVATION, RESERVATION + 1),
            Err(RegistryError::ReconciliationRequired)
        ));
        assert!(matches!(
            registry.transition(ID, StoreState::NeedsReconciliation, StoreState::Active),
            Err(RegistryError::InvalidTransition)
        ));
    }
}

#[cfg(test)]
mod parent_store_settlement_tests {
    use super::settle_parent_store_budget;
    use verification_budget::{
        BlockReason, BudgetController, BudgetError, JobState, Limits, MINIMUM_FREE_RESERVE,
        PER_REPOSITORY_TEMP_LIMIT,
    };

    fn controller() -> BudgetController {
        BudgetController::new(Limits { headroom_bytes: 0 })
    }

    #[test]
    fn successful_charge_preserves_ingress_then_marks_incomplete_and_stopped_once() {
        let mut budget = controller();
        let job = budget.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_eq!(
            settle_parent_store_budget(&mut budget, job, 0, MINIMUM_FREE_RESERVE + 8192, 0,),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(budget.snapshot(job).unwrap().usage.stored_bytes, 0);
        budget.record_ingress(job, 29).unwrap();
        assert_eq!(
            settle_parent_store_budget(&mut budget, job, 4096, MINIMUM_FREE_RESERVE + 8192, 0,),
            Ok(4096)
        );
        let snapshot = budget.snapshot(job).unwrap();
        assert_eq!(snapshot.usage.ingress_bytes, 29);
        assert_eq!(snapshot.usage.stored_bytes, 4096);
        assert_eq!(
            snapshot.state,
            JobState::Failed {
                reason: BlockReason::StoreIncomplete,
                worker_stopped: true,
            }
        );
        assert_eq!(
            settle_parent_store_budget(&mut budget, job, 4096, MINIMUM_FREE_RESERVE + 8192, 0,),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(budget.snapshot(job).unwrap().usage.stored_bytes, 4096);
        assert_eq!(budget.aggregate_stored(), Ok(4096));
        let next = budget.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_ne!(next, job);
        assert_eq!(budget.aggregate_stored(), Ok(4096));
        assert_eq!(budget.retained_job_count(), 2);
    }

    #[test]
    fn free_space_refusal_keeps_observed_charge_and_original_reason_after_stop() {
        let mut budget = controller();
        let job = budget.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_eq!(
            settle_parent_store_budget(&mut budget, job, 2048, MINIMUM_FREE_RESERVE - 1, 0),
            Err(BudgetError::Blocked(BlockReason::FreeSpaceReserve))
        );
        let snapshot = budget.snapshot(job).unwrap();
        assert_eq!(snapshot.usage.stored_bytes, 2048);
        assert_eq!(
            snapshot.state,
            JobState::Failed {
                reason: BlockReason::FreeSpaceReserve,
                worker_stopped: true,
            }
        );
    }

    #[test]
    fn numeric_storage_overshoot_is_retained_without_large_allocation() {
        let mut budget = controller();
        let job = budget
            .start(MINIMUM_FREE_RESERVE + PER_REPOSITORY_TEMP_LIMIT + 1)
            .unwrap();
        let observed = PER_REPOSITORY_TEMP_LIMIT + 1;
        assert_eq!(
            settle_parent_store_budget(
                &mut budget,
                job,
                observed,
                MINIMUM_FREE_RESERVE + PER_REPOSITORY_TEMP_LIMIT + 1,
                0,
            ),
            Err(BudgetError::Blocked(BlockReason::RepositoryStorageLimit))
        );
        let snapshot = budget.snapshot(job).unwrap();
        assert_eq!(snapshot.usage.stored_bytes, observed);
        assert_eq!(
            snapshot.state,
            JobState::Failed {
                reason: BlockReason::RepositoryStorageLimit,
                worker_stopped: true,
            }
        );
    }

    #[test]
    fn unknown_or_already_charged_running_jobs_refuse_before_second_charge() {
        let mut budget = controller();
        assert_eq!(
            settle_parent_store_budget(
                &mut budget,
                verification_budget::JobId(99),
                1024,
                MINIMUM_FREE_RESERVE + 8192,
                0,
            ),
            Err(BudgetError::UnknownJob(verification_budget::JobId(99)))
        );

        let job = budget.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        budget
            .record_store(job, 17, MINIMUM_FREE_RESERVE + 8192)
            .unwrap();
        assert_eq!(
            settle_parent_store_budget(&mut budget, job, 1024, MINIMUM_FREE_RESERVE + 8192, 0,),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        let snapshot = budget.snapshot(job).unwrap();
        assert_eq!(snapshot.usage.stored_bytes, 17);
        assert_eq!(snapshot.state, JobState::Running);
    }

    #[test]
    fn elapsed_limit_boundary_is_inclusive_and_timeout_keeps_charge_and_stop() {
        use verification_budget::MAX_ATTEMPT_MILLIS;

        let mut budget = controller();
        let job = budget.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_eq!(
            settle_parent_store_budget(
                &mut budget,
                job,
                2048,
                MINIMUM_FREE_RESERVE + 8192,
                MAX_ATTEMPT_MILLIS,
            ),
            Ok(2048)
        );

        let mut budget = controller();
        let job = budget.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_eq!(
            settle_parent_store_budget(
                &mut budget,
                job,
                2048,
                MINIMUM_FREE_RESERVE + 8192,
                MAX_ATTEMPT_MILLIS + 1,
            ),
            Err(BudgetError::Blocked(BlockReason::AttemptTimedOut))
        );
        let snapshot = budget.snapshot(job).unwrap();
        assert_eq!(snapshot.usage.stored_bytes, 2048);
        assert_eq!(
            snapshot.state,
            JobState::Failed {
                reason: BlockReason::AttemptTimedOut,
                worker_stopped: true,
            }
        );
        let retained = snapshot.clone();
        assert_eq!(
            settle_parent_store_budget(&mut budget, job, 2048, MINIMUM_FREE_RESERVE + 8192, 0,),
            Err(BudgetError::Blocked(BlockReason::JobNotRunning))
        );
        assert_eq!(budget.snapshot(job).unwrap(), &retained);
    }

    #[test]
    fn storage_refusal_remains_first_when_elapsed_limit_is_also_exceeded() {
        use verification_budget::MAX_ATTEMPT_MILLIS;

        let mut budget = controller();
        let job = budget
            .start(MINIMUM_FREE_RESERVE + PER_REPOSITORY_TEMP_LIMIT + 1)
            .unwrap();
        let observed = PER_REPOSITORY_TEMP_LIMIT + 1;
        assert_eq!(
            settle_parent_store_budget(
                &mut budget,
                job,
                observed,
                MINIMUM_FREE_RESERVE + PER_REPOSITORY_TEMP_LIMIT + 1,
                MAX_ATTEMPT_MILLIS + 1,
            ),
            Err(BudgetError::Blocked(BlockReason::RepositoryStorageLimit))
        );
        let snapshot = budget.snapshot(job).unwrap();
        assert_eq!(snapshot.usage.stored_bytes, observed);
        assert_eq!(
            snapshot.state,
            JobState::Failed {
                reason: BlockReason::RepositoryStorageLimit,
                worker_stopped: true,
            }
        );
    }
}

#[cfg(test)]
mod abandon_record_tests {
    use super::parse_abandoned_record;
    const VALID: &[u8] = b"v1\na1-b2-c3\n1:2\n3:4\n5:6\n7\nend\n";
    #[test]
    fn accepts_closed_record_shape() {
        assert_eq!(parse_abandoned_record(VALID).unwrap().bound_pid, 7);
    }
    #[test]
    fn rejects_traversal_overflow_oversize_and_future_version() {
        let valid = std::str::from_utf8(VALID).unwrap();
        for (original, replacement) in [
            ("a1-b2-c3", "../x"),
            ("a1-b2-c3", "a--c"),
            ("a1-b2-c3", "A1-b2-c3"),
            ("a1-b2-c3", "a1-b2"),
            ("1:2", "18446744073709551616:2"),
            ("1:2", "+1:2"),
            ("1:2", "01:2"),
            ("\n7\n", "\n4294967296\n"),
            ("\n7\n", "\n0\n"),
            ("v1\n", "v2\n"),
            ("end\n", "end\nextra\n"),
        ] {
            let bad = valid.replace(original, replacement);
            assert!(
                parse_abandoned_record(bad.as_bytes()).is_err(),
                "{replacement}"
            );
        }
        assert!(parse_abandoned_record(&vec![b'a'; 1025]).is_err());
    }
}

#[cfg(test)]
mod bounded_shutdown_tests {
    use super::{bounded_reader_join, bounded_reader_join_until};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn completed_reader_and_panicked_reader_have_distinct_join_results() {
        assert!(bounded_reader_join(thread::spawn(|| {})));
        assert!(!bounded_reader_join(thread::spawn(|| panic!(
            "synthetic reader failure"
        ))));
    }

    #[test]
    fn expired_deadline_and_zero_poll_budget_withhold_join_confirmation() {
        for max_polls in [0, 512] {
            let reader = thread::spawn(|| {});
            let completion_deadline = Instant::now() + Duration::from_secs(1);
            while !reader.is_finished() && Instant::now() < completion_deadline {
                thread::sleep(Duration::from_millis(1));
            }
            assert!(reader.is_finished(), "test reader did not finish");
            let deadline = if max_polls == 0 {
                Instant::now() + Duration::from_secs(1)
            } else {
                Instant::now()
            };
            // Even a known-finished thread cannot bypass the stage deadline
            // or poll budget to produce shutdown evidence.
            assert!(!bounded_reader_join_until(reader, deadline, max_polls));
        }
    }

    #[test]
    fn timed_out_reader_is_detached_but_released_and_observed_finished() {
        let (release, wait) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = Arc::clone(&finished);
        let reader = thread::spawn(move || {
            let _ = wait.recv_timeout(Duration::from_secs(5));
            thread_finished.store(true, Ordering::Release);
        });
        let joined = bounded_reader_join(reader);
        release.send(()).expect("release controlled test reader");
        let deadline = Instant::now() + Duration::from_secs(1);
        while !finished.load(Ordering::Acquire) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(
            finished.load(Ordering::Acquire),
            "test reader did not finish"
        );
        assert!(!joined, "blocked reader unexpectedly joined before release");
    }
}

#[cfg(test)]
mod stop_policy_tests {
    use super::{stop_policy, OUTER_DEADLINE};
    use std::time::Instant;

    #[test]
    fn confirmed_reaped_child_skips_kill_and_confirmation() {
        let mut calls = Vec::new();
        let result = stop_policy(
            &mut calls,
            |calls| {
                calls.push("query");
                Ok(true)
            },
            |calls| {
                calls.push("kill");
                Err("unexpected kill".into())
            },
            |calls, _| {
                calls.push("confirm");
                Err("unexpected confirm".into())
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(calls, ["query"]);
    }

    #[test]
    fn query_failure_still_attempts_kill_and_bounded_confirmation() {
        let mut calls = Vec::new();
        let started = Instant::now();
        let result = stop_policy(
            &mut calls,
            |calls| {
                calls.push("query");
                Err("query failed".into())
            },
            |calls| {
                calls.push("kill");
                Ok(())
            },
            |calls, deadline| {
                calls.push("confirm");
                assert!(deadline >= started + OUTER_DEADLINE);
                assert!(deadline <= Instant::now() + OUTER_DEADLINE);
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(calls, ["query", "kill", "confirm"]);
    }

    #[test]
    fn kill_error_racing_natural_exit_succeeds_only_after_confirmation() {
        let mut calls = Vec::new();
        let result = stop_policy(
            &mut calls,
            |calls| {
                calls.push("query");
                Ok(false)
            },
            |calls| {
                calls.push("kill");
                Err("already exited".into())
            },
            |calls, _| {
                calls.push("confirm");
                Ok(())
            },
        );
        assert_eq!(result, Ok(()));
        assert_eq!(calls, ["query", "kill", "confirm"]);
    }

    #[test]
    fn query_kill_and_confirmation_failures_retain_every_diagnostic() {
        let mut calls = Vec::new();
        let error = stop_policy(
            &mut calls,
            |calls| {
                calls.push("query");
                Err("query failed".into())
            },
            |calls| {
                calls.push("kill");
                Err("kill failed".into())
            },
            |calls, _| {
                calls.push("confirm");
                Err("exit unconfirmed".into())
            },
        )
        .unwrap_err();
        assert_eq!(calls, ["query", "kill", "confirm"]);
        for diagnostic in ["query failed", "kill failed", "exit unconfirmed"] {
            assert!(error.contains(diagnostic), "missing {diagnostic}: {error}");
        }
    }

    #[test]
    fn independent_exit_confirmation_resolves_both_query_and_kill_errors() {
        let mut confirmed = false;
        let result = stop_policy(
            &mut confirmed,
            |_| Err("query unavailable".into()),
            |_| Err("kill raced exit".into()),
            |confirmed, _| {
                *confirmed = true;
                Ok(())
            },
        );
        assert!(confirmed);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn kill_success_without_confirmed_exit_does_not_claim_stop() {
        let error = stop_policy(
            &mut (),
            |_| Ok(false),
            |_| Ok(()),
            |_, _| Err("exit unknown".into()),
        )
        .unwrap_err();
        assert!(error.contains("kill fixed child succeeded"));
        assert!(error.contains("exit unknown"));
    }
}

#[cfg(test)]
mod admitted_attempt_finalizer_tests {
    use super::finalize_admitted_attempt_result;
    use verification_budget::{
        BlockReason, BudgetController, BudgetError, JobState, Limits, MINIMUM_FREE_RESERVE,
    };

    fn budget() -> BudgetController {
        BudgetController::new(Limits { headroom_bytes: 0 })
    }

    #[test]
    fn running_error_becomes_incomplete_without_changing_usage_or_releasing_slot() {
        let mut controller = budget();
        let job = controller.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        controller.record_ingress(job, 37).unwrap();
        controller
            .record_store(job, 2048, MINIMUM_FREE_RESERVE + 8192)
            .unwrap();
        assert_eq!(
            finalize_admitted_attempt_result::<()>(
                &mut controller,
                job,
                Err("late fixture failure".into()),
            ),
            Err("late fixture failure".into())
        );
        let first = controller.snapshot(job).unwrap().clone();
        assert_eq!(first.usage.ingress_bytes, 37);
        assert_eq!(first.usage.stored_bytes, 2048);
        assert_eq!(
            first.state,
            JobState::Failed {
                reason: BlockReason::StoreIncomplete,
                worker_stopped: false,
            }
        );
        assert_eq!(
            controller.start(MINIMUM_FREE_RESERVE + 8192),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        );
        assert_eq!(
            finalize_admitted_attempt_result::<()>(
                &mut controller,
                job,
                Err("replayed fixture failure".into()),
            ),
            Err("replayed fixture failure".into())
        );
        assert_eq!(controller.snapshot(job).unwrap(), &first);
    }

    #[test]
    fn earlier_real_ingress_refusal_keeps_overshoot_and_first_reason() {
        let mut controller = budget();
        let job = controller.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_eq!(
            controller.record_ingress(job, verification_budget::PER_REPOSITORY_INGRESS_LIMIT + 1),
            Err(BudgetError::Blocked(BlockReason::IngressLimit)),
        );
        let before = controller.snapshot(job).unwrap().clone();
        assert_eq!(
            finalize_admitted_attempt_result::<()>(
                &mut controller,
                job,
                Err("subsequent readback refused".into())
            ),
            Err("subsequent readback refused".into()),
        );
        assert_eq!(controller.snapshot(job).unwrap(), &before);
        assert_eq!(
            controller.start(MINIMUM_FREE_RESERVE + 8192),
            Err(BudgetError::Blocked(BlockReason::AlreadyAtConcurrencyLimit))
        );
    }

    #[test]
    fn running_error_before_usage_still_records_incomplete_state() {
        let mut controller = budget();
        let job = controller.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        assert_eq!(
            finalize_admitted_attempt_result::<()>(
                &mut controller,
                job,
                Err("early fixture failure".into()),
            ),
            Err("early fixture failure".into())
        );
        assert_eq!(
            controller.snapshot(job).unwrap().state,
            JobState::Failed {
                reason: BlockReason::StoreIncomplete,
                worker_stopped: false,
            }
        );
        assert_eq!(controller.snapshot(job).unwrap().usage.stored_bytes, 0);
    }

    #[test]
    fn existing_failed_and_unclean_canceled_states_preserve_first_state_and_usage() {
        for state in [
            JobState::Failed {
                reason: BlockReason::FreeSpaceReserve,
                worker_stopped: false,
            },
            JobState::Failed {
                reason: BlockReason::AttemptTimedOut,
                worker_stopped: true,
            },
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: false,
            },
            JobState::Canceled {
                cleanup_confirmed: false,
                worker_stopped: true,
            },
        ] {
            let mut controller = budget();
            let job = controller.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
            controller.record_ingress(job, 11).unwrap();
            controller
                .record_store(job, 19, MINIMUM_FREE_RESERVE + 8192)
                .unwrap();
            match state {
                JobState::Failed { reason, .. } => controller.fail(job, reason).unwrap(),
                JobState::Canceled { .. } => controller.cancel(job).unwrap(),
                _ => unreachable!(),
            }
            if matches!(
                state,
                JobState::Failed {
                    worker_stopped: true,
                    ..
                } | JobState::Canceled {
                    worker_stopped: true,
                    ..
                }
            ) {
                controller.acknowledge_worker_stopped(job).unwrap();
            }
            let before = controller.snapshot(job).unwrap().clone();
            assert_eq!(
                finalize_admitted_attempt_result::<()>(
                    &mut controller,
                    job,
                    Err("later fixture failure".into()),
                ),
                Err("later fixture failure".into())
            );
            assert_eq!(controller.snapshot(job).unwrap(), &before);
        }
    }

    #[test]
    fn unknown_completed_and_cleanup_confirmed_canceled_append_diagnostic_without_mutation() {
        let mut controller = budget();
        let missing = verification_budget::JobId(99);
        let error = finalize_admitted_attempt_result::<()>(
            &mut controller,
            missing,
            Err("original unknown error".into()),
        )
        .unwrap_err();
        assert!(error.contains("original unknown error"));
        assert!(error.contains("could not establish StoreIncomplete"));
        assert_eq!(controller.retained_job_count(), 0);

        for initial in [
            JobState::Completed {
                cleanup_confirmed: false,
            },
            JobState::Completed {
                cleanup_confirmed: true,
            },
            JobState::Canceled {
                cleanup_confirmed: true,
                worker_stopped: true,
            },
        ] {
            let mut controller = budget();
            let job = controller.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
            match initial {
                JobState::Completed { cleanup_confirmed } => {
                    controller.complete(job).unwrap();
                    if cleanup_confirmed {
                        controller.confirm_cleanup(job).unwrap();
                    }
                }
                JobState::Canceled { .. } => {
                    controller.cancel(job).unwrap();
                    controller.acknowledge_worker_stopped(job).unwrap();
                    controller.confirm_cleanup(job).unwrap();
                }
                _ => unreachable!(),
            }
            let before = controller.snapshot(job).unwrap().clone();
            let error = finalize_admitted_attempt_result::<()>(
                &mut controller,
                job,
                Err("original terminal error".into()),
            )
            .unwrap_err();
            assert!(error.contains("original terminal error"));
            assert!(error.contains("could not establish StoreIncomplete"));
            assert_eq!(controller.snapshot(job).unwrap(), &before);
        }
    }

    #[test]
    fn success_is_returned_unchanged() {
        let mut controller = budget();
        let job = controller.start(MINIMUM_FREE_RESERVE + 8192).unwrap();
        controller.record_ingress(job, 5).unwrap();
        controller
            .record_store(job, 7, MINIMUM_FREE_RESERVE + 8192)
            .unwrap();
        let before = controller.snapshot(job).unwrap().clone();
        assert_eq!(
            finalize_admitted_attempt_result(&mut controller, job, Ok(23)),
            Ok(23)
        );
        assert_eq!(controller.snapshot(job).unwrap(), &before);
    }
}
