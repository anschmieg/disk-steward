//! Portable, side-effect-free domain contracts for Disk Steward.
//!
//! This crate deliberately contains no filesystem or process operations. It provides
//! opaque identifiers and revision-bound scope/approval values for backend services.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// A typed identifier whose representation is not exposed to callers.
///
/// IDs are opaque within the process and suitable for domain references. Durable
/// persistence and cross-process serialization are intentionally left to a future
/// storage layer.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpaqueId(u128);

impl OpaqueId {
    /// Creates a new process-local opaque identifier.
    #[must_use]
    pub fn new() -> Self {
        let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let epoch = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        Self((epoch << 64) ^ u128::from(sequence))
    }
}

impl Default for OpaqueId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for OpaqueId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OpaqueId([redacted])")
    }
}

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(OpaqueId);

        impl $name {
            #[must_use]
            pub fn new() -> Self {
                Self(OpaqueId::new())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([redacted])"))
            }
        }
    };
}

typed_id!(ScopeId);
typed_id!(PlanId);
typed_id!(RevisionId);
typed_id!(ActionId);
typed_id!(ApprovalId);
typed_id!(EvidenceId);

/// Identifies the approved plan state without carrying paths into the renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeSnapshot {
    id: ScopeId,
    generation: u64,
    access: ScopeAccess,
}

impl ScopeSnapshot {
    #[must_use]
    pub const fn new(id: ScopeId, generation: u64, access: ScopeAccess) -> Self {
        Self {
            id,
            generation,
            access,
        }
    }

    #[must_use]
    pub const fn id(&self) -> ScopeId {
        self.id
    }

    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub const fn access(&self) -> ScopeAccess {
        self.access
    }
}

/// Backend-owned access state for one explicitly selected scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeAccess {
    Granted,
    Denied,
    Revoked,
    Unsupported,
}

/// Digest wrapper for canonical backend-generated content.
///
/// The core does not choose a hash algorithm or accept renderer-provided paths.
/// Callers must compute this value from a canonical manifest representation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Digest([u8; 32]);

impl Digest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One typed operation included in an approved plan manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionBinding {
    id: ActionId,
    scope: ScopeId,
    operation: SupportedOperation,
    target: Digest,
    preconditions: Digest,
}

impl ActionBinding {
    #[must_use]
    pub const fn new(
        id: ActionId,
        scope: ScopeId,
        operation: SupportedOperation,
        target: Digest,
        preconditions: Digest,
    ) -> Self {
        Self {
            id,
            scope,
            operation,
            target,
            preconditions,
        }
    }

    #[must_use]
    pub const fn id(&self) -> ActionId {
        self.id
    }

    #[must_use]
    pub const fn scope(&self) -> ScopeId {
        self.scope
    }

    #[must_use]
    pub const fn operation(&self) -> SupportedOperation {
        self.operation
    }

    #[must_use]
    pub const fn target_digest(&self) -> Digest {
        self.target
    }

    #[must_use]
    pub const fn preconditions_digest(&self) -> Digest {
        self.preconditions
    }
}

/// A deliberately closed operation set. There is no arbitrary path or shell action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SupportedOperation {
    StageToRecovery,
    RestoreFromRecovery,
    PermanentlyRemoveStaged,
}

/// Input bundle for constructing an immutable plan snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanSnapshotParts {
    pub plan: PlanId,
    pub revision: RevisionId,
    pub revision_number: u64,
    pub manifest: Digest,
    pub preconditions: Digest,
    pub scopes: Vec<ScopeSnapshot>,
    pub adapter_versions: Vec<(String, String)>,
    pub actions: Vec<ActionBinding>,
}

/// Immutable set of backend-validated facts covered by approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanSnapshot {
    plan: PlanId,
    revision: RevisionId,
    revision_number: u64,
    manifest: Digest,
    preconditions: Digest,
    scopes: Vec<ScopeSnapshot>,
    adapter_versions: Vec<(String, String)>,
    actions: Vec<ActionBinding>,
}

impl PlanSnapshot {
    #[must_use]
    pub fn new(parts: PlanSnapshotParts) -> Self {
        Self {
            plan: parts.plan,
            revision: parts.revision,
            revision_number: parts.revision_number,
            manifest: parts.manifest,
            preconditions: parts.preconditions,
            scopes: parts.scopes,
            adapter_versions: parts.adapter_versions,
            actions: parts.actions,
        }
    }

    #[must_use]
    pub const fn plan_id(&self) -> PlanId {
        self.plan
    }

    #[must_use]
    pub const fn revision_id(&self) -> RevisionId {
        self.revision
    }

    #[must_use]
    pub const fn revision_number(&self) -> u64 {
        self.revision_number
    }

    #[must_use]
    pub fn actions(&self) -> &[ActionBinding] {
        &self.actions
    }
}

/// Short-lived backend-issued authorization bound to one exact plan snapshot.
///
/// This contract is cloneable and validation does not consume it. It is not yet a
/// replay-safe executor capability: a future execution service must atomically
/// consume/record approval use in its durable ledger before permitting mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Approval {
    id: ApprovalId,
    plan: PlanId,
    revision: RevisionId,
    revision_number: u64,
    manifest: Digest,
    preconditions: Digest,
    scopes: Vec<ScopeSnapshot>,
    adapter_versions: Vec<(String, String)>,
    actions: Vec<ActionBinding>,
    issued_at: u64,
    expires_at: u64,
}

impl Approval {
    /// Issues a capability for the exact snapshot shown to and accepted by the user.
    ///
    /// This constructor is a domain contract only; the application service must ensure
    /// that issuance follows an explicit native confirmation and policy validation.
    #[must_use]
    pub fn issue(snapshot: &PlanSnapshot, issued_at: u64, expires_at: u64) -> Option<Self> {
        if expires_at <= issued_at || snapshot.actions.is_empty() {
            return None;
        }
        if snapshot
            .scopes
            .iter()
            .any(|scope| scope.access != ScopeAccess::Granted)
        {
            return None;
        }
        if snapshot
            .actions
            .iter()
            .any(|action| !snapshot.scopes.iter().any(|scope| scope.id == action.scope))
        {
            return None;
        }
        Some(Self {
            id: ApprovalId::new(),
            plan: snapshot.plan,
            revision: snapshot.revision,
            revision_number: snapshot.revision_number,
            manifest: snapshot.manifest,
            preconditions: snapshot.preconditions,
            scopes: snapshot.scopes.clone(),
            adapter_versions: snapshot.adapter_versions.clone(),
            actions: snapshot.actions.clone(),
            issued_at,
            expires_at,
        })
    }

    #[must_use]
    pub const fn id(&self) -> ApprovalId {
        self.id
    }

    /// Checks that an approval is current for the exact current backend snapshot.
    pub fn validate(&self, current: &PlanSnapshot, now: u64) -> Result<(), ApprovalError> {
        if now < self.issued_at || now >= self.expires_at {
            return Err(ApprovalError::Expired);
        }
        if current
            .scopes
            .iter()
            .any(|scope| scope.access != ScopeAccess::Granted)
        {
            return Err(ApprovalError::ScopeUnavailable);
        }
        if self.plan != current.plan
            || self.revision != current.revision
            || self.revision_number != current.revision_number
            || self.manifest != current.manifest
            || self.preconditions != current.preconditions
            || self.scopes != current.scopes
            || self.adapter_versions != current.adapter_versions
            || self.actions != current.actions
        {
            return Err(ApprovalError::Stale);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalError {
    Expired,
    Stale,
    ScopeUnavailable,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> Digest {
        Digest::from_bytes([byte; 32])
    }

    fn snapshot() -> PlanSnapshot {
        let scope = ScopeId::new();
        PlanSnapshot::new(PlanSnapshotParts {
            plan: PlanId::new(),
            revision: RevisionId::new(),
            revision_number: 4,
            manifest: digest(1),
            preconditions: digest(2),
            scopes: vec![ScopeSnapshot::new(scope, 7, ScopeAccess::Granted)],
            adapter_versions: vec![("recovery".into(), "0.0.1".into())],
            actions: vec![ActionBinding::new(
                ActionId::new(),
                scope,
                SupportedOperation::StageToRecovery,
                digest(3),
                digest(4),
            )],
        })
    }

    #[test]
    fn opaque_ids_are_typed_unique_and_redacted_in_debug() {
        let first = ScopeId::new();
        let second = ScopeId::new();
        assert_ne!(first, second);
        assert_eq!(format!("{first:?}"), "ScopeId([redacted])");
        assert_eq!(format!("{:?}", OpaqueId::new()), "OpaqueId([redacted])");
    }

    #[test]
    fn action_binding_exposes_only_its_typed_approved_fields() {
        let scope = ScopeId::new();
        let target = digest(3);
        let preconditions = digest(4);
        let action = ActionBinding::new(
            ActionId::new(),
            scope,
            SupportedOperation::StageToRecovery,
            target,
            preconditions,
        );

        assert_eq!(action.scope(), scope);
        assert_eq!(action.operation(), SupportedOperation::StageToRecovery);
        assert_eq!(action.target_digest(), target);
        assert_eq!(action.preconditions_digest(), preconditions);
        assert_eq!(target.as_bytes(), &[3; 32]);
    }

    #[test]
    fn approval_is_bound_to_exact_snapshot_and_valid_time_window() {
        let approved = snapshot();
        let approval = Approval::issue(&approved, 100, 200).expect("valid approval");
        assert_eq!(approval.validate(&approved, 100), Ok(()));
        assert_eq!(approval.validate(&approved, 199), Ok(()));
        assert_eq!(
            approval.validate(&approved, 200),
            Err(ApprovalError::Expired)
        );
        assert_eq!(
            approval.validate(&approved, 99),
            Err(ApprovalError::Expired)
        );

        let mut edited = approved.clone();
        edited.revision_number += 1;
        assert_eq!(approval.validate(&edited, 150), Err(ApprovalError::Stale));
    }

    #[test]
    fn manifest_scope_adapter_and_action_changes_invalidate_approval() {
        let approved = snapshot();
        let approval = Approval::issue(&approved, 100, 200).expect("valid approval");

        let mut changed_manifest = approved.clone();
        changed_manifest.manifest = digest(8);
        assert_eq!(
            approval.validate(&changed_manifest, 150),
            Err(ApprovalError::Stale)
        );

        let mut changed_scope = approved.clone();
        changed_scope.scopes[0].generation += 1;
        assert_eq!(
            approval.validate(&changed_scope, 150),
            Err(ApprovalError::Stale)
        );

        let mut revoked = approved.clone();
        revoked.scopes[0].access = ScopeAccess::Revoked;
        assert_eq!(
            approval.validate(&revoked, 150),
            Err(ApprovalError::ScopeUnavailable)
        );

        let mut changed_adapter = approved.clone();
        changed_adapter.adapter_versions[0].1 = "0.0.2".into();
        assert_eq!(
            approval.validate(&changed_adapter, 150),
            Err(ApprovalError::Stale)
        );

        let mut changed_actions = approved.clone();
        changed_actions.actions.clear();
        assert_eq!(
            approval.validate(&changed_actions, 150),
            Err(ApprovalError::Stale)
        );
    }

    #[test]
    fn approval_cannot_be_issued_for_empty_or_unavailable_scope() {
        let mut no_actions = snapshot();
        no_actions.actions.clear();
        assert!(Approval::issue(&no_actions, 100, 200).is_none());

        let mut denied = snapshot();
        denied.scopes[0].access = ScopeAccess::Denied;
        assert!(Approval::issue(&denied, 100, 200).is_none());

        assert!(Approval::issue(&snapshot(), 200, 200).is_none());
    }
}
