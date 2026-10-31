//! Portable domain contracts for Disk Steward.
//!
//! This crate contains no filesystem traversal or process execution. ID creation
//! uses operating-system randomness. The remaining contracts bind opaque references
//! and exact scope/approval revisions for backend services.

use std::collections::HashSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// A typed identifier whose representation is not exposed to callers.
///
/// IDs are opaque domain references, encoded durably as canonical UUID v4 strings.
/// Possessing an ID does not grant authority to access or mutate anything.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpaqueId(Uuid);

impl OpaqueId {
    /// Creates a random UUID v4 using the operating system's random number source.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    fn parse_canonical(value: &str) -> Result<Self, &'static str> {
        let uuid = Uuid::parse_str(value).map_err(|_| "invalid UUID")?;
        if uuid.is_nil()
            || uuid.get_version_num() != 4
            || uuid.get_variant() != uuid::Variant::RFC4122
        {
            return Err("identifier must be a non-nil UUID v4");
        }
        if uuid.hyphenated().to_string() != value {
            return Err("identifier must use canonical lowercase UUID spelling");
        }
        Ok(Self(uuid))
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
    ($name:ident, $kind:literal) => {
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

        impl FromStr for $name {
            type Err = &'static str;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let prefix = concat!("disk-steward:", $kind, ":v1:");
                let uuid = value.strip_prefix(prefix).ok_or("wrong typed ID schema")?;
                OpaqueId::parse_canonical(uuid).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let value = format!("disk-steward:{}:v1:{}", $kind, self.0 .0.hyphenated());
                serializer.serialize_str(&value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                value.parse().map_err(serde::de::Error::custom)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([redacted])"))
            }
        }
    };
}

typed_id!(ScopeId, "scope");
typed_id!(PlanId, "plan");
typed_id!(RevisionId, "revision");
typed_id!(ReviewId, "review");
typed_id!(SessionId, "session");
typed_id!(ReceiptId, "receipt");
typed_id!(ExecutionId, "execution");
typed_id!(AttemptId, "attempt");
typed_id!(ActionId, "action");
typed_id!(ApprovalId, "approval");
typed_id!(EvidenceId, "evidence");

/// Host platform identifiers used to describe adapter coverage. These values do
/// not imply that an adapter is installed, available, or validated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HostPlatform {
    MacOS,
    Windows,
    Linux,
    Other,
}

/// Portable platform functions that an adapter may report status for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PlatformCapability {
    ScopeAccess,
    FileIdentity,
    RecoverableStage,
    Restore,
    PermanentRemoval,
    SecureCredentialStore,
    NativeConfirmation,
}

/// Why a platform capability cannot currently be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityStatus {
    /// A concrete adapter reports the capability available. This is not by itself
    /// proof that a destructive operation passed its required validation gate.
    Available { adapter_version: String },
    /// The capability is supported in principle, but a runtime dependency or
    /// condition is missing (for example, permission has not been granted).
    Unavailable { reason: String },
    /// This adapter/platform combination does not implement the capability.
    Unsupported { reason: String },
}

/// A platform adapter's reported status for one capability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilityReport {
    pub platform: HostPlatform,
    pub capability: PlatformCapability,
    pub status: CapabilityStatus,
}

impl CapabilityReport {
    #[must_use]
    pub const fn new(
        platform: HostPlatform,
        capability: PlatformCapability,
        status: CapabilityStatus,
    ) -> Self {
        Self {
            platform,
            capability,
            status,
        }
    }

    /// Returns true only for an adapter's explicit availability report.
    /// Callers must still apply operation-specific validation and approval gates.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self.status, CapabilityStatus::Available { .. })
    }
}

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
        // Ambiguous identities or adapter versions must never become an
        // approval binding. Semantic eligibility still belongs to policy.
        let mut action_ids = HashSet::new();
        let mut scope_ids = HashSet::new();
        let mut adapter_names = HashSet::new();
        if snapshot
            .actions
            .iter()
            .any(|action| !action_ids.insert(action.id))
            || snapshot
                .scopes
                .iter()
                .any(|scope| !scope_ids.insert(scope.id))
            || snapshot.adapter_versions.is_empty()
            || snapshot.adapter_versions.iter().any(|(name, version)| {
                !adapter_names.insert(name.as_str())
                    || [name, version].iter().any(|value| {
                        value.is_empty()
                            || value.trim() != value.as_str()
                            || value.chars().any(char::is_control)
                    })
            })
        {
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
    fn typed_ids_round_trip_through_persisted_bytes() {
        let original = PlanId::new();
        let persisted = serde_json::to_vec(&original).expect("serialize typed ID");
        // Parsing is stateless, so this models a fresh process loading persisted bytes.
        let restored: PlanId = serde_json::from_slice(&persisted).expect("load typed ID");
        assert_eq!(restored, original);
        assert_eq!(
            std::str::from_utf8(&persisted).expect("UTF-8 UUID"),
            format!("\"disk-steward:plan:v1:{}\"", original.0 .0.hyphenated())
        );
        assert_eq!(format!("{restored:?}"), "PlanId([redacted])");
        assert!(serde_json::from_slice::<ScopeId>(&persisted).is_err());
    }

    #[test]
    fn typed_id_parser_rejects_malformed_nil_wrong_version_and_noncanonical_values() {
        for invalid in [
            "not-a-uuid",
            "00000000-0000-0000-0000-000000000000",
            "00000000-0000-1000-8000-000000000000",
            "550E8400-E29B-41D4-A716-446655440000",
            "550e8400-e29b-41d4-1716-446655440000",
        ] {
            let encoded = format!("disk-steward:scope:v1:{invalid}");
            assert!(encoded.parse::<ScopeId>().is_err(), "accepted {invalid}");
            assert!(serde_json::from_str::<ScopeId>(&format!("\"{encoded}\"")).is_err());
        }
        assert!(serde_json::from_str::<ScopeId>("42").is_err());
        assert!("disk-steward:scope:v2:550e8400-e29b-41d4-a716-446655440000"
            .parse::<ScopeId>()
            .is_err());
    }

    #[test]
    fn new_typed_ids_are_non_nil_uuid_v4() {
        let id = ScopeId::new();
        let persisted = serde_json::to_string(&id).expect("serialize typed ID");
        let text = persisted
            .trim_matches('"')
            .strip_prefix("disk-steward:scope:v1:")
            .expect("typed ID schema");
        let uuid = Uuid::parse_str(text).expect("generated UUID");
        assert!(!uuid.is_nil());
        assert_eq!(uuid.get_version_num(), 4);
    }

    #[test]
    fn platform_status_keeps_unavailable_and_unsupported_distinct() {
        let permission_missing = CapabilityReport::new(
            HostPlatform::MacOS,
            PlatformCapability::ScopeAccess,
            CapabilityStatus::Unavailable {
                reason: "user permission not granted".into(),
            },
        );
        let no_adapter = CapabilityReport::new(
            HostPlatform::Windows,
            PlatformCapability::RecoverableStage,
            CapabilityStatus::Unsupported {
                reason: "no validated staging adapter".into(),
            },
        );

        assert!(!permission_missing.is_available());
        assert!(!no_adapter.is_available());
        assert_ne!(permission_missing.status, no_adapter.status);
    }

    #[test]
    fn reported_availability_does_not_imply_mutation_authority() {
        let report = CapabilityReport::new(
            HostPlatform::MacOS,
            PlatformCapability::RecoverableStage,
            CapabilityStatus::Available {
                adapter_version: "fixture-adapter/0.0.1".into(),
            },
        );

        assert!(report.is_available());
        // The report has no operation executor, path, target, or approval API.
        assert_eq!(report.capability, PlatformCapability::RecoverableStage);
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
    #[test]
    fn ambiguous_manifest_identifiers_and_adapter_bindings_cannot_be_approved() {
        let mut duplicate_action = snapshot();
        duplicate_action
            .actions
            .push(duplicate_action.actions[0].clone());
        assert!(Approval::issue(&duplicate_action, 10, 20).is_none());
        let mut duplicate_scope = snapshot();
        duplicate_scope
            .scopes
            .push(duplicate_scope.scopes[0].clone());
        assert!(Approval::issue(&duplicate_scope, 10, 20).is_none());
        let mut missing_adapter = snapshot();
        missing_adapter.adapter_versions.clear();
        assert!(Approval::issue(&missing_adapter, 10, 20).is_none());
        let mut conflicting_adapter = snapshot();
        conflicting_adapter
            .adapter_versions
            .push(("recovery".into(), "different".into()));
        assert!(Approval::issue(&conflicting_adapter, 10, 20).is_none());
        let mut blank_adapter = snapshot();
        blank_adapter.adapter_versions[0].1 = " ".into();
        assert!(Approval::issue(&blank_adapter, 10, 20).is_none());
    }
}
